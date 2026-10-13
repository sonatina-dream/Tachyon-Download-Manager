//! App-wide state and the functions that open each window.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, AppContext, Bounds, Global, Pixels, Size, TitlebarOptions, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, point, px, size,
};
use tachyon_engine::{DownloadId, Engine, EngineEvent, PostAction, looks_downloadable};

use crate::add_dialog::{AddDialog, AddPrefill};
use crate::complete_dialog::CompleteDialog;
use crate::main_window::MainWindow;
use crate::options_window::OptionsWindow;
use crate::post_action::PostActionDialog;
use crate::progress_window::ProgressWindow;
use crate::scheduler_window::SchedulerWindow;

pub struct AppState {
    pub engine: Arc<Engine>,
    main_window: Option<WindowHandle<MainWindow>>,
    progress_windows: HashMap<DownloadId, WindowHandle<ProgressWindow>>,
    options_window: Option<WindowHandle<OptionsWindow>>,
    scheduler_window: Option<WindowHandle<SchedulerWindow>>,
    last_clipboard: Option<String>,
}

impl Global for AppState {}

pub fn engine(cx: &App) -> Arc<Engine> {
    cx.global::<AppState>().engine.clone()
}

pub fn init(engine: Arc<Engine>, cx: &mut App) {
    let last_clipboard = cx.read_from_clipboard().and_then(|c| c.text());
    cx.set_global(AppState {
        engine,
        main_window: None,
        progress_windows: HashMap::new(),
        options_window: None,
        scheduler_window: None,
        last_clipboard,
    });
    cx.spawn(async move |cx| {
        loop {
            gpui::Timer::after(Duration::from_millis(400)).await;
            if cx.update(tick).is_err() {
                break;
            }
        }
    })
    .detach();
}

/// Reacts to engine events and watches the clipboard.
fn tick(cx: &mut App) {
    let engine = engine(cx);
    for event in engine.drain_events() {
        match event {
            EngineEvent::Completed(id) => on_completed(id, cx),
            EngineEvent::Failed { .. } => {}
            EngineEvent::Captured(request) => {
                open_add_dialog(AddPrefill::Captured(request), cx);
            }
            EngineEvent::QueueFinished { post_action } => run_post_action(post_action, cx),
            EngineEvent::PostAction(action) => run_post_action(action, cx),
        }
    }

    if engine.settings().monitor_clipboard {
        let text = cx.read_from_clipboard().and_then(|c| c.text());
        let state = cx.global_mut::<AppState>();
        if text != state.last_clipboard {
            state.last_clipboard = text.clone();
            if let Some(url) = text.map(|t| t.trim().to_string())
                && looks_downloadable(&url, &engine.settings().categories)
            {
                open_add_dialog(AddPrefill::Url(url), cx);
            }
        }
    }
}

fn on_completed(id: DownloadId, cx: &mut App) {
    let engine = engine(cx);
    let Some(download) = engine.get(id) else { return };
    if let Some(handle) = cx.global_mut::<AppState>().progress_windows.remove(&id) {
        handle.update(cx, |_, window, _| window.remove_window()).ok();
    }
    if download.completion.open_file {
        cx.open_with_system(&download.path());
    }
    if download.completion.show_dialog && engine.settings().show_complete_dialog {
        open_complete_dialog(id, cx);
    }
}

pub fn run_post_action(action: PostAction, cx: &mut App) {
    if action == PostAction::Nothing {
        return;
    }
    let bounds = centered(size(px(380.), px(170.)), cx);
    cx.open_window(dialog_options("Downloads Finished", bounds), |window, cx| {
        cx.new(|cx| PostActionDialog::new(action, window, cx))
    })
    .ok();
    cx.activate(true);
}

pub fn centered(size: Size<Pixels>, cx: &App) -> Bounds<Pixels> {
    Bounds::centered(None, size, cx)
}

pub fn dialog_options(title: &str, bounds: Bounds<Pixels>) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(title.to_string().into()),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        focus: true,
        show: true,
        kind: WindowKind::Normal,
        is_resizable: false,
        is_minimizable: true,
        window_min_size: Some(bounds.size),
        ..Default::default()
    }
}

/// Brings an existing window forward; returns false if it was closed.
fn activate<T: 'static>(handle: Option<WindowHandle<T>>, cx: &mut App) -> bool {
    handle.is_some_and(|h| h.update(cx, |_, window, _| window.activate_window()).is_ok())
}

pub fn open_main_window(cx: &mut App) {
    if activate(cx.global::<AppState>().main_window, cx) {
        return;
    }
    let bounds = centered(size(px(1120.), px(680.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Tachyon Download Manager".into()),
            appears_transparent: false,
            traffic_light_position: Some(point(px(12.), px(12.))),
        }),
        window_min_size: Some(size(px(760.), px(420.))),
        focus: true,
        ..Default::default()
    };
    if let Ok(handle) = cx.open_window(options, |window, cx| cx.new(|cx| MainWindow::new(window, cx))) {
        cx.global_mut::<AppState>().main_window = Some(handle);
    }
    cx.activate(true);
}

pub fn open_add_dialog(prefill: AddPrefill, cx: &mut App) {
    let bounds = centered(size(px(560.), px(400.)), cx);
    cx.open_window(dialog_options("Add Download", bounds), |window, cx| {
        cx.new(|cx| AddDialog::new(prefill, window, cx))
    })
    .ok();
    cx.activate(true);
}

pub const PROGRESS_SIZE: (f32, f32) = (540., 360.);
pub const PROGRESS_SIZE_EXPANDED: (f32, f32) = (540., 600.);

pub fn open_progress_window(id: DownloadId, cx: &mut App) {
    let existing = cx.global::<AppState>().progress_windows.get(&id).copied();
    if activate(existing, cx) {
        return;
    }
    let bounds = centered(size(px(PROGRESS_SIZE.0), px(PROGRESS_SIZE.1)), cx);
    let mut options = dialog_options("Download", bounds);
    options.focus = false;
    if let Ok(handle) = cx.open_window(options, |window, cx| cx.new(|cx| ProgressWindow::new(id, window, cx))) {
        cx.global_mut::<AppState>().progress_windows.insert(id, handle);
    }
}

pub fn forget_progress_window(id: DownloadId, cx: &mut App) {
    cx.global_mut::<AppState>().progress_windows.remove(&id);
}

pub fn open_complete_dialog(id: DownloadId, cx: &mut App) {
    let bounds = centered(size(px(520.), px(270.)), cx);
    cx.open_window(dialog_options("Download Complete", bounds), |window, cx| {
        cx.new(|cx| CompleteDialog::new(id, window, cx))
    })
    .ok();
    cx.activate(true);
}

pub fn open_options(cx: &mut App) {
    if activate(cx.global::<AppState>().options_window, cx) {
        return;
    }
    let bounds = centered(size(px(640.), px(520.)), cx);
    if let Ok(handle) = cx.open_window(dialog_options("Options", bounds), |window, cx| {
        cx.new(|cx| OptionsWindow::new(window, cx))
    }) {
        cx.global_mut::<AppState>().options_window = Some(handle);
    }
    cx.activate(true);
}

pub fn open_scheduler(cx: &mut App) {
    if activate(cx.global::<AppState>().scheduler_window, cx) {
        return;
    }
    let bounds = centered(size(px(620.), px(620.)), cx);
    if let Ok(handle) = cx.open_window(dialog_options("Scheduler", bounds), |window, cx| {
        cx.new(|cx| SchedulerWindow::new(window, cx))
    }) {
        cx.global_mut::<AppState>().scheduler_window = Some(handle);
    }
    cx.activate(true);
}

/// The bundled browser extension, for the Options window's "Show extension" button.
pub fn extension_dir() -> Option<PathBuf> {
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join("../Resources/extension")));
    let source = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../extension"));
    [bundled, Some(source)]
        .into_iter()
        .flatten()
        .find(|p| p.join("manifest.json").exists())
        .and_then(|p| p.canonicalize().ok())
}
