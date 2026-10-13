//! Tachyon Download Manager: a native macOS download accelerator inspired by
//! Internet Download Manager, written in Rust with GPUI.

mod add_dialog;
mod app;
mod assets;
mod complete_dialog;
mod format;
mod main_window;
mod options_window;
mod post_action;
mod progress_window;
mod scheduler_window;
#[cfg(feature = "dev-snapshots")]
mod snapshot;
mod theme;
mod widgets;

use std::sync::Arc;

use gpui::{App, Application, KeyBinding, Menu, MenuItem, SystemMenuType, actions};
use tachyon_engine::Engine;

use crate::add_dialog::AddPrefill;

actions!(
    tachyon,
    [
        Quit,
        About,
        AddUrl,
        ShowOptions,
        ShowScheduler,
        ShowMainWindow,
        StartQueue,
        StopQueue,
        StopAll,
        DeleteCompleted,
        CloseWindow,
    ]
);

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // TACHYON_DATA_DIR keeps development runs away from the real download list.
    let opened = match std::env::var_os("TACHYON_DATA_DIR") {
        Some(dir) => Engine::open(dir.into()),
        None => Engine::open_default(),
    };
    let engine = match opened {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            eprintln!("Tachyon could not start its download engine: {e:#}");
            std::process::exit(1);
        }
    };

    let app = Application::new().with_assets(assets::Assets);
    app.on_reopen(app::open_main_window);
    app.run(move |cx: &mut App| {
        app::init(engine.clone(), cx);
        widgets::text_input::bind_keys(cx);
        main_window::bind_keys(cx);
        register_actions(cx);
        cx.set_menus(menus());

        let quit_engine = engine.clone();
        cx.on_app_quit(move |_| {
            quit_engine.shutdown();
            async {}
        })
        .detach();

        #[cfg(feature = "dev-snapshots")]
        snapshot::init(cx);

        app::open_main_window(cx);
    });
}

fn register_actions(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &About, cx| {
        app::open_main_window(cx);
        main_window::show_about(cx);
    });
    cx.on_action(|_: &AddUrl, cx| app::open_add_dialog(AddPrefill::FromClipboard, cx));
    cx.on_action(|_: &ShowOptions, cx| app::open_options(cx));
    cx.on_action(|_: &ShowScheduler, cx| app::open_scheduler(cx));
    cx.on_action(|_: &ShowMainWindow, cx| app::open_main_window(cx));
    cx.on_action(|_: &StartQueue, cx| app::engine(cx).start_queue());
    cx.on_action(|_: &StopQueue, cx| app::engine(cx).stop_queue());
    cx.on_action(|_: &StopAll, cx| app::engine(cx).pause_all());
    cx.on_action(|_: &DeleteCompleted, cx| {
        app::engine(cx).remove_completed();
    });
    cx.on_action(|_: &CloseWindow, cx| {
        if let Some(window) = cx.active_window() {
            window.update(cx, |_, window, _| window.remove_window()).ok();
        }
    });

    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-n", AddUrl, None),
        KeyBinding::new("cmd-,", ShowOptions, None),
        KeyBinding::new("cmd-shift-s", ShowScheduler, None),
        KeyBinding::new("cmd-0", ShowMainWindow, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
    ]);
}

fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "Tachyon".into(),
            items: vec![
                MenuItem::action("About Tachyon", About),
                MenuItem::separator(),
                MenuItem::action("Options…", ShowOptions),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Quit Tachyon", Quit),
            ],
        },
        Menu {
            name: "Downloads".into(),
            items: vec![
                MenuItem::action("Add URL…", AddUrl),
                MenuItem::separator(),
                MenuItem::action("Stop All", StopAll),
                MenuItem::action("Delete All Completed", DeleteCompleted),
                MenuItem::separator(),
                MenuItem::action("Start Queue", StartQueue),
                MenuItem::action("Stop Queue", StopQueue),
                MenuItem::action("Scheduler…", ShowScheduler),
            ],
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Downloads", ShowMainWindow),
                MenuItem::action("Close Window", CloseWindow),
            ],
        },
    ]
}
