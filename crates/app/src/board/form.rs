//! The board's details (name, description, links) and a tile's title and
//! description, edited in dialogs.

use gpui_kit::component::{
    ActiveTheme as _,
    input::{Input, InputState, Textarea, TextareaState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_core::board::{Board, Link, Tile};
use tangul_service::boards::{links_text, parse_links};

/// What the details form gives back.
pub struct Details {
    pub name: String,
    pub description: String,
    pub links: Vec<Link>,
}

pub struct DetailsForm {
    name: Entity<InputState>,
    description: Entity<TextareaState>,
    links: Entity<TextareaState>,
    error: Option<SharedString>,
}

impl DetailsForm {
    pub fn new(board: &Board, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
        let links = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder("Epic https://…")
                .default_value(links_text(&board.links))
        });
        window.focus(&name.read(cx).focus_handle(cx), cx);
        Self {
            name,
            description,
            links,
            error: None,
        }
    }

    /// The details, or `None` (and why, in the form) if they don't fit.
    pub fn submit(&mut self, cx: &mut Context<Self>) -> Option<Details> {
        let name = self.name.read(cx).value().trim().to_string();
        let links = parse_links(&self.links.read(cx).value());
        self.error = if name.is_empty() {
            Some("Name is required".into())
        } else {
            links
                .iter()
                .find(|l| !l.is_web())
                .map(|l| format!("Not a web address: {}", l.url).into())
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
}

impl Render for DetailsForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let label = |text: &'static str| {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        v_flex()
            .gap_2()
            .child(label("Name"))
            .child(Input::new(&self.name))
            .child(label("Description"))
            .child(Textarea::new(&self.description))
            .child(label("Links, one per line: a label, then the address"))
            .child(Textarea::new(&self.links))
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_sm().text_color(theme.danger).child(error))
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

/// "Add to board": the tile's title, and the board (one of the database's, or a
/// new one). Without a tile only a new board's name ("New board").
pub struct AddForm {
    title: Entity<InputState>,
    with_tile: bool,
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
                .placeholder("Tile title")
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
            boards,
            chosen,
            new_name,
            error: None,
        }
    }

    /// The tile's title and where it goes, or `None` (and why, in the form).
    pub fn submit(&mut self, cx: &mut Context<Self>) -> Option<(String, Target)> {
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
        Some((title, target))
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
        v_flex()
            .gap_2()
            .child(Input::new(&self.title))
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
