//! Drawing a board: the header (name, description, links, refresh and edit), and
//! the tiles in rows, full or half width; in edit mode with handles to drag them,
//! their width, labels and removing them.

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    spinner::Spinner,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_assist::suggest::one_line;
use tangul_core::board::{LinkTo, Refresh, Row, Tile, Width, is_web, rows};
use tangul_service::boards::{self, Edit};

use tangul_core::chart::{ChartStyle, ChartView};

use super::form::{AskForm, QueryForm, TileForm, view_label};
use super::{BoardEvent, BoardTab, link_action, link_icon};
use crate::assets::{Lucide, style_icon};
use crate::result_view::{SetView, chart_menu};
use crate::ui::enter_saves;

/// A tile's height; tables scroll inside it.
const TILE_HEIGHT: f32 = 300.;
/// The intervals offered, in seconds.
const INTERVALS: [u32; 6] = [10, 30, 60, 300, 900, 3600];

/// A tile being dragged to another place.
#[derive(Clone)]
struct DraggedTile {
    id: String,
    title: SharedString,
}

impl Render for DraggedTile {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_2()
            .py_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.ring)
            .bg(theme.background)
            .text_sm()
            .child(self.title.clone())
    }
}

/// "Every 30 s", "Every 5 min", …
fn refresh_label(refresh: Refresh) -> String {
    match refresh {
        Refresh::OnOpen => "On open".into(),
        Refresh::Manual => "By hand".into(),
        Refresh::Every(secs) => format!("Every {}", interval_label(secs)),
    }
}

fn interval_label(secs: u32) -> String {
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs / 60),
        _ => format!("{} h", secs / 3600),
    }
}

/// "just now", "3 min ago".
fn ago(elapsed: Duration) -> String {
    match elapsed.as_secs() {
        0..60 => "just now".into(),
        s @ 60..3600 => format!("{} min ago", s / 60),
        s => format!("{} h ago", s / 3600),
    }
}

impl BoardTab {
    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let board = &self.board;
        let status: AnyElement = match (&self.conn, self.running, &self.failed, self.updated) {
            (Err(why), ..) => div()
                .text_color(theme.danger)
                .child(why.clone())
                .into_any_element(),
            (_, Some(_), ..) => Spinner::new().xsmall().into_any_element(),
            (_, _, Some(error), _) => div()
                .text_color(theme.danger)
                .truncate()
                .child(error.clone())
                .into_any_element(),
            (_, _, _, Some(at)) => div()
                .child(format!("Updated {}", ago(at.elapsed())))
                .into_any_element(),
            _ => div().into_any_element(),
        };
        let refresh = board.refresh;
        let entity = cx.entity().downgrade();
        let refresh_menu = Button::new("board-refresh-mode")
            .label(refresh_label(refresh))
            .ghost()
            .xsmall()
            .dropdown_menu(move |mut menu, _, _| {
                let mut choices = vec![Refresh::OnOpen];
                choices.extend(INTERVALS.map(Refresh::Every));
                choices.push(Refresh::Manual);
                for choice in choices {
                    let entity = entity.clone();
                    menu = menu.item(
                        PopupMenuItem::new(refresh_label(choice))
                            .checked(choice == refresh)
                            .on_click(move |_, _, cx| {
                                let _ = entity.update(cx, |this, cx| this.set_refresh(choice, cx));
                            }),
                    );
                }
                menu
            });
        let (first, more) = match board.description.split_once('\n') {
            Some((first, _)) => (first, true),
            None => (board.description.as_str(), false),
        };
        let description = (!board.description.is_empty()).then(|| {
            let text = if self.show_description {
                board.description.clone()
            } else {
                first.to_string()
            };
            h_flex()
                .gap_1()
                .items_start()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(
                    div()
                        .min_w_0()
                        .when(!self.show_description, |d| d.truncate())
                        .child(text),
                )
                .when(more || self.show_description, |this| {
                    let label = if self.show_description {
                        "less"
                    } else {
                        "more…"
                    };
                    this.child(
                        Button::new("board-description-more")
                            .label(label)
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_description = !this.show_description;
                                cx.notify();
                            })),
                    )
                })
        });
        let links = (!board.links.is_empty()).then(|| {
            h_flex()
                .gap_1()
                .flex_wrap()
                .children(board.links.iter().enumerate().map(|(i, link)| {
                    let broken = self.is_broken(i);
                    let tooltip: SharedString = if broken {
                        "It is gone: remove the link in the board's details".into()
                    } else {
                        link_action(&link.to).into()
                    };
                    let to = link.to.clone();
                    h_flex()
                        .id(("board-link", i))
                        .test_support()
                        .gap_1()
                        .px_2()
                        .py_0p5()
                        .items_center()
                        .rounded_full()
                        .border_1()
                        .border_color(theme.border)
                        .text_xs()
                        .when(broken, |this| this.text_color(theme.muted_foreground))
                        .when(!broken, |this| {
                            this.cursor_pointer().hover(|s| s.bg(theme.secondary))
                        })
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                        .child(Icon::new(link_icon(&link.to)).xsmall())
                        .child(link.label.clone())
                        .on_click(cx.listener(move |_, _, _, cx| match &to {
                            _ if broken => {}
                            LinkTo::Url(url) => {
                                if is_web(url) {
                                    cx.open_url(url)
                                }
                            }
                            _ => cx.emit(BoardEvent::OpenLink(i)),
                        }))
                }))
        });
        v_flex()
            .px_4()
            .pt_3()
            .pb_3()
            .gap_1p5()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(board.name.clone()),
                    )
                    .when(self.editing, |this| {
                        this.child(
                            Button::new("board-details")
                                .icon(Icon::new(Lucide::Pencil))
                                .label("Details")
                                .ghost()
                                .xsmall()
                                .on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(BoardEvent::EditDetails)),
                                ),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(status)
                            .child(self.render_add_menu(cx))
                            .child(refresh_menu)
                            .child(
                                Button::new("board-refresh")
                                    .icon(IconName::RotateCw)
                                    .ghost()
                                    .xsmall()
                                    .tooltip("Refresh now")
                                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                            )
                            .child(
                                Button::new("board-edit")
                                    .icon(Icon::new(Lucide::Pencil))
                                    .label(if self.editing { "Done" } else { "Edit" })
                                    .ghost()
                                    .xsmall()
                                    .selected(self.editing)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.editing = !this.editing;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .children(description)
            .children(links)
            .into_any_element()
    }

    fn render_tiles(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        if self.board.tiles.is_empty() {
            return div()
                .p_4()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(
                    "No tiles yet. Add a query with “Add to board” in a query tab or a \
                     bookmark's menu.",
                )
                .into_any_element();
        }
        let tiles = &self.board.tiles;
        v_flex()
            .p_4()
            .gap_3()
            .children(rows(tiles).into_iter().map(|row| {
                match row {
                    Row::Full(i) => h_flex()
                        .w_full()
                        .child(self.render_tile(i, &tiles[i], cx))
                        .into_any_element(),
                    Row::Halves(a, b) => h_flex()
                        .w_full()
                        .gap_3()
                        .child(self.render_tile(a, &tiles[a], cx))
                        .child(match b {
                            Some(b) => self.render_tile(b, &tiles[b], cx).into_any_element(),
                            // Still half wide on its own.
                            None => div().flex_1().into_any_element(),
                        })
                        .into_any_element(),
                }
            }))
            .into_any_element()
    }

    fn render_tile(&self, ix: usize, tile: &Tile, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let view = self.tiles.get(&tile.id);
        let title: SharedString = if tile.title.is_empty() {
            "Untitled".into()
        } else {
            tile.title.clone().into()
        };
        let id = tile.id.clone();
        let body: AnyElement = match view {
            Some(view) if view.loaded => view.result.clone().into_any_element(),
            _ if self.running.is_some() => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(Spinner::new().small())
                .into_any_element(),
            _ => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(match self.board.refresh {
                    Refresh::Manual => "Press refresh to run it",
                    _ => "Not run yet",
                })
                .into_any_element(),
        };
        let open_id = id.clone();
        // In both modes: a tile's query in a tab of its own, to try other charts or
        // see the table behind one.
        let open = Button::new(("tile-open", ix))
            .icon(Icon::new(Lucide::ExternalLink))
            .ghost()
            .xsmall()
            .tooltip("Open in a tab")
            .on_click(cx.listener(move |this, _, _, cx| this.open_tile(&open_id, cx)));
        // A chart the result doesn't fit is shown as the table; this says why.
        let unfit = view
            .filter(|v| v.loaded && tile.view.is_some())
            .and_then(|v| v.result.read(cx).chart().err());
        let actions = if self.editing {
            let (width_id, label_id, remove_id) = (id.clone(), id.clone(), id.clone());
            // With a result: the status bar's menu, of what it fits. Before one: the
            // table and every style, fitted to the result once it is there.
            let (chart, shown) = match view.filter(|v| v.loaded) {
                Some(v) => {
                    let r = v.result.read(cx);
                    (r.chart().ok(), r.chart_shown())
                }
                None => (None, tile.view),
            };
            let (view_id, this) = (id.clone(), cx.entity().downgrade());
            let set: SetView = Rc::new(move |view, cx| {
                let _ = this.update(cx, |b, cx| b.set_tile_view(&view_id, view, cx));
            });
            let shown_as = Button::new(("tile-view", ix))
                .icon(Icon::new(style_icon(shown.map(|v| v.style))))
                .ghost()
                .xsmall()
                .tooltip("Shown as")
                .dropdown_menu(move |menu, window, cx| match &chart {
                    Some(data) => chart_menu(menu, data, shown, set.clone(), window, cx),
                    None => {
                        let views = std::iter::once(None).chain(ChartStyle::ALL.map(|style| {
                            Some(ChartView {
                                style,
                                series: None,
                            })
                        }));
                        views.fold(menu, |menu, view| {
                            let set = set.clone();
                            menu.item(
                                PopupMenuItem::new(view_label(view))
                                    .icon(Icon::new(style_icon(view.map(|v| v.style))))
                                    .checked(view.map(|v| v.style) == shown.map(|v| v.style))
                                    .on_click(move |_, _, cx| set(view, cx)),
                            )
                        })
                    }
                });
            let (icon, tooltip, width) = match tile.width {
                Width::Full => (Lucide::Minimize2, "Half width", Width::Half),
                Width::Half => (Lucide::Maximize2, "Full width", Width::Full),
            };
            h_flex()
                .gap_0p5()
                .child(shown_as)
                .child(
                    Button::new(("tile-width", ix))
                        .icon(Icon::new(icon))
                        .ghost()
                        .xsmall()
                        .tooltip(tooltip)
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(BoardEvent::Edit(Edit::SetWidth {
                                tile: width_id.clone(),
                                width,
                            }))
                        })),
                )
                .child(
                    Button::new(("tile-label", ix))
                        .icon(Icon::new(Lucide::Pencil))
                        .ghost()
                        .xsmall()
                        .tooltip("Title and description")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_tile_form(&label_id, window, cx)
                        })),
                )
                .child(open)
                .child(
                    Button::new(("tile-remove", ix))
                        .icon(IconName::Delete)
                        .ghost()
                        .xsmall()
                        .tooltip("Remove from the board")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(BoardEvent::Edit(Edit::RemoveTile(remove_id.clone())))
                        })),
                )
        } else {
            h_flex().child(open)
        };
        let dragged = DraggedTile {
            id: id.clone(),
            title: title.clone(),
        };
        let header = h_flex()
            .h_9()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .when(self.editing, |this| {
                this.child(
                    div()
                        .id(("tile-grip", ix))
                        .test_support()
                        .cursor_grab()
                        .text_color(theme.muted_foreground)
                        .child(Icon::new(Lucide::GripVertical).xsmall())
                        .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone())),
                )
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .id(("tile-title", ix))
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .when(!self.editing, |this| {
                                let open_id = id.clone();
                                this.cursor_pointer().on_click(
                                    cx.listener(move |this, _, _, cx| this.open_tile(&open_id, cx)),
                                )
                            })
                            .child(title),
                    )
                    .when(!tile.description.is_empty(), |this| {
                        this.child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(tile.description.clone()),
                        )
                    }),
            )
            .when_some(unfit, |this, reason| {
                this.child(
                    div()
                        .id(("tile-unfit", ix))
                        .test_support()
                        .text_color(theme.warning)
                        .child(Icon::new(IconName::TriangleAlert).xsmall())
                        .tooltip(move |window, cx| {
                            Tooltip::new(format!("Shown as the table: {reason}")).build(window, cx)
                        }),
                )
            })
            .child(actions);
        let drop_id = id.clone();
        v_flex()
            .id(SharedString::from(format!("tile-{id}")))
            .test_support()
            .flex_1()
            .min_w_0()
            .h(px(TILE_HEIGHT))
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius)
            .overflow_hidden()
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
            .when(self.editing, |this| {
                this.drag_over::<DraggedTile>(|style, _, _, cx| style.border_color(cx.theme().ring))
                    .on_drop(cx.listener(move |_, dragged: &DraggedTile, _, cx| {
                        if dragged.id != drop_id {
                            cx.emit(BoardEvent::Edit(Edit::MoveTile {
                                tile: dragged.id.clone(),
                                to: ix,
                            }));
                        }
                    }))
            })
    }

    /// "Add tile": from a bookmark or the history in one click, a query typed in,
    /// or (with the assistant set up) asked of the AI.
    fn render_add_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (this, choices) = (cx.entity().downgrade(), self.choices.clone());
        Button::new("board-add")
            .icon(IconName::Plus)
            .label("Add tile")
            .ghost()
            .xsmall()
            .dropdown_menu(move |mut menu, window, cx| {
                let choices = choices.as_ref().map(|f| f(cx)).unwrap_or_default();
                let add = |label: String, tile: Tile, this: &WeakEntity<BoardTab>| {
                    let this = this.clone();
                    PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        let tile = tile.clone();
                        let _ =
                            this.update(cx, |_, cx| cx.emit(BoardEvent::Edit(Edit::AddTile(tile))));
                    })
                };
                if !choices.bookmarks.is_empty() {
                    let (bookmarks, this) = (choices.bookmarks.clone(), this.clone());
                    menu = menu.submenu("From bookmark", window, cx, move |mut menu, _, _| {
                        for b in &bookmarks {
                            let tile = boards::tile(&b.name, &b.sql, b.view);
                            menu = menu.item(add(b.name.clone(), tile, &this));
                        }
                        menu.max_h(px(320.)).scrollable(true)
                    });
                }
                if !choices.history.is_empty() {
                    let (history, this) = (choices.history.clone(), this.clone());
                    menu = menu.submenu("From history", window, cx, move |mut menu, _, _| {
                        for sql in &history {
                            let line = one_line(sql, 60);
                            let tile = boards::tile(&one_line(sql, 40), sql, None);
                            menu = menu.item(add(line, tile, &this));
                        }
                        menu.max_h(px(320.)).scrollable(true)
                    });
                }
                let query = this.clone();
                menu = menu.item(PopupMenuItem::new("New query…").on_click(
                    move |_, window, cx| {
                        let _ = query.update(cx, |t, cx| t.open_query_form(window, cx));
                    },
                ));
                if choices.ai {
                    let ask = this.clone();
                    menu = menu.item(PopupMenuItem::new("Ask AI…").on_click(
                        move |_, window, cx| {
                            let _ = ask.update(cx, |t, cx| t.open_ask_form(window, cx));
                        },
                    ));
                }
                menu
            })
    }

    /// A tile from a query typed in.
    fn open_query_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let form = cx.new(|cx| QueryForm::new(window, cx));
        let board = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let save = {
                let (form, board) = (form.clone(), board.clone());
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let Some(tile) = form.update(cx, |f, cx| f.submit(cx)) else {
                        return;
                    };
                    let _ =
                        board.update(cx, |_, cx| cx.emit(BoardEvent::Edit(Edit::AddTile(tile))));
                    window.close_dialog(cx);
                }
            };
            let in_tab = {
                let (form, board) = (form.clone(), board.clone());
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let (title, sql) = form.read(cx).draft(cx);
                    let _ =
                        board.update(cx, |_, cx| cx.emit(BoardEvent::NewTileTab { title, sql }));
                    window.close_dialog(cx);
                }
            };
            dialog
                .on_ok(enter_saves(save.clone()))
                .title("New tile")
                .w(px(560.))
                .child(form.clone())
                .footer(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            Button::new("tile-in-tab")
                                .label("Open in a tab")
                                .icon(Icon::new(Lucide::ExternalLink))
                                .ghost()
                                .tooltip("Edit the tile's query in a query tab")
                                .on_click(in_tab),
                        )
                        .child(footer(save)),
                )
        });
    }

    /// What the tile should show, asked of the assistant.
    fn open_ask_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let form = cx.new(|cx| AskForm::new(window, cx));
        let board = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let save = {
                let (form, board) = (form.clone(), board.clone());
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let question = form.read(cx).question(cx);
                    if question.is_empty() {
                        return;
                    }
                    let _ = board.update(cx, |_, cx| cx.emit(BoardEvent::AskAi(question)));
                    window.close_dialog(cx);
                }
            };
            dialog
                .on_ok(enter_saves(save.clone()))
                .title("Ask AI for a tile")
                .w(px(480.))
                .child(form.clone())
                .footer(footer(save))
        });
    }

    fn open_tile_form(&mut self, tile: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tile) = self.board.tile(tile).cloned() else {
            return;
        };
        let form = cx.new(|cx| TileForm::new(&tile, window, cx));
        let board = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let save = {
                let (form, board, id) = (form.clone(), board.clone(), tile.id.clone());
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let (title, description) = form.read(cx).submit(cx);
                    let _ = board.update(cx, |_, cx| {
                        cx.emit(BoardEvent::Edit(Edit::LabelTile {
                            tile: id.clone(),
                            title,
                            description,
                        }))
                    });
                    window.close_dialog(cx);
                }
            };
            dialog
                .on_ok(enter_saves(save.clone()))
                .title("Tile")
                .w(px(460.))
                .child(form.clone())
                .footer(footer(save))
        });
    }
}

fn footer(save: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> impl IntoElement {
    h_flex()
        .w_full()
        .gap_2()
        .justify_end()
        .child(
            Button::new("cancel")
                .label("Cancel")
                .ghost()
                .on_click(|_, window, cx| window.close_dialog(cx)),
        )
        .child(Button::new("save").label("Save").primary().on_click(save))
}

impl Render for BoardTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh_if_due(window, cx);
        let header = self.render_header(cx);
        let tiles = self.render_tiles(cx);
        let background = cx.theme().background;
        v_flex()
            .id("board")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(background)
            .child(header)
            .child(
                div()
                    .id("board-tiles")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(tiles),
            )
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's `test` attribute would shadow the built-in one.
    use super::{Duration, Refresh, ago, refresh_label};

    #[test]
    fn labels() {
        assert_eq!(refresh_label(Refresh::OnOpen), "On open");
        assert_eq!(refresh_label(Refresh::Manual), "By hand");
        assert_eq!(refresh_label(Refresh::Every(10)), "Every 10 s");
        assert_eq!(refresh_label(Refresh::Every(300)), "Every 5 min");
        assert_eq!(refresh_label(Refresh::Every(3600)), "Every 1 h");
        assert_eq!(ago(Duration::from_secs(5)), "just now");
        assert_eq!(ago(Duration::from_secs(185)), "3 min ago");
        assert_eq!(ago(Duration::from_secs(7300)), "2 h ago");
    }
}
