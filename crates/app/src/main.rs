mod assets;
mod bookmark_form;
mod chart;
mod completion;
mod connection;
mod connection_form;
mod file_tab;
mod monitor;
mod palette;
mod query_router;
mod query_tab;
mod result_view;
mod settings;
mod tabs;
mod ui;
mod workspace;

use gpui_kit::component::{Root, Theme, ThemeMode, TitleBar};
use gpui_kit::*;

actions!(
    tangul,
    [
        RunQuery,
        RunAll,
        FormatQuery,
        InspectValue,
        CloseValueViewer,
        ToggleRightSidebar,
        BookmarkQuery,
        SaveFile,
        NewTab,
        CloseTab,
        ToggleSidebar,
        OpenPalette,
        OpenSettings,
        NextSidebarPanel,
        PrevSidebarPanel,
        Quit
    ]
);

fn main() {
    // The engine owns its own tokio runtime thread; the UI only exchanges messages.
    let (engine, events) = tangul_engine::start();

    let app = application().with_assets(assets::AppAssets);
    // macOS keeps the app running when its window closes; clicking the Dock icon
    // then asks for a window again.
    {
        let (engine, events) = (engine.clone(), events.clone());
        app.on_reopen(move |cx| {
            if cx.windows().is_empty() {
                open_main_window(engine.clone(), events.clone(), cx);
            }
        });
    }
    app.run(move |cx| {
        init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        settings::init(cx);

        bind_keys(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());

        open_main_window(engine, events, cx);
        cx.activate(true);
    });
}

/// The app's key bindings (also used by the UI tests).
fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        // Must win over the editor's own `secondary-enter` (newline), hence the
        // same depth ("Input") and registration after gpui_kit::init.
        KeyBinding::new("secondary-enter", RunQuery, Some("QueryEditor > Input")),
        KeyBinding::new("secondary-shift-enter", RunAll, Some("QueryEditor > Input")),
        KeyBinding::new("secondary-d", BookmarkQuery, Some("QueryEditor > Input")),
        // DataGrip's "Reformat code".
        KeyBinding::new("alt-secondary-l", FormatQuery, Some("QueryEditor > Input")),
        KeyBinding::new("secondary-s", SaveFile, Some("QueryEditor > Input")),
        KeyBinding::new("secondary-t", NewTab, Some("Workspace")),
        KeyBinding::new("secondary-w", CloseTab, Some("Workspace")),
        KeyBinding::new("secondary-b", ToggleSidebar, Some("Workspace")),
        KeyBinding::new("alt-secondary-b", ToggleRightSidebar, Some("Workspace")),
        // On a result: open the value viewer on the selected cell, or close it (like
        // Quick Look).
        KeyBinding::new("space", InspectValue, Some("DataTable")),
        // Before the table's own Esc (clears the selection), which runs when the
        // viewer is closed. Popovers, inputs and dialogs inside are deeper and
        // close first.
        KeyBinding::new("escape", CloseValueViewer, Some("DataTable")),
        KeyBinding::new("escape", CloseValueViewer, Some("ValueViewer")),
        KeyBinding::new("secondary-shift-p", OpenPalette, Some("Workspace")),
        KeyBinding::new("secondary-,", OpenSettings, Some("Workspace")),
        // In the command palette: switch the sidebar panel.
        KeyBinding::new("tab", NextSidebarPanel, Some("Palette")),
        KeyBinding::new("shift-tab", PrevSidebarPanel, Some("Palette")),
        KeyBinding::new("secondary-q", Quit, None),
    ]);
}

/// Opens the workspace window. Only one exists at a time: the engine's event stream
/// goes to the workspace that is alive.
fn open_main_window(
    engine: tangul_engine::EngineHandle,
    events: async_channel::Receiver<tangul_engine::Event>,
    cx: &mut App,
) {
    let options = WindowOptions {
        // The title is not drawn (custom title bar) but names the window in the
        // Dock's and the Window menu's window list.
        titlebar: Some(TitlebarOptions {
            title: Some("tangul".into()),
            ..TitleBar::title_bar_options()
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1280.), px(800.)),
            cx,
        ))),
        ..Default::default()
    };
    cx.spawn(async move |cx| {
        cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| {
                workspace::Workspace::new(engine, events, tangul_data::Data::open(), window, cx)
            });
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("failed to open window");
    })
    .detach();
}
