//! The board's details (name, description, links), a tile's title and
//! description, and "Add to board", edited in dialogs. Links are shown by their
//! label and what they open, never by where they point.

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_assist::suggest::one_line;
use tangul_core::board::{Board, Link, LinkTo, Tile};
use tangul_core::chart::{ChartStyle, ChartView};

use super::{link_action, link_icon};

/// What the details form gives back.
pub struct Details {
    pub name: String,
    pub description: String,
    pub links: Vec<Link>,
}

/// What links can go to: the database's bookmarks (id, name, chart), its other
/// boards (id, name) and recent queries.
#[derive(Clone, Default)]
pub struct Choices {
    pub bookmarks: Vec<(String, String, Option<ChartView>)>,
    pub boards: Vec<(String, String)>,
    pub history: Vec<String>,
}

/// A link being edited.
struct LinkRow {
    label: Entity<InputState>,
    to: RowTo,
}

enum RowTo {
    Url(Entity<InputState>),
    /// Id and name (`None`: gone).
    Bookmark(String, Option<String>),
    Board(String, Option<String>),
    Query {
        sql: Entity<TextareaState>,
        view: Option<ChartView>,
    },
}

pub struct DetailsForm {
    name: Entity<InputState>,
    description: Entity<TextareaState>,
    links: Vec<LinkRow>,
    choices: Choices,
    error: Option<SharedString>,
}

impl DetailsForm {
    pub fn new(
        board: &Board,
        choices: Choices,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Name")
                .default_value(board.name.clone())
        });
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .placeholder("What it is for")
                .default_value(board.description.clone())
        });
        window.focus(&name.read(cx).focus_handle(cx), cx);
        let mut form = Self {
            name,
            description,
            links: Vec::new(),
            choices,
            error: None,
        };
        for link in &board.links {
            form.push(link.clone(), window, cx);
        }
        form
    }

    /// Adds `link` at the end.
    fn push(&mut self, link: Link, window: &mut Window, cx: &mut Context<Self>) {
        let label = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Label")
                .default_value(link.label.clone())
        });
        let to = match link.to {
            LinkTo::Url(url) => RowTo::Url(cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("https://…")
                    .default_value(url)
            })),
            LinkTo::Bookmark(id) => {
                let name = self
                    .choices
                    .bookmarks
                    .iter()
                    .find(|b| b.0 == id)
                    .map(|b| b.1.clone());
                RowTo::Bookmark(id, name)
            }
            LinkTo::Board(id) => {
                let name = self
                    .choices
                    .boards
                    .iter()
                    .find(|b| b.0 == id)
                    .map(|b| b.1.clone());
                RowTo::Board(id, name)
            }
            LinkTo::Query { sql, view } => RowTo::Query {
                sql: cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .auto_grow(2, 6)
                        .placeholder("select …")
                        .default_value(sql)
                }),
                view,
            },
        };
        self.links.push(LinkRow { label, to });
        self.error = None;
        cx.notify();
    }

    /// The details, or `None` (and why, in the form) if they don't fit.
    pub fn submit(&mut self, cx: &mut Context<Self>) -> Option<Details> {
        let name = self.name.read(cx).value().trim().to_string();
        let links: Vec<Link> = self
            .links
            .iter()
            .map(|row| Link {
                label: row.label.read(cx).value().trim().to_string(),
                to: match &row.to {
                    RowTo::Url(url) => LinkTo::Url(url.read(cx).value().trim().to_string()),
                    RowTo::Bookmark(id, _) => LinkTo::Bookmark(id.clone()),
                    RowTo::Board(id, _) => LinkTo::Board(id.clone()),
                    RowTo::Query { sql, view } => LinkTo::Query {
                        sql: sql.read(cx).value().trim().to_string(),
                        view: *view,
                    },
                },
            })
            .collect();
        self.error = if name.is_empty() {
            Some("Name is required".into())
        } else if links.iter().any(|l| l.label.is_empty()) {
            Some("Every link needs a label".into())
        } else {
            links.iter().find(|l| !l.is_valid()).map(|l| match &l.to {
                LinkTo::Url(_) => format!("{}: not a web address (https://…)", l.label).into(),
                _ => format!("{}: needs a query", l.label).into(),
            })
        };
        cx.notify();
        if self.error.is_some() {
            return None;
        }
        Some(Details {
            name,
            description: self.description.read(cx).value().trim().to_string(),
            links,
        })
    }

    /// The "Add link" menu: a web address, a query, or one of the choices.
    fn add_menu(
        this: WeakEntity<Self>,
        choices: &Choices,
        mut menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let item = |label: String, link: Link, this: &WeakEntity<Self>| {
            let this = this.clone();
            PopupMenuItem::new(label).on_click(move |_, window, cx| {
                let _ = this.update(cx, |f, cx| f.push(link.clone(), window, cx));
            })
        };
        menu = menu
            .item(item(
                "Web address".into(),
                Link {
                    label: String::new(),
                    to: LinkTo::Url(String::new()),
                },
                &this,
            ))
            .item(item(
                "Query".into(),
                Link {
                    label: String::new(),
                    to: LinkTo::Query {
                        sql: String::new(),
                        view: None,
                    },
                },
                &this,
            ));
        if !choices.bookmarks.is_empty() {
            let (bookmarks, this) = (choices.bookmarks.clone(), this.clone());
            menu = menu.submenu("Bookmark", window, cx, move |mut menu, _, _| {
                for (id, name, _) in &bookmarks {
                    let link = Link {
                        label: name.clone(),
                        to: LinkTo::Bookmark(id.clone()),
                    };
                    menu = menu.item(item(name.clone(), link, &this));
                }
                menu.max_h(px(320.)).scrollable(true)
            });
        }
        if !choices.boards.is_empty() {
            let (boards, this) = (choices.boards.clone(), this.clone());
            menu = menu.submenu("Board", window, cx, move |mut menu, _, _| {
                for (id, name) in &boards {
                    let link = Link {
                        label: name.clone(),
                        to: LinkTo::Board(id.clone()),
                    };
                    menu = menu.item(item(name.clone(), link, &this));
                }
                menu
            });
        }
        if !choices.history.is_empty() {
            let history = choices.history.clone();
            menu = menu.submenu("Recent query", window, cx, move |mut menu, _, _| {
                for sql in &history {
                    let line = one_line(sql, 60);
                    let link = Link {
                        label: String::new(),
                        to: LinkTo::Query {
                            sql: sql.clone(),
                            view: None,
                        },
                    };
                    menu = menu.item(item(line, link, &this));
                }
                menu.max_h(px(320.)).scrollable(true)
            });
        }
        menu
    }

    fn render_link(&self, ix: usize, row: &LinkRow, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let to = match &row.to {
            RowTo::Url(url) => LinkTo::Url(url.read(cx).value().to_string()),
            RowTo::Bookmark(id, _) => LinkTo::Bookmark(id.clone()),
            RowTo::Board(id, _) => LinkTo::Board(id.clone()),
            RowTo::Query { view, .. } => LinkTo::Query {
                sql: String::new(),
                view: *view,
            },
        };
        let target: AnyElement = match &row.to {
            RowTo::Url(url) => Input::new(url).small().into_any_element(),
            RowTo::Bookmark(_, name) | RowTo::Board(_, name) => {
                let text = match name {
                    Some(name) => format!("{}: {name}", link_action(&to)),
                    None => "It is gone; remove the link".to_string(),
                };
                div()
                    .text_xs()
                    .text_color(if name.is_some() { muted } else { theme.danger })
                    .child(text)
                    .into_any_element()
            }
            RowTo::Query { sql, view } => {
                let this = cx.entity().downgrade();
                let current = *view;
                v_flex()
                    .gap_1()
                    .child(Textarea::new(sql))
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .text_xs()
                            .text_color(muted)
                            .child("Shown as")
                            .child(
                                Button::new(("link-view", ix))
                                    .label(view_label(current))
                                    .ghost()
                                    .xsmall()
                                    .dropdown_menu(move |mut menu, _, _| {
                                        let views = std::iter::once(None).chain(
                                            ChartStyle::ALL.map(|style| {
                                                Some(ChartView {
                                                    style,
                                                    series: None,
                                                })
                                            }),
                                        );
                                        for view in views {
                                            let this = this.clone();
                                            menu = menu.item(
                                                PopupMenuItem::new(view_label(view))
                                                    .checked(view == current)
                                                    .on_click(move |_, _, cx| {
                                                        let _ = this.update(cx, |f, cx| {
                                                            if let Some(RowTo::Query {
                                                                view: v,
                                                                ..
                                                            }) = f
                                                                .links
                                                                .get_mut(ix)
                                                                .map(|r| &mut r.to)
                                                            {
                                                                *v = view;
                                                            }
                                                            cx.notify();
                                                        });
                                                    }),
                                            );
                                        }
                                        menu
                                    }),
                            ),
                    )
                    .into_any_element()
            }
        };
        v_flex()
            .gap_1()
            .p_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(Icon::new(link_icon(&to)).xsmall().text_color(muted))
                    .child(div().flex_1().child(Input::new(&row.label).small()))
                    .child(
                        Button::new(("link-remove", ix))
                            .icon(IconName::Delete)
                            .ghost()
                            .xsmall()
                            .tooltip("Remove link")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if ix < this.links.len() {
                                    this.links.remove(ix);
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(target)
            .into_any_element()
    }
}

/// "Table", "Bars side by side", …
fn view_label(view: Option<ChartView>) -> &'static str {
    match view {
        None => "Table",
        Some(view) => view.style.label(),
    }
}

impl Render for DetailsForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label = |text: &'static str, cx: &App| {
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(text)
        };
        let links: Vec<AnyElement> = self
            .links
            .iter()
            .enumerate()
            .map(|(i, row)| self.render_link(i, row, cx))
            .collect();
        let (this, choices) = (cx.entity().downgrade(), self.choices.clone());
        let danger = cx.theme().danger;
        v_flex()
            .gap_2()
            .child(label("Name", cx))
            .child(Input::new(&self.name))
            .child(label("Description", cx))
            .child(Textarea::new(&self.description))
            .child(label("Links", cx))
            .child(
                v_flex()
                    .id("board-links")
                    .test_support()
                    .max_h(px(300.))
                    .overflow_y_scroll()
                    .gap_1()
                    .children(links),
            )
            .child(
                h_flex().child(
                    Button::new("add-link")
                        .icon(IconName::Plus)
                        .label("Add link")
                        .ghost()
                        .xsmall()
                        .dropdown_menu(move |menu, window, cx| {
                            Self::add_menu(this.clone(), &choices, menu, window, cx)
                        }),
                ),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_sm().text_color(danger).child(error))
            })
    }
}

pub struct TileForm {
    title: Entity<InputState>,
    description: Entity<TextareaState>,
}

impl TileForm {
    pub fn new(tile: &Tile, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Title")
                .default_value(tile.title.clone())
        });
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder("What it shows")
                .default_value(tile.description.clone())
        });
        window.focus(&title.read(cx).focus_handle(cx), cx);
        Self { title, description }
    }

    /// Title and description.
    pub fn submit(&self, cx: &App) -> (String, String) {
        (
            self.title.read(cx).value().trim().to_string(),
            self.description.read(cx).value().trim().to_string(),
        )
    }
}

impl Render for TileForm {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .child(Input::new(&self.title))
            .child(Textarea::new(&self.description))
    }
}

/// Where "Add to board" puts the tile.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Board(String),
    /// A new board of this name.
    New(String),
}

/// "Add to board": the title, as a tile (its result on the board) or a link (opens
/// it in a tab), and the board (one of the database's, or a new one). Without
/// anything to add only a new board's name ("New board").
pub struct AddForm {
    title: Entity<InputState>,
    with_tile: bool,
    as_link: bool,
    /// The database's boards as (id, name).
    boards: Vec<(String, String)>,
    /// Index into `boards`; `None` is a new board.
    chosen: Option<usize>,
    new_name: Entity<InputState>,
    error: Option<SharedString>,
}

impl AddForm {
    pub fn new(
        title: &str,
        boards: Vec<(String, String)>,
        with_tile: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Title")
                .default_value(title.to_string())
        });
        let new_name = cx.new(|cx| InputState::new(window, cx).placeholder("New board's name"));
        let chosen = (with_tile && !boards.is_empty()).then_some(0);
        let focus = match chosen {
            Some(_) => title.read(cx).focus_handle(cx),
            None => new_name.read(cx).focus_handle(cx),
        };
        window.focus(&focus, cx);
        Self {
            title,
            with_tile,
            as_link: false,
            boards,
            chosen,
            new_name,
            error: None,
        }
    }

    /// The title, where it goes and whether as a link, or `None` (and why, in the
    /// form).
    pub fn submit(&mut self, cx: &mut Context<Self>) -> Option<(String, Target, bool)> {
        let title = self.title.read(cx).value().trim().to_string();
        let target = match self.chosen.and_then(|i| self.boards.get(i)) {
            Some((id, _)) => Target::Board(id.clone()),
            None => {
                let name = self.new_name.read(cx).value().trim().to_string();
                if name.is_empty() {
                    self.error = Some("Name the new board".into());
                    cx.notify();
                    return None;
                }
                Target::New(name)
            }
        };
        if self.as_link && title.is_empty() {
            self.error = Some("A link needs a title".into());
            cx.notify();
            return None;
        }
        Some((title, target, self.as_link))
    }
}

impl Render for AddForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let option = |ix: usize, label: String, chosen: bool, pick: Option<usize>| {
            div()
                .id(("add-to-board", ix))
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .text_sm()
                .cursor_pointer()
                .when(chosen, |this| this.bg(theme.secondary))
                .hover(|s| s.bg(theme.secondary))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.chosen = pick;
                    this.error = None;
                    cx.notify();
                }))
        };
        let new_ix = self.boards.len();
        if !self.with_tile {
            return v_flex()
                .gap_2()
                .child(Input::new(&self.new_name))
                .when_some(self.error.clone(), |this, error| {
                    this.child(div().text_sm().text_color(theme.danger).child(error))
                });
        }
        let kind = |id: &'static str, label: &'static str, link: bool| {
            Button::new(id)
                .label(label)
                .xsmall()
                .ghost()
                .selected(self.as_link == link)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.as_link = link;
                    cx.notify();
                }))
        };
        let hint = if self.as_link {
            "A link on top of the board opens it in a new tab."
        } else {
            "A tile shows its result on the board."
        };
        v_flex()
            .gap_2()
            .child(Input::new(&self.title))
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(kind("add-as-tile", "As a tile", false))
                    .child(kind("add-as-link", "As a link", true))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(hint),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Board"),
            )
            .child(
                v_flex()
                    .gap_0p5()
                    .children(self.boards.iter().enumerate().map(|(i, (_, name))| {
                        option(i, name.clone(), self.chosen == Some(i), Some(i))
                    }))
                    .child(option(
                        new_ix,
                        "New board…".into(),
                        self.chosen.is_none(),
                        None,
                    )),
            )
            .when(self.chosen.is_none(), |this| {
                this.child(Input::new(&self.new_name))
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_sm().text_color(theme.danger).child(error))
            })
    }
}
