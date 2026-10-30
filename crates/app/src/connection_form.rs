//! Form for creating/editing a database and its users. Rendered inside a dialog.
//!
//! One layout throughout (`ui::form_section`, `form_row`, `form_note`): sections
//! with a muted heading, a label column, and choices as segmented controls. The
//! databases offered come from [`OFFERED`]; nothing below names one, so adding a
//! database adds no code here. A file ([`DriverKind::is_file`]) has a path instead of
//! host, TLS, SSH and users: it gets one user of its own, without a password.

use gpui_kit::component::{
    Sizable as _,
    button::Button,
    h_flex,
    input::{Input, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_core::{
    AiAccess, Database, DbUser, DriverKind, SshAuth, SshConfig, TlsMode, ValueAccess,
};

use crate::ui::{
    FORM_LABEL_WIDTH, Severity, alert_card, form_note, form_row, form_section, segmented, switch,
    wrapped_tooltip,
};
use tangul_assist::dialect::dialect_for;
use tangul_data::profiles::{new_id, new_password};

/// The databases a connection can be set up for.
pub(crate) const OFFERED: [DriverKind; 3] =
    [DriverKind::Postgres, DriverKind::Mysql, DriverKind::Sqlite];

const TLS: [(TlsMode, &str); 3] = [
    (TlsMode::Disable, "Off"),
    (TlsMode::Prefer, "Prefer"),
    (TlsMode::Require, "Require"),
];

const TLS_INFO: &str = "Prefer encrypts when the server offers TLS, Require always. \
     Neither checks the server's certificate.";

const SSH_AUTH: [(SshAuth, &str); 3] = [
    (SshAuth::Agent, "SSH agent"),
    (SshAuth::Key, "Key file"),
    (SshAuth::Password, "Password"),
];

const ACCESS_INFO: &str = "With schema only, queries the AI writes open in a tab for \
     you to run. Model provider and MCP server: in the AI settings.";

const VALUES_INFO: &str = "From a sample of up to 10,000 rows per column. Columns that \
     look like passwords, tokens or keys are never read.";

const SET_UP_INFO: &str = "Adds a user that may only read, with a new password \
     (Keychain), as the AI's. Saving opens the statements that create it; run them \
     as an admin.";

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
    /// A read-only AI user to set up: already among the database's users and chosen
    /// as the AI's; its password goes to the Keychain and the statements creating it
    /// to a tab.
    pub ai_account: Option<(DbUser, String)>,
}

/// A file's one user: no name, no password.
fn file_user() -> DbUser {
    DbUser {
        id: new_id(),
        user: String::new(),
    }
}

/// A file dropped on the window, as if added in the form: named after the file, with
/// its one user.
pub fn file_database(path: &str) -> FormOutput {
    FormOutput {
        database: Database {
            id: new_id(),
            name: String::new(),
            driver: OFFERED
                .into_iter()
                .find(|d| d.is_file())
                .expect("a file database is offered"),
            host: String::new(),
            port: 0,
            database: path.to_string(),
            tls: TlsMode::Disable,
            users: Vec::new(),
            ssh: None,
            assistant_user: None,
            ai_values: ValueAccess::default(),
            ai_access: AiAccess::default(),
            ai_tools: true,
            editor_values: true,
        },
        user: Some((file_user(), None)),
        ssh_secret: None,
        ai_account: None,
    }
}

/// Name of the read-only user tangul sets up for the AI.
const AI_ACCOUNT: &str = "tangul_ai";

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
    /// A file's path ([`DriverKind::is_file`]).
    path: Entity<InputState>,
    ssh: bool,
    ssh_auth: SshAuth,
    ssh_host: Entity<InputState>,
    ssh_port: Entity<InputState>,
    ssh_user: Entity<InputState>,
    ssh_key_path: Entity<InputState>,
    ssh_secret: Entity<InputState>,
    /// The user the AI assistant connects as; `None` uses the selected one.
    assistant_user: Option<String>,
    /// What the assistant may read from the rows of this database.
    ai_values: ValueAccess,
    /// What AI tools may see: its data, or only its schema.
    ai_access: AiAccess,
    /// AI tools (MCP clients, the assistant) may use this database.
    ai_tools: bool,
    /// The editor may look at a few hundred rows of a text column to offer values.
    editor_values: bool,
    /// Set up by "Set up a read-only AI user…", saved with the form.
    ai_account: Option<(DbUser, String)>,
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
                db.filter(|p| !p.driver.is_file())
                    .map(|p| p.database.clone())
                    .unwrap_or_default(),
                "optional",
                window,
                cx,
            ),
            path: input(
                db.filter(|p| p.driver.is_file())
                    .map(|p| p.database.clone())
                    .unwrap_or_default(),
                "/path/to/file.db",
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
            assistant_user: db.and_then(|d| d.assistant_user.clone()),
            ai_values: db.map(|d| d.ai_values).unwrap_or_default(),
            ai_access: db.map(|d| d.ai_access).unwrap_or_default(),
            // On for a new database too: the user turns it off.
            ai_tools: db.is_none_or(|d| d.ai_tools),
            editor_values: db.is_none_or(|d| d.editor_values),
            ai_account: None,
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

    /// Asks for the file in the system's dialog.
    fn choose_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.path.update(cx, |s, cx| {
                    s.set_value(path.to_string_lossy().to_string(), window, cx)
                });
            });
        })
        .detach();
    }

    /// The databases to choose from: an existing database stays a file or a server
    /// (its users belong to one or the other).
    fn offered(&self) -> Vec<DriverKind> {
        OFFERED
            .into_iter()
            .filter(|d| match self.mode {
                FormMode::EditDatabase => d.is_file() == self.driver.is_file(),
                _ => true,
            })
            .collect()
    }

    /// Adds a new user for the AI (a free name, a random password) and makes it the
    /// AI's; it is created on the server by the statements opened after saving.
    fn set_up_ai_account(&mut self, cx: &mut Context<Self>) {
        let Some(base) = self.base.as_mut() else {
            return;
        };
        let taken = |name: &str| base.users.iter().any(|u| u.user == name);
        let name = (1..)
            .map(|n| match n {
                1 => AI_ACCOUNT.to_string(),
                n => format!("{AI_ACCOUNT}_{n}"),
            })
            .find(|name| !taken(name))
            .unwrap_or_default();
        let user = DbUser {
            id: new_id(),
            user: name,
        };
        base.users.push(user.clone());
        self.assistant_user = Some(user.id.clone());
        self.ai_account = Some((user, new_password()));
        cx.notify();
    }

    /// What files and servers share: the database's id, name and users, and what the
    /// AI and the editor may do. The rest is filled in by the caller.
    fn database_fields(&self, base: Option<&Database>, cx: &App) -> Database {
        Database {
            id: base.map_or_else(new_id, |b| b.id.clone()),
            name: self.name.read(cx).value().trim().to_string(),
            driver: self.driver,
            host: String::new(),
            port: 0,
            database: String::new(),
            tls: TlsMode::Disable,
            users: base.map(|b| b.users.clone()).unwrap_or_default(),
            ssh: None,
            assistant_user: None,
            ai_values: self.ai_values,
            ai_access: self.ai_access,
            ai_tools: self.ai_tools,
            editor_values: self.editor_values,
        }
    }

    /// Validates the form. On error the message is shown inline and `None` returned.
    pub fn submit(&mut self, cx: &mut Context<Self>) -> Option<FormOutput> {
        let value = |s: &Entity<InputState>| s.read(cx).value().trim().to_string();
        let host = value(&self.host);
        let user = value(&self.user);
        let port = value(&self.port).parse::<u16>();
        let (db_fields, user_fields) = (self.mode.has_db_fields(), self.mode.has_user_fields());
        let file = self.driver.is_file();
        let path = value(&self.path);
        let ssh = db_fields && self.ssh && !file;
        let ssh_port = value(&self.ssh_port).parse::<u16>();
        let error = if db_fields && file && path.is_empty() {
            Some("File is required")
        } else if file {
            None
        } else if db_fields && host.is_empty() {
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
            (base, _) if file => Database {
                host: String::new(),
                port: 0,
                database: path,
                tls: TlsMode::Disable,
                ssh: None,
                // Opened read-only for everyone: there is no user to choose.
                assistant_user: None,
                ..self.database_fields(base.as_ref(), cx)
            },
            (base, _) => Database {
                host,
                port: port.unwrap_or_default(),
                database: value(&self.database),
                tls: self.tls,
                assistant_user: self.assistant_user.clone(),
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
                ..self.database_fields(base.as_ref(), cx)
            },
        };
        let secret = self.ssh_secret.read(cx).value().to_string();
        let ssh_secret =
            (ssh && self.ssh_auth != SshAuth::Agent && !secret.is_empty()).then_some(secret);
        let password = self.password.read(cx).value().to_string();
        // A new file gets its one user; an edited one keeps it.
        let user = if file {
            (self.mode == FormMode::NewDatabase).then(|| (file_user(), None))
        } else {
            user_fields.then(|| {
                let id = self.user_id.clone().unwrap_or_else(new_id);
                (
                    DbUser { id, user },
                    (!password.is_empty()).then_some(password),
                )
            })
        };
        Some(FormOutput {
            database,
            user,
            ssh_secret,
            ai_account: self.ai_account.clone(),
        })
    }
}

impl ConnectionForm {
    fn render_connection(&self, cx: &mut Context<Self>) -> Div {
        let section = form_section("Connection", None, cx)
            .child(form_row(
                "Database",
                None,
                segmented(
                    "driver",
                    self.offered().into_iter().map(|d| (d, d.label().into())),
                    self.driver,
                    cx.listener(|this, driver: &DriverKind, window, cx| {
                        this.set_driver(*driver, window, cx)
                    }),
                    cx,
                ),
                cx,
            ))
            .child(form_row("Name", None, Input::new(&self.name), cx));
        if self.driver.is_file() {
            return section
                .child(form_row(
                    "File",
                    None,
                    h_flex()
                        .flex_1()
                        .gap_2()
                        .child(
                            div()
                                .id("form-path")
                                .test_support()
                                .flex_1()
                                .child(Input::new(&self.path)),
                        )
                        .child(
                            Button::new("choose-file")
                                .label("Choose…")
                                .small()
                                .outline()
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.choose_file(window, cx)),
                                ),
                        ),
                    cx,
                ))
                .child(form_note("Opened read-only.", cx));
        }
        section
            .child(form_row(
                "Host",
                None,
                h_flex()
                    .flex_1()
                    .gap_2()
                    .child(Input::new(&self.host).flex_1())
                    .child(Input::new(&self.port).w(px(72.))),
                cx,
            ))
            .when(self.ssh, |this| {
                this.child(form_note("As seen from the SSH server.", cx))
            })
            .child(form_row(
                "Database name",
                None,
                Input::new(&self.database),
                cx,
            ))
            .child(form_row(
                "TLS",
                Some(TLS_INFO),
                segmented(
                    "tls",
                    TLS.map(|(mode, label)| (mode, label.into())),
                    self.tls,
                    cx.listener(|this, mode: &TlsMode, _, cx| {
                        this.tls = *mode;
                        cx.notify();
                    }),
                    cx,
                ),
                cx,
            ))
    }

    fn render_user(&self, cx: &mut Context<Self>) -> Div {
        form_section("Sign in", None, cx)
            .child(form_row(
                "User",
                None,
                div()
                    .id("form-user")
                    .test_support()
                    .flex_1()
                    .child(Input::new(&self.user)),
                cx,
            ))
            .child(form_row(
                "Password",
                None,
                Input::new(&self.password).mask_toggle(),
                cx,
            ))
    }

    fn render_ssh(&self, cx: &mut Context<Self>) -> Div {
        let toggle = switch("use-ssh", cx)
            .checked(self.ssh)
            .small()
            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                this.ssh = *checked;
                cx.notify();
            }));
        form_section("SSH tunnel", Some(toggle.into_any_element()), cx).when(self.ssh, |this| {
            this.child(form_row(
                "SSH host",
                None,
                h_flex()
                    .flex_1()
                    .gap_2()
                    .child(Input::new(&self.ssh_host).flex_1())
                    .child(Input::new(&self.ssh_port).w(px(72.))),
                cx,
            ))
            .child(form_row("SSH user", None, Input::new(&self.ssh_user), cx))
            .child(form_row(
                "Sign in with",
                None,
                segmented(
                    "ssh-auth",
                    SSH_AUTH.map(|(auth, label)| (auth, label.into())),
                    self.ssh_auth,
                    cx.listener(|this, auth: &SshAuth, _, cx| {
                        this.ssh_auth = *auth;
                        cx.notify();
                    }),
                    cx,
                ),
                cx,
            ))
            .when(self.ssh_auth == SshAuth::Key, |this| {
                this.child(form_row(
                    "Key file",
                    None,
                    Input::new(&self.ssh_key_path),
                    cx,
                ))
                .child(form_row(
                    "Passphrase",
                    None,
                    Input::new(&self.ssh_secret).mask_toggle(),
                    cx,
                ))
            })
            .when(self.ssh_auth == SshAuth::Password, |this| {
                this.child(form_row(
                    "SSH password",
                    None,
                    Input::new(&self.ssh_secret).mask_toggle(),
                    cx,
                ))
            })
        })
    }

    fn render_ai(&self, cx: &mut Context<Self>) -> Div {
        let toggle = switch("ai-tools", cx)
            .checked(self.ai_tools)
            .small()
            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                this.ai_tools = *checked;
                cx.notify();
            }));
        let section = form_section("AI tools", Some(toggle.into_any_element()), cx);
        if !self.ai_tools {
            return section.child(form_note_flush(
                "Not offered to the assistant or MCP clients.",
                cx,
            ));
        }
        let section = section
            .child(form_row(
                "Access",
                Some(ACCESS_INFO),
                segmented(
                    "ai-access",
                    AiAccess::ALL.map(|a| (a, a.title().into())),
                    self.ai_access,
                    cx.listener(|this, access: &AiAccess, _, cx| {
                        this.ai_access = *access;
                        cx.notify();
                    }),
                    cx,
                ),
                cx,
            ))
            .child(form_note(self.ai_access.description(), cx));
        // Its user and value profiles only matter where the AI reads data, and the
        // user only once the database has users.
        if !self.ai_access.is_data() {
            return section;
        }
        // A file has no users to choose from: the AI opens it read-only too.
        let users = self
            .base
            .as_ref()
            .filter(|_| !self.driver.is_file())
            .map(|b| b.users.clone())
            .unwrap_or_default();
        let can_set_up = self.mode == FormMode::EditDatabase
            && self.ai_account.is_none()
            && dialect_for(self.driver)
                .readonly_account_sql("", "", "", &[])
                .is_some();
        section
            .when(!users.is_empty(), |this| {
                let options = std::iter::once((None, "Your user".into())).chain(
                    users
                        .iter()
                        .map(|u| (Some(u.id.clone()), SharedString::from(u.user.clone()))),
                );
                // Ids for the segmented control must be `Copy`: index into the list.
                let ids: Vec<Option<String>> = options.clone().map(|(id, _)| id).collect();
                let chosen = ids
                    .iter()
                    .position(|id| *id == self.assistant_user)
                    .unwrap_or(0);
                this.child(form_row(
                    "Connects as",
                    Some(crate::ui::USER_INFO),
                    segmented(
                        "assistant",
                        options.enumerate().map(|(ix, (_, label))| (ix, label)),
                        chosen,
                        cx.listener(move |this, ix: &usize, _, cx| {
                            this.assistant_user = ids[*ix].clone();
                            cx.notify();
                        }),
                        cx,
                    ),
                    cx,
                ))
                .child(match (&self.ai_account, &self.assistant_user) {
                    (Some((user, _)), _) => form_note(
                        format!(
                            "{} is added as the AI's user. Save, then run the statements \
                             in the new tab as an admin.",
                            user.user
                        ),
                        cx,
                    ),
                    (None, None) => form_note(crate::ui::YOUR_USER, cx),
                    (None, Some(_)) => form_note("Never connects as your user.", cx),
                })
                .when(can_set_up, |this| {
                    this.child(
                        h_flex().pl(px(FORM_LABEL_WIDTH + 12.)).child(
                            div()
                                .id("set-up-info")
                                .tooltip(wrapped_tooltip(SET_UP_INFO))
                                .child(
                                    Button::new("set-up-ai-account")
                                        .label("Set up a read-only AI user…")
                                        .xsmall()
                                        .outline()
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.set_up_ai_account(cx)
                                            }),
                                        ),
                                ),
                        ),
                    )
                })
            })
            .child(form_row(
                "Column values",
                Some(VALUES_INFO),
                segmented(
                    "ai-values",
                    ValueAccess::ALL.map(|a| (a, a.title().into())),
                    self.ai_values,
                    cx.listener(|this, access: &ValueAccess, _, cx| {
                        this.ai_values = *access;
                        cx.notify();
                    }),
                    cx,
                ),
                cx,
            ))
            .child(form_note(self.ai_values.description(), cx))
    }

    fn render_editor(&self, cx: &mut Context<Self>) -> Div {
        form_section("Editor", None, cx).child(
            h_flex()
                .gap_3()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .child("Suggest values while typing conditions"),
                )
                .child(
                    switch("editor-values", cx)
                        .checked(self.editor_values)
                        .small()
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.editor_values = *checked;
                            cx.notify();
                        })),
                ),
        )
    }
}

/// A note under a section heading, not under a control.
fn form_note_flush(text: &'static str, cx: &App) -> Div {
    form_note(text, cx).pl_0()
}

impl Render for ConnectionForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let db = self.mode.has_db_fields();
        let file = self.driver.is_file();
        v_flex()
            .id("connection-form")
            .test_support()
            .gap_5()
            .when(db, |this| this.child(self.render_connection(cx)))
            .when(self.mode.has_user_fields() && !file, |this| {
                this.child(self.render_user(cx))
            })
            .when(db && !file, |this| this.child(self.render_ssh(cx)))
            .when(db, |this| {
                this.child(self.render_ai(cx)).child(self.render_editor(cx))
            })
            .children(
                self.error
                    .clone()
                    .map(|e| alert_card(Severity::Error, e, cx)),
            )
    }
}
