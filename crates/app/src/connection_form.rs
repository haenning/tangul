//! Form for creating/editing a database and its users. Rendered inside a dialog.

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_core::{Database, DbUser, DriverKind, SshAuth, SshConfig, TlsMode};

use crate::store::new_id;

#[derive(Clone, Copy, PartialEq)]
pub enum FormMode {
    /// Connection settings plus a first user.
    NewDatabase,
    /// Connection settings only; the users are kept.
    EditDatabase,
    /// User and password for an existing database.
    User,
}

impl FormMode {
    fn has_db_fields(self) -> bool {
        self != FormMode::User
    }

    fn has_user_fields(self) -> bool {
        self != FormMode::EditDatabase
    }
}

pub struct FormOutput {
    pub database: Database,
    /// The user to add or update, with a new password (`None` keeps the stored one).
    pub user: Option<(DbUser, Option<String>)>,
    /// New SSH password or key passphrase (`None` keeps the stored one).
    pub ssh_secret: Option<String>,
}

pub struct ConnectionForm {
    mode: FormMode,
    /// The database being edited or extended; `None` for [`FormMode::NewDatabase`].
    base: Option<Database>,
    user_id: Option<String>,
    driver: DriverKind,
    tls: TlsMode,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    database: Entity<InputState>,
    ssh: bool,
    ssh_auth: SshAuth,
    ssh_host: Entity<InputState>,
    ssh_port: Entity<InputState>,
    ssh_user: Entity<InputState>,
    ssh_key_path: Entity<InputState>,
    ssh_secret: Entity<InputState>,
    error: Option<SharedString>,
}

impl ConnectionForm {
    /// `user` is the user to edit in [`FormMode::User`] (`None` adds one). The password
    /// field stays empty when editing; empty means "keep the stored password".
    pub fn new(
        mode: FormMode,
        db: Option<&Database>,
        user: Option<&DbUser>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let driver = db.map_or(DriverKind::Postgres, |p| p.driver);
        let ssh = db.and_then(|d| d.ssh.as_ref());
        let input = |value: String,
                     placeholder: &'static str,
                     window: &mut Window,
                     cx: &mut Context<Self>| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        Self {
            mode,
            base: db.cloned(),
            user_id: user.map(|u| u.id.clone()),
            driver,
            tls: db.map_or(TlsMode::Prefer, |p| p.tls),
            name: input(
                db.map(|p| p.name.clone()).unwrap_or_default(),
                "optional",
                window,
                cx,
            ),
            host: input(
                db.map_or("localhost".into(), |p| p.host.clone()),
                "localhost",
                window,
                cx,
            ),
            port: input(
                db.map_or(driver.default_port(), |p| p.port).to_string(),
                "port",
                window,
                cx,
            ),
            user: input(
                user.map(|u| u.user.clone()).unwrap_or_default(),
                "user",
                window,
                cx,
            ),
            password: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder(if user.is_some() {
                        "unchanged"
                    } else {
                        "password"
                    })
            }),
            database: input(
                db.map(|p| p.database.clone()).unwrap_or_default(),
                "optional",
                window,
                cx,
            ),
            ssh: ssh.is_some(),
            ssh_auth: ssh.map(|s| s.auth).unwrap_or_default(),
            ssh_host: input(
                ssh.map(|s| s.host.clone()).unwrap_or_default(),
                "bastion.example.com",
                window,
                cx,
            ),
            ssh_port: input(ssh.map_or(22, |s| s.port).to_string(), "22", window, cx),
            ssh_user: input(
                ssh.map_or_else(
                    || std::env::var("USER").unwrap_or_default(),
                    |s| s.user.clone(),
                ),
                "user",
                window,
                cx,
            ),
            ssh_key_path: input(
                ssh.map(|s| s.key_path.clone())
                    .filter(|p| !p.is_empty())
                    .unwrap_or_else(|| "~/.ssh/id_ed25519".into()),
                "~/.ssh/id_ed25519",
                window,
                cx,
            ),
            ssh_secret: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder(if ssh.is_some() {
                        "unchanged"
                    } else {
                        "optional"
                    })
            }),
            error: None,
        }
    }

    fn set_driver(&mut self, driver: DriverKind, window: &mut Window, cx: &mut Context<Self>) {
        // Follow the default port unless the user typed a custom one.
        let port = self.port.read(cx).value();
        if port.is_empty() || port == self.driver.default_port().to_string().as_str() {
            self.port.update(cx, |s, cx| {
                s.set_value(driver.default_port().to_string(), window, cx)
            });
        }
        self.driver = driver;
        cx.notify();
    }

    /// Validates the form. On error the message is shown inline and `None` returned.
    pub fn submit(&mut self, cx: &mut Context<Self>) -> Option<FormOutput> {
        let value = |s: &Entity<InputState>| s.read(cx).value().trim().to_string();
        let host = value(&self.host);
        let user = value(&self.user);
        let port = value(&self.port).parse::<u16>();
        let (db_fields, user_fields) = (self.mode.has_db_fields(), self.mode.has_user_fields());
        let ssh = db_fields && self.ssh;
        let ssh_port = value(&self.ssh_port).parse::<u16>();
        let error = if db_fields && host.is_empty() {
            Some("Host is required")
        } else if user_fields && user.is_empty() {
            Some("User is required")
        } else if db_fields && port.is_err() {
            Some("Port must be a number")
        } else if ssh && value(&self.ssh_host).is_empty() {
            Some("SSH host is required")
        } else if ssh && value(&self.ssh_user).is_empty() {
            Some("SSH user is required")
        } else if ssh && ssh_port.is_err() {
            Some("SSH port must be a number")
        } else if ssh && self.ssh_auth == SshAuth::Key && value(&self.ssh_key_path).is_empty() {
            Some("SSH key file is required")
        } else {
            None
        };
        if let Some(e) = error {
            self.error = Some(e.into());
            cx.notify();
            return None;
        }
        let database = match (&self.base, db_fields) {
            (Some(base), false) => base.clone(),
            (base, _) => Database {
                id: base.as_ref().map_or_else(new_id, |b| b.id.clone()),
                name: value(&self.name),
                driver: self.driver,
                host,
                port: port.unwrap_or_default(),
                database: value(&self.database),
                tls: self.tls,
                users: base.as_ref().map(|b| b.users.clone()).unwrap_or_default(),
                ssh: ssh.then(|| SshConfig {
                    host: value(&self.ssh_host),
                    port: ssh_port.clone().unwrap_or(22),
                    user: value(&self.ssh_user),
                    auth: self.ssh_auth,
                    key_path: match self.ssh_auth {
                        SshAuth::Key => value(&self.ssh_key_path),
                        _ => String::new(),
                    },
                }),
            },
        };
        let secret = self.ssh_secret.read(cx).value().to_string();
        let ssh_secret =
            (ssh && self.ssh_auth != SshAuth::Agent && !secret.is_empty()).then_some(secret);
        let password = self.password.read(cx).value().to_string();
        let user = user_fields.then(|| {
            let id = self.user_id.clone().unwrap_or_else(new_id);
            (
                DbUser { id, user },
                (!password.is_empty()).then_some(password),
            )
        });
        Some(FormOutput {
            database,
            user,
            ssh_secret,
        })
    }
}

fn field(label: &'static str, input: impl IntoElement) -> Div {
    v_flex()
        .gap_1()
        .child(div().text_xs().child(label))
        .child(input)
}

impl Render for ConnectionForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let driver_button = |kind: DriverKind, current: DriverKind, cx: &mut Context<Self>| {
            let b = Button::new(kind.label()).label(kind.label()).small();
            let b = if kind == current {
                b.primary()
            } else {
                b.ghost()
            };
            b.on_click(cx.listener(move |this, _, window, cx| this.set_driver(kind, window, cx)))
        };
        let tls_button =
            |mode: TlsMode, label: &'static str, current: TlsMode, cx: &mut Context<Self>| {
                let b = Button::new(label).label(label).xsmall();
                let b = if mode == current {
                    b.primary()
                } else {
                    b.ghost()
                };
                b.on_click(cx.listener(move |this, _, _, cx| {
                    this.tls = mode;
                    cx.notify();
                }))
            };

        let db_fields = v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(driver_button(DriverKind::Postgres, self.driver, cx))
                    .child(driver_button(DriverKind::Mysql, self.driver, cx)),
            )
            .child(field("Name", Input::new(&self.name)))
            .child(
                h_flex()
                    .gap_2()
                    .child(field("Host", Input::new(&self.host)).flex_1())
                    .child(field("Port", Input::new(&self.port)).w(px(90.))),
            )
            .child(field("Database", Input::new(&self.database)))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_xs().child("TLS"))
                    .child(tls_button(TlsMode::Disable, "off", self.tls, cx))
                    .child(tls_button(TlsMode::Prefer, "prefer", self.tls, cx))
                    .child(tls_button(TlsMode::Require, "require", self.tls, cx)),
            );
        let auth_button = |auth: SshAuth, label: &'static str, cx: &mut Context<Self>| {
            let b = Button::new(label).label(label).xsmall();
            let b = if auth == self.ssh_auth {
                b.primary()
            } else {
                b.ghost()
            };
            b.on_click(cx.listener(move |this, _, _, cx| {
                this.ssh_auth = auth;
                cx.notify();
            }))
        };
        let ssh_fields = v_flex()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Host and port above are as seen from the SSH server."),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(field("SSH host", Input::new(&self.ssh_host)).flex_1())
                    .child(field("Port", Input::new(&self.ssh_port)).w(px(90.))),
            )
            .child(field("SSH user", Input::new(&self.ssh_user)))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_xs().child("Auth"))
                    .child(auth_button(SshAuth::Agent, "ssh-agent", cx))
                    .child(auth_button(SshAuth::Key, "key file", cx))
                    .child(auth_button(SshAuth::Password, "password", cx)),
            )
            .when(self.ssh_auth == SshAuth::Key, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .child(field("Key file", Input::new(&self.ssh_key_path)).flex_1())
                        .child(
                            field("Passphrase", Input::new(&self.ssh_secret).mask_toggle())
                                .w(px(150.)),
                        ),
                )
            })
            .when(self.ssh_auth == SshAuth::Password, |this| {
                this.child(field(
                    "SSH password",
                    Input::new(&self.ssh_secret).mask_toggle(),
                ))
            });
        let db_fields = db_fields
            .child(
                Checkbox::new("use-ssh")
                    .label("Connect through an SSH tunnel")
                    .checked(self.ssh)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.ssh = *checked;
                        cx.notify();
                    })),
            )
            .when(self.ssh, |this| this.child(ssh_fields));
        let user_fields = h_flex()
            .gap_2()
            .child(field("User", Input::new(&self.user)).flex_1())
            .child(field("Password", Input::new(&self.password).mask_toggle()).flex_1());

        v_flex()
            .gap_3()
            .when(self.mode.has_db_fields(), |this| this.child(db_fields))
            .when(self.mode.has_user_fields(), |this| this.child(user_fields))
            .children(
                self.error
                    .clone()
                    .map(|e| div().text_sm().text_color(cx.theme().danger).child(e)),
            )
    }
}
