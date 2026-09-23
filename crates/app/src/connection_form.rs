//! Form for creating/editing a connection profile. Rendered inside a dialog.

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    v_flex,
};
use gpui_kit::*;
use rusdb_core::{DriverKind, Profile, TlsMode};

use crate::store::new_profile_id;

pub struct ConnectionForm {
    profile_id: Option<String>,
    driver: DriverKind,
    tls: TlsMode,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    database: Entity<InputState>,
    error: Option<SharedString>,
}

impl ConnectionForm {
    /// `profile` = None creates a new profile. The password field stays empty when
    /// editing; empty means "keep the stored password".
    pub fn new(profile: Option<&Profile>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let driver = profile.map_or(DriverKind::Postgres, |p| p.driver);
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
            profile_id: profile.map(|p| p.id.clone()),
            driver,
            tls: profile.map_or(TlsMode::Prefer, |p| p.tls),
            name: input(
                profile.map(|p| p.name.clone()).unwrap_or_default(),
                "optional",
                window,
                cx,
            ),
            host: input(
                profile.map_or("localhost".into(), |p| p.host.clone()),
                "localhost",
                window,
                cx,
            ),
            port: input(
                profile
                    .map_or(driver.default_port(), |p| p.port)
                    .to_string(),
                "port",
                window,
                cx,
            ),
            user: input(
                profile.map(|p| p.user.clone()).unwrap_or_default(),
                "user",
                window,
                cx,
            ),
            password: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder(if profile.is_some() {
                        "unchanged"
                    } else {
                        "password"
                    })
            }),
            database: input(
                profile.map(|p| p.database.clone()).unwrap_or_default(),
                "optional",
                window,
                cx,
            ),
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
    pub fn submit(&mut self, cx: &mut Context<Self>) -> Option<(Profile, Option<String>)> {
        let value = |s: &Entity<InputState>| s.read(cx).value().trim().to_string();
        let host = value(&self.host);
        let user = value(&self.user);
        let port = value(&self.port).parse::<u16>();
        let error = if host.is_empty() {
            Some("Host is required")
        } else if user.is_empty() {
            Some("User is required")
        } else if port.is_err() {
            Some("Port must be a number")
        } else {
            None
        };
        if let Some(e) = error {
            self.error = Some(e.into());
            cx.notify();
            return None;
        }
        let password = self.password.read(cx).value().to_string();
        let profile = Profile {
            id: self.profile_id.clone().unwrap_or_else(new_profile_id),
            name: value(&self.name),
            driver: self.driver,
            host,
            port: port.unwrap_or_default(),
            user,
            database: value(&self.database),
            tls: self.tls,
        };
        Some((profile, (!password.is_empty()).then_some(password)))
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

        v_flex()
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
            .child(
                h_flex()
                    .gap_2()
                    .child(field("User", Input::new(&self.user)).flex_1())
                    .child(field("Password", Input::new(&self.password).mask_toggle()).flex_1()),
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
            )
            // TODO(ssh): SSH tunnel section (host, user, password | key | agent).
            .children(
                self.error
                    .clone()
                    .map(|e| div().text_sm().text_color(cx.theme().danger).child(e)),
            )
    }
}
