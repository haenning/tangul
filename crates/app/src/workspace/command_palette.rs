//! The command palette (⌘⇧P) and its sidebar pills.

use super::*;

impl Workspace {
    pub(super) fn open_palette(
        &mut self,
        _: &OpenPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let db = self.active_db.clone();
        let bookmarks = self.data.local.bookmarks_for(db.as_deref());
        let history = match &db {
            Some(db) => self.data.local.history(db, "", HISTORY_LIMIT),
            None => Vec::new(),
        };
        let groups = palette::groups(&bookmarks, &history);
        let list = cx.new(|cx| CommandState::new(window, cx));
        let workspace = cx.entity().downgrade();
        window.open_dialog(cx, {
            let list = list.clone();
            move |dialog, _, _| {
                let mut command = PaletteList::new(&list)
                    .placeholder("Search commands, bookmarks and history")
                    .bordered(false);
                for group in &groups {
                    let icon = || match group.kind {
                        PaletteKind::Command => Icon::new(IconName::Settings),
                        PaletteKind::Bookmark => Icon::new(Lucide::Bookmark),
                        PaletteKind::History => Icon::new(Lucide::RotateCcwClock),
                    };
                    command = command.group(CommandGroup::new().label(group.label).items(
                        group.items.iter().map(|item| {
                            CommandItem::new()
                                .label(item.label.clone())
                                .keywords([item.search.clone()])
                                .icon(icon())
                        }),
                    ));
                }
                // Groups and items keep their positions while the list is filtered.
                let actions: Vec<Vec<PaletteAction>> = groups
                    .iter()
                    .map(|g| g.items.iter().map(|i| i.action.clone()).collect())
                    .collect();
                let pills = {
                    let workspace = workspace.clone();
                    move |_: &CommandState, _: &mut Window, cx: &mut App| {
                        palette_pills(&workspace, cx)
                    }
                };
                let cycle = |step: isize| {
                    let workspace = workspace.clone();
                    move |window: &mut Window, cx: &mut App| {
                        let _ = workspace.update(cx, |ws, cx| ws.cycle_panel(step, cx));
                        // The dialog isn't part of the workspace view; redraw it too.
                        window.refresh();
                    }
                };
                let (next, prev) = (cycle(1), cycle(-1));
                let command = command.header(pills);
                let workspace = workspace.clone();
                dialog.w(px(560.)).close_button(false).child(
                    div()
                        .key_context("Palette")
                        .on_action(move |_: &NextSidebarPanel, window, cx| next(window, cx))
                        .on_action(move |_: &PrevSidebarPanel, window, cx| prev(window, cx))
                        .child(command.on_confirm(move |path, window, cx| {
                            let Some(action) = actions
                                .get(path.section)
                                .and_then(|items| items.get(path.row))
                                .cloned()
                            else {
                                return;
                            };
                            window.close_dialog(cx);
                            let _ =
                                workspace.update(cx, |ws, cx| ws.run_palette(action, window, cx));
                        })),
                )
            }
        });
        // The dialog is built on the next frame; focus the search field once it exists.
        window.defer(cx, move |window, cx| {
            list.update(cx, |list, cx| list.focus(window, cx))
        });
    }

    pub(super) fn run_palette(
        &mut self,
        action: PaletteAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            PaletteAction::Settings => self.open_settings(window, cx),
            PaletteAction::RunAll => {
                if let Some(tab) = self.tabs.active_query().cloned() {
                    tab.update(cx, |t, cx| t.run_all(&RunAll, window, cx));
                }
            }
            PaletteAction::Bookmark(id) => self.open_bookmark(&id, false, window, cx),
            PaletteAction::History(sql) => self.open_sql("History", &sql, window, cx),
        }
    }
}

/// Command palette header: one pill per sidebar panel, the shown one selected.
/// A click (or Tab / Shift-Tab) switches the sidebar; the palette stays open.
pub(super) fn palette_pills(workspace: &WeakEntity<Workspace>, cx: &App) -> AnyElement {
    let Some(ws) = workspace.upgrade() else {
        return div().into_any_element();
    };
    let ws = ws.read(cx);
    let theme = cx.theme();
    h_flex()
        .px_3()
        .pt_2()
        .gap_1()
        .items_center()
        .children(ws.left.panels.iter().map(|&view| {
            let workspace = workspace.clone();
            Button::new(("palette-panel", view as usize))
                .icon(Icon::new(view.icon()))
                .label(view.label())
                .xsmall()
                .ghost()
                .selected(ws.left.shows(view))
                .on_click(move |_, window, cx| {
                    let _ = workspace.update(cx, |ws, cx| ws.show_panel(view, cx));
                    window.refresh();
                })
        }))
        .child(div().flex_1())
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("Tab to switch"),
        )
        .into_any_element()
}
