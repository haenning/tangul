//! SSH tunnels for database connections: one SSH session per tunnel, a local
//! `127.0.0.1` listener, and one `direct-tcpip` channel per accepted connection, so
//! the drivers connect to a plain local port and stay unchanged.
//!
//! The session is opened lazily and re-opened when it died, on the next connection.
//! Server keys are checked against `~/.ssh/known_hosts` and never accepted silently:
//! unknown keys surface as [`HostKeyError::Unknown`] for the user to confirm.

use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use rusdb_core::{SshAuth, SshTarget};
use russh::client::{self, Handle};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// The server's host key did not verify. `Unknown` can be trusted by the user
/// ([`trust_host_key`]); `Changed` must not be (possible man-in-the-middle).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyError {
    Unknown {
        host: String,
        port: u16,
        /// `SHA256:…`, as `ssh` prints it.
        fingerprint: String,
        /// The key in OpenSSH format, to write to known_hosts once trusted.
        key: String,
    },
    Changed {
        host: String,
        port: u16,
        fingerprint: String,
        /// Line in known_hosts holding the old key.
        line: usize,
    },
}

impl std::fmt::Display for HostKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostKeyError::Unknown {
                host, fingerprint, ..
            } => write!(f, "unknown SSH host key for {host} ({fingerprint})"),
            HostKeyError::Changed {
                host,
                fingerprint,
                line,
                ..
            } => write!(
                f,
                "SSH host key for {host} CHANGED ({fingerprint}); it does not match \
                 ~/.ssh/known_hosts line {line}. This could be a man-in-the-middle attack. \
                 If the server was reinstalled, remove that line and connect again."
            ),
        }
    }
}

impl std::error::Error for HostKeyError {}

/// `~/.ssh/known_hosts`, or `RUSDB_KNOWN_HOSTS` (tests use a scratch file).
fn known_hosts_path() -> Option<PathBuf> {
    std::env::var_os("RUSDB_KNOWN_HOSTS").map(PathBuf::from)
}

/// Adds a host key the user confirmed to known_hosts.
pub fn trust_host_key(host: &str, port: u16, key: &str) -> Result<()> {
    let key = PublicKey::from_openssh(key).context("invalid host key")?;
    match known_hosts_path() {
        Some(path) => russh::keys::known_hosts::learn_known_hosts_path(host, port, &key, path)?,
        None => russh::keys::known_hosts::learn_known_hosts(host, port, &key)?,
    }
    Ok(())
}

pub struct Tunnel {
    target: SshTarget,
    /// Where the SSH server should connect to (the database, as it sees it).
    remote_host: String,
    remote_port: u16,
    active: Mutex<Option<Active>>,
}

struct Active {
    session: Arc<Handle<Client>>,
    local_port: u16,
    accept: JoinHandle<()>,
}

impl Drop for Active {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

impl Tunnel {
    pub fn new(target: SshTarget, remote_host: String, remote_port: u16) -> Self {
        Self {
            target,
            remote_host,
            remote_port,
            active: Mutex::new(None),
        }
    }

    /// Local port that forwards to the database; opens (or re-opens) the SSH
    /// session first if needed.
    pub async fn local_port(&self) -> Result<u16> {
        let mut active = self.active.lock().await;
        if let Some(a) = active.as_ref()
            && !a.session.is_closed()
        {
            return Ok(a.local_port);
        }
        *active = None;
        let session = Arc::new(connect(&self.target).await?);
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let local_port = listener.local_addr()?.port();
        let accept = tokio::spawn(accept_loop(
            listener,
            session.clone(),
            self.remote_host.clone(),
            self.remote_port,
        ));
        *active = Some(Active {
            session,
            local_port,
            accept,
        });
        Ok(local_port)
    }
}

async fn accept_loop(listener: TcpListener, session: Arc<Handle<Client>>, host: String, port: u16) {
    loop {
        let Ok((socket, peer)) = listener.accept().await else {
            return;
        };
        if session.is_closed() {
            // Dropping the socket fails this DB connection; the pool's next attempt
            // asks for the port again, which reconnects.
            return;
        }
        let session = session.clone();
        let host = host.clone();
        tokio::spawn(async move {
            if let Err(e) = forward(socket, peer.port(), &session, &host, port).await {
                eprintln!("ssh tunnel: {e:#}");
            }
        });
    }
}

async fn forward(
    mut socket: TcpStream,
    local_port: u16,
    session: &Handle<Client>,
    host: &str,
    port: u16,
) -> Result<()> {
    let channel = session
        .channel_open_direct_tcpip(host, u32::from(port), "127.0.0.1", u32::from(local_port))
        .await
        .with_context(|| format!("could not reach {host}:{port} through the tunnel"))?;
    let mut stream = channel.into_stream();
    // Either side closing ends the forward; errors here are ordinary disconnects.
    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
    Ok(())
}

struct Client {
    host: String,
    port: u16,
    /// Why the key was rejected, read after `connect` failed.
    rejected: Arc<StdMutex<Option<HostKeyError>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            // Host certificates would need a CA from known_hosts (@cert-authority).
            return Ok(false);
        };
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        let known = match known_hosts_path() {
            Some(path) => russh::keys::check_known_hosts_path(&self.host, self.port, key, path),
            None => russh::keys::check_known_hosts(&self.host, self.port, key),
        };
        let rejection = match known {
            Ok(true) => return Ok(true),
            Ok(false) => HostKeyError::Unknown {
                host: self.host.clone(),
                port: self.port,
                fingerprint,
                key: key.to_openssh().unwrap_or_default(),
            },
            Err(russh::keys::Error::KeyChanged { line }) => HostKeyError::Changed {
                host: self.host.clone(),
                port: self.port,
                fingerprint,
                line,
            },
            Err(e) => {
                eprintln!("known_hosts: {e}");
                return Ok(false);
            }
        };
        *self.rejected.lock().unwrap() = Some(rejection);
        Ok(false)
    }
}

async fn connect(target: &SshTarget) -> Result<Handle<Client>> {
    let cfg = &target.config;
    let config = Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        nodelay: true,
        ..Default::default()
    });
    let rejected = Arc::new(StdMutex::new(None));
    let handler = Client {
        host: cfg.host.clone(),
        port: cfg.port,
        rejected: rejected.clone(),
    };
    let connecting = client::connect(config, (cfg.host.as_str(), cfg.port), handler);
    let mut session = match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
        Err(_) => bail!("SSH connection to {}:{} timed out", cfg.host, cfg.port),
        Ok(Err(e)) => {
            if let Some(host_key) = rejected.lock().unwrap().take() {
                return Err(host_key.into());
            }
            return Err(anyhow!(e).context(format!(
                "SSH connection to {}:{} failed",
                cfg.host, cfg.port
            )));
        }
        Ok(Ok(session)) => session,
    };
    authenticate(&mut session, target).await?;
    Ok(session)
}

async fn authenticate(session: &mut Handle<Client>, target: &SshTarget) -> Result<()> {
    let cfg = &target.config;
    let user = cfg.user.as_str();
    let rsa_hash = || async {
        session
            .best_supported_rsa_hash()
            .await
            .ok()
            .flatten()
            .flatten()
    };
    let ok = match cfg.auth {
        SshAuth::Password => {
            let password = target.secret.clone().unwrap_or_default();
            session
                .authenticate_password(user, password)
                .await?
                .success()
        }
        SshAuth::Key => {
            let path = expand_home(&cfg.key_path);
            let secret = target.secret.as_deref().filter(|s| !s.is_empty());
            let key = russh::keys::load_secret_key(&path, secret).map_err(|e| match e {
                russh::keys::Error::KeyIsEncrypted => {
                    anyhow!(
                        "SSH key {} is encrypted: enter its passphrase",
                        path.display()
                    )
                }
                russh::keys::Error::IO(e) => {
                    anyhow!("could not read SSH key {}: {e}", path.display())
                }
                _ if secret.is_some() => anyhow!(
                    "could not decrypt SSH key {} (wrong passphrase?)",
                    path.display()
                ),
                e => anyhow!("could not load SSH key {}: {e}", path.display()),
            })?;
            let hash = rsa_hash().await;
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
            session.authenticate_publickey(user, key).await?.success()
        }
        SshAuth::Agent => {
            let mut agent = AgentClient::connect_env()
                .await
                .context("no ssh-agent (SSH_AUTH_SOCK) available")?;
            let identities = agent.request_identities().await?;
            if identities.is_empty() {
                bail!("ssh-agent has no keys (add one with ssh-add)");
            }
            let hash = rsa_hash().await;
            let mut ok = false;
            for identity in identities {
                let AgentIdentity::PublicKey { key, .. } = identity else {
                    continue;
                };
                let result = session
                    .authenticate_publickey_with(user, key, hash, &mut agent)
                    .await
                    .map_err(|e| anyhow!("{e:?}"))?;
                if result.success() {
                    ok = true;
                    break;
                }
            }
            ok
        }
    };
    if !ok {
        bail!("SSH authentication as {user}@{} failed", cfg.host);
    }
    Ok(())
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::home_dir().map_or_else(|| PathBuf::from(path), |h| h.join(rest)),
        None => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_home() {
        let home = std::env::home_dir().unwrap();
        assert_eq!(
            expand_home("~/.ssh/id_ed25519"),
            home.join(".ssh/id_ed25519")
        );
        assert_eq!(expand_home("/k"), PathBuf::from("/k"));
    }

    #[test]
    fn host_key_messages() {
        let e = HostKeyError::Changed {
            host: "h".into(),
            port: 22,
            fingerprint: "SHA256:x".into(),
            line: 7,
        };
        assert!(e.to_string().contains("line 7"));
    }
}
