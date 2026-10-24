//! Updates from tangul's GitHub releases (`tangul_update`): a check at start and
//! once a day while `updates.check` is on, the title bar's notice, and installing
//! the new release on a click (verified against this app's signature first), then
//! starting it.

use tangul_update::{Blocked, Release, Requirement, Version};

use super::*;
use crate::ui::{Severity, menu_alert};

/// How often to look again while the app runs.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);

#[derive(Default)]
pub(super) struct Updates {
    /// A newer release, once found.
    available: Option<Release>,
    /// What an update has to be signed like; else why this app can't install one
    /// itself (the notice then links to the release).
    installable: Option<Result<Requirement, Blocked>>,
    /// While the update downloads and installs.
    installing: bool,
    /// The checks; dropped when checking is switched off.
    checks: Option<Task<()>>,
}

impl Workspace {
    /// Starts or stops checking as `updates.check` says.
    pub(super) fn sync_updates(&mut self, cx: &mut Context<Self>) {
        // Tests never ask GitHub.
        if cfg!(test) {
            return;
        }
        if !settings::current(cx).updates.check {
            if self.updates.checks.take().is_some() && !self.updates.installing {
                self.updates.available = None;
                cx.notify();
            }
            return;
        }
        if self.updates.checks.is_some() {
            return;
        }
        self.updates.checks = Some(cx.spawn(async move |this, cx| {
            loop {
                let check = tangul_ai::runtime::spawn(tangul_update::check(Version::current()));
                match check.await.map_err(|e| e.to_string()).flatten() {
                    Ok(Some(release)) => {
                        // Runs `codesign`: not on the UI thread.
                        let installable = cx
                            .background_spawn(async {
                                tangul_update::running_app()
                                    .ok_or(Blocked::NotAnApp)
                                    .and_then(|app| tangul_update::installable(&app))
                            })
                            .await;
                        let shown = this.update(cx, |ws, cx| {
                            ws.updates.available = Some(release);
                            ws.updates.installable = Some(installable);
                            cx.notify();
                        });
                        if shown.is_err() {
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("update check: {e}"),
                }
                cx.background_executor().timer(CHECK_EVERY).await;
            }
        }));
    }

    /// A release as the check would find it.
    #[cfg(test)]
    pub(super) fn show_update(
        &mut self,
        release: Release,
        installable: Result<Requirement, Blocked>,
        installing: bool,
        cx: &mut Context<Self>,
    ) {
        self.updates.available = Some(release);
        self.updates.installable = Some(installable);
        self.updates.installing = installing;
        cx.notify();
    }

    /// Removes what the last update left beside the app (the replaced version).
    pub(super) fn tidy_after_update(cx: &mut Context<Self>) {
        if let Some(app) = tangul_update::running_app() {
            cx.background_spawn(async move { tangul_update::remove_old(&app) })
                .detach();
        }
    }

    /// Downloads, verifies and installs the release, then starts it and quits; a
    /// failure leaves this app as it is and says why.
    fn install_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(release), Some(Ok(requirement)), Some(app)) = (
            self.updates.available.clone(),
            self.updates.installable.clone(),
            tangul_update::running_app(),
        ) else {
            return;
        };
        self.updates.installing = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let dir = std::env::temp_dir().join(format!("tangul-update-{}", release.version));
            let version = release.version;
            let download = {
                let dir = dir.clone();
                tangul_ai::runtime::spawn(async move {
                    std::fs::create_dir_all(&dir)
                        .map_err(|e| format!("Could not download the update: {e}"))?;
                    tangul_update::download(&release, &dir).await
                })
            };
            let installed = match download.await.map_err(|e| e.to_string()).flatten() {
                Ok(downloaded) => {
                    let app = app.clone();
                    cx.background_spawn(async move {
                        tangul_update::install(&downloaded, &app, &requirement, version)
                    })
                    .await
                }
                Err(e) => Err(e),
            };
            let _ = std::fs::remove_dir_all(&dir);
            let started = installed.and_then(|()| tangul_update::relaunch(&app));
            let _ = this.update_in(cx, |ws, window, cx| match started {
                Ok(()) => cx.quit(),
                Err(e) => {
                    ws.updates.installing = false;
                    window.push_notification(Notification::error(e), cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The title bar's notice of a newer release: a menu to install it (or why
    /// this app can't, and where to get it) and its notes.
    pub(super) fn render_update_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let release = self.updates.available.as_ref()?;
        let theme = cx.theme();
        if self.updates.installing {
            return Some(
                h_flex()
                    .id("update-installing")
                    .test_support()
                    .gap_1()
                    .px_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(Spinner::new().xsmall())
                    .child(format!("Updating to {}…", release.version))
                    .into_any_element(),
            );
        }
        let weak = cx.entity().downgrade();
        let page = release.page.clone();
        let blocked = match &self.updates.installable {
            Some(Err(blocked)) => Some(blocked.to_string()),
            _ => None,
        };
        Some(
            Button::new("update-notice")
                .label(format!("Update to {}", release.version))
                .icon(Icon::new(Lucide::CircleArrowDown).text_color(theme.primary))
                .ghost()
                .small()
                .dropdown_menu(move |mut menu, _, _| {
                    menu = match &blocked {
                        None => {
                            let weak = weak.clone();
                            menu.item(PopupMenuItem::new("Install and restart").on_click(
                                move |_, window, cx| {
                                    let _ = weak.update(cx, |ws, cx| ws.install_update(window, cx));
                                },
                            ))
                        }
                        Some(reason) => menu.item(menu_alert(
                            Severity::Warning,
                            format!("Can't install it here: {reason}."),
                        )),
                    };
                    let page = page.clone();
                    menu.item(
                        PopupMenuItem::new(if blocked.is_some() {
                            "Download from GitHub"
                        } else {
                            "Release notes"
                        })
                        .on_click(move |_, _, cx| cx.open_url(&page)),
                    )
                })
                .into_any_element(),
        )
    }
}
