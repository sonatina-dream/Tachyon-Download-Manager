use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext, ClipboardItem, Context, FocusHandle, Focusable, FontWeight,
    KeyBinding, MouseButton, MouseDownEvent, Pixels, Point, PromptLevel, SharedString, Task,
    UniformListScrollHandle, Window, WindowBounds, actions, anchored, deferred, div, prelude::*, px,
    size, uniform_list,
};
use tachyon_engine::{DownloadId, DownloadSnapshot, DownloadStatus, Engine, GENERAL};

use crate::add_dialog::AddPrefill;
use crate::app;
use crate::assets::Icon;
use crate::format;
use crate::theme::Theme;
use crate::widgets::{Button, ToolbarButton, icon, progress_bar, separator, truncated};

actions!(
    main_window,
    [ResumeSelected, StopSelected, DeleteSelected, SelectAllRows, OpenSelected, RevealSelected]
);

const CONTEXT: &str = "MainWindow";
const ROW_HEIGHT: f32 = 30.;
const SIDEBAR_WIDTH: f32 = 200.;

pub fn bind_keys(cx: &mut App) {
    let ctx = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("backspace", DeleteSelected, ctx),
        KeyBinding::new("delete", DeleteSelected, ctx),
        KeyBinding::new("cmd-a", SelectAllRows, ctx),
        KeyBinding::new("enter", OpenSelected, ctx),
        KeyBinding::new("cmd-o", OpenSelected, ctx),
        KeyBinding::new("cmd-r", ResumeSelected, ctx),
        KeyBinding::new("cmd-.", StopSelected, ctx),
        KeyBinding::new("cmd-shift-r", RevealSelected, ctx),
    ]);
}

#[derive(Clone, PartialEq, Eq)]
enum Filter {
    All,
    Category(String),
    Unfinished,
    Finished,
    Queue,
}

impl Filter {
    fn matches(&self, d: &DownloadSnapshot) -> bool {
        match self {
            Self::All => true,
            Self::Category(c) => &d.category == c,
            Self::Unfinished => d.status != DownloadStatus::Completed,
            Self::Finished => d.status == DownloadStatus::Completed,
            Self::Queue => d.in_queue,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Column {
    Name,
    Queue,
    Size,
    Status,
    TimeLeft,
    Rate,
    LastTry,
    Description,
}

/// IDM's main list columns. `None` width means the column takes the remaining space.
const COLUMNS: [(Column, &str, Option<f32>); 8] = [
    (Column::Name, "File Name", None),
    (Column::Queue, "Q", Some(30.)),
    (Column::Size, "Size", Some(86.)),
    (Column::Status, "Status", Some(112.)),
    (Column::TimeLeft, "Time Left", Some(80.)),
    (Column::Rate, "Transfer Rate", Some(104.)),
    (Column::LastTry, "Last Try Date", Some(142.)),
    (Column::Description, "Description", Some(150.)),
];

fn compare(column: Column, a: &DownloadSnapshot, b: &DownloadSnapshot) -> Ordering {
    match column {
        Column::Name => a.file_name.to_lowercase().cmp(&b.file_name.to_lowercase()),
        Column::Queue => (a.in_queue, a.queue_order).cmp(&(b.in_queue, b.queue_order)),
        Column::Size => a.size.cmp(&b.size),
        Column::Status => a
            .progress()
            .unwrap_or(0.0)
            .partial_cmp(&b.progress().unwrap_or(0.0))
            .unwrap_or(Ordering::Equal),
        Column::TimeLeft => a.eta_secs.cmp(&b.eta_secs),
        Column::Rate => a.speed.cmp(&b.speed),
        Column::LastTry => a.last_try.cmp(&b.last_try),
        Column::Description => a.description.cmp(&b.description),
    }
}

pub(crate) struct ContextMenu {
    pub(crate) position: Point<Pixels>,
}

pub struct MainWindow {
    engine: Arc<Engine>,
    focus_handle: FocusHandle,
    downloads: Vec<DownloadSnapshot>,
    /// Indices into `downloads`, filtered and sorted.
    rows: Vec<usize>,
    filter: Filter,
    sort: (Column, bool),
    selected: BTreeSet<DownloadId>,
    anchor: Option<DownloadId>,
    scroll: UniformListScrollHandle,
    pub(crate) context_menu: Option<ContextMenu>,
    categories_open: bool,
    queue_running: bool,
    total_speed: u64,
    _poll: Task<()>,
}

impl MainWindow {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                gpui::Timer::after(Duration::from_millis(250)).await;
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
            }
        });
        cx.observe_window_appearance(window, |_, _, cx| cx.notify()).detach();
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
        let mut this = Self {
            engine: app::engine(cx),
            focus_handle,
            downloads: Vec::new(),
            rows: Vec::new(),
            filter: Filter::All,
            sort: (Column::LastTry, false),
            selected: BTreeSet::new(),
            anchor: None,
            scroll: UniformListScrollHandle::new(),
            context_menu: None,
            categories_open: true,
            queue_running: false,
            total_speed: 0,
            _poll: poll,
        };
        this.refresh(cx);
        this
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.downloads = self.engine.snapshot();
        self.queue_running = self.engine.is_queue_running();
        self.total_speed = self.engine.total_speed();
        let ids: BTreeSet<DownloadId> = self.downloads.iter().map(|d| d.id).collect();
        self.selected.retain(|id| ids.contains(id));
        self.rebuild_rows();
        cx.notify();
    }

    fn rebuild_rows(&mut self) {
        let (column, ascending) = self.sort;
        let mut rows: Vec<usize> = (0..self.downloads.len())
            .filter(|&i| self.filter.matches(&self.downloads[i]))
            .collect();
        rows.sort_by(|&a, &b| {
            let (a, b) = (&self.downloads[a], &self.downloads[b]);
            let ord = compare(column, a, b).then(a.added_at.cmp(&b.added_at)).then(a.id.cmp(&b.id));
            if ascending { ord } else { ord.reverse() }
        });
        self.rows = rows;
    }

    fn download(&self, id: DownloadId) -> Option<&DownloadSnapshot> {
        self.downloads.iter().find(|d| d.id == id)
    }

    fn selected_downloads(&self) -> Vec<&DownloadSnapshot> {
        self.downloads.iter().filter(|d| self.selected.contains(&d.id)).collect()
    }

    fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.filter = filter;
        self.selected.clear();
        self.rebuild_rows();
        cx.notify();
    }

    fn set_sort(&mut self, column: Column, cx: &mut Context<Self>) {
        self.sort = if self.sort.0 == column { (column, !self.sort.1) } else { (column, true) };
        self.rebuild_rows();
        cx.notify();
    }

    // Selection

    fn on_row_mouse_down(&mut self, id: DownloadId, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        self.context_menu = None;
        if event.modifiers.platform {
            if !self.selected.remove(&id) {
                self.selected.insert(id);
            }
            self.anchor = Some(id);
        } else if event.modifiers.shift
            && let Some(anchor) = self.anchor
        {
            let position = |target| self.rows.iter().position(|&r| self.downloads[r].id == target);
            if let (Some(a), Some(b)) = (position(anchor), position(id)) {
                let (lo, hi) = (a.min(b), a.max(b));
                self.selected = self.rows[lo..=hi].iter().map(|&r| self.downloads[r].id).collect();
            }
        } else {
            self.selected = BTreeSet::from([id]);
            self.anchor = Some(id);
            if event.click_count == 2 {
                self.open(id, cx);
            }
        }
        cx.notify();
    }

    fn on_row_right_click(&mut self, id: DownloadId, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        if !self.selected.contains(&id) {
            self.selected = BTreeSet::from([id]);
            self.anchor = Some(id);
        }
        self.context_menu = Some(ContextMenu { position: event.position });
        cx.notify();
    }

    pub(crate) fn select_all(&mut self, _: &SelectAllRows, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = self.rows.iter().map(|&r| self.downloads[r].id).collect();
        cx.notify();
    }

    // Commands

    fn open(&mut self, id: DownloadId, cx: &mut Context<Self>) {
        let Some(d) = self.download(id) else { return };
        if d.status == DownloadStatus::Completed {
            cx.open_with_system(&d.path());
        } else {
            app::open_progress_window(id, cx);
        }
    }

    fn open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<_> = self.selected.iter().copied().collect();
        for id in ids {
            self.open(id, cx);
        }
    }

    fn resume(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<_> = self
            .selected_downloads()
            .iter()
            .filter(|d| !d.status.is_active() && d.status != DownloadStatus::Completed)
            .map(|d| d.id)
            .collect();
        let show_progress = self.engine.settings().show_progress_window && ids.len() == 1;
        for &id in &ids {
            if let Err(e) = self.engine.start(id) {
                log::warn!("cannot resume download {id}: {e}");
            }
        }
        if show_progress {
            app::open_progress_window(ids[0], cx);
        }
        self.refresh(cx);
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        for id in self.selected.clone() {
            self.engine.pause(id);
        }
        self.refresh(cx);
    }

    fn redownload(&mut self, cx: &mut Context<Self>) {
        for id in self.selected.clone() {
            if let Err(e) = self.engine.redownload(id) {
                log::warn!("cannot redownload {id}: {e}");
            }
        }
        self.refresh(cx);
    }

    fn reveal(&mut self, cx: &mut Context<Self>) {
        if let Some(d) = self.selected_downloads().first() {
            let path = if d.status == DownloadStatus::Completed && d.path().exists() {
                d.path()
            } else {
                d.directory.clone()
            };
            cx.reveal_path(&path);
        }
    }

    fn copy_address(&mut self, cx: &mut Context<Self>) {
        let urls: Vec<String> = self.selected_downloads().iter().map(|d| d.url.clone()).collect();
        if !urls.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(urls.join("\n")));
        }
    }

    fn set_queued(&mut self, queued: bool, cx: &mut Context<Self>) {
        let ids: Vec<_> = self.selected.iter().copied().collect();
        self.engine.set_queued(&ids, queued);
        self.refresh(cx);
    }

    fn move_in_queue(&mut self, up: bool, cx: &mut Context<Self>) {
        if let Some(&id) = self.selected.iter().next() {
            self.engine.move_in_queue(id, up);
            self.refresh(cx);
        }
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<DownloadId> = self.selected.iter().copied().collect();
        if ids.is_empty() {
            return;
        }
        let any_completed = self
            .selected_downloads()
            .iter()
            .any(|d| d.status == DownloadStatus::Completed);
        let message = if ids.len() == 1 {
            format!("Remove “{}” from the list?", self.download(ids[0]).map(|d| d.file_name.as_str()).unwrap_or(""))
        } else {
            format!("Remove {} downloads from the list?", ids.len())
        };
        let detail = "Unfinished downloads lose the data received so far.";
        let answers: &[&str] = if any_completed {
            &["Remove", "Remove and Delete Files", "Cancel"]
        } else {
            &["Remove", "Cancel"]
        };
        let answer = window.prompt(PromptLevel::Warning, &message, Some(detail), answers, cx);
        let engine = self.engine.clone();
        cx.spawn(async move |this, cx| {
            let Ok(choice) = answer.await else { return };
            let delete_files = match (any_completed, choice) {
                (_, 0) => false,
                (true, 1) => true,
                _ => return,
            };
            engine.remove(&ids, delete_files);
            this.update(cx, |this, cx| {
                this.selected.clear();
                this.refresh(cx);
            })
            .ok();
        })
        .detach();
    }

    fn delete_completed(&mut self, cx: &mut Context<Self>) {
        self.engine.remove_completed();
        self.refresh(cx);
    }

    // Rendering

    fn render_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let selection = self.selected_downloads();
        let can_resume = selection
            .iter()
            .any(|d| !d.status.is_active() && d.status != DownloadStatus::Completed);
        let can_stop = selection.iter().any(|d| d.status.is_active());
        let any_active = self.downloads.iter().any(|d| d.status.is_active());
        let any_completed = self.downloads.iter().any(|d| d.status == DownloadStatus::Completed);
        let divider = || div().w(px(1.)).h(px(36.)).mx(px(4.)).bg(theme.border);

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .px(px(10.))
            .py(px(6.))
            .bg(theme.chrome)
            .border_b_1()
            .border_color(theme.border)
            .child(
                ToolbarButton::new("tb-add", "Add URL", Icon::Add)
                    .tint(theme.accent)
                    .tooltip("Add a new download (⌘N)")
                    .on_click(|_, _, cx| app::open_add_dialog(AddPrefill::FromClipboard, cx)),
            )
            .child(divider())
            .child(
                ToolbarButton::new("tb-resume", "Resume", Icon::Play)
                    .tint(theme.success)
                    .disabled(!can_resume)
                    .on_click(cx.listener(|this, _, _, cx| this.resume(cx))),
            )
            .child(
                ToolbarButton::new("tb-stop", "Stop", Icon::Pause)
                    .disabled(!can_stop)
                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
            )
            .child(
                ToolbarButton::new("tb-stop-all", "Stop All", Icon::StopAll)
                    .disabled(!any_active && !self.queue_running)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.engine.pause_all();
                        this.refresh(cx);
                    })),
            )
            .child(divider())
            .child(
                ToolbarButton::new("tb-delete", "Delete", Icon::Trash)
                    .disabled(self.selected.is_empty())
                    .on_click(cx.listener(|this, _, window, cx| this.delete(window, cx))),
            )
            .child(
                ToolbarButton::new("tb-delete-completed", "Delete Done", Icon::TrashCheck)
                    .tooltip("Remove all completed downloads from the list")
                    .disabled(!any_completed)
                    .on_click(cx.listener(|this, _, _, cx| this.delete_completed(cx))),
            )
            .child(divider())
            .child(
                ToolbarButton::new("tb-options", "Options", Icon::Sliders)
                    .on_click(|_, _, cx| app::open_options(cx)),
            )
            .child(
                ToolbarButton::new("tb-scheduler", "Scheduler", Icon::Calendar)
                    .on_click(|_, _, cx| app::open_scheduler(cx)),
            )
            .child(divider())
            .child(
                ToolbarButton::new("tb-start-queue", "Start Queue", Icon::QueuePlay)
                    .disabled(self.queue_running)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.engine.start_queue();
                        this.refresh(cx);
                    })),
            )
            .child(
                ToolbarButton::new("tb-stop-queue", "Stop Queue", Icon::QueueStop)
                    .disabled(!self.queue_running)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.engine.stop_queue();
                        this.refresh(cx);
                    })),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .pr(px(6.))
                    .text_color(theme.accent)
                    .child(icon(Icon::Bolt, px(18.), theme.accent))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Tachyon"),
                    ),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn sidebar_item(
        &self,
        key: &'static str,
        label: SharedString,
        item_icon: Icon,
        filter: Filter,
        count: usize,
        indent: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.filter == filter;
        let theme = *theme;
        div()
            .id(SharedString::from(format!("side-{key}")))
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(28.))
            .pl(px(if indent { 28. } else { 10. }))
            .pr(px(10.))
            .mx(px(8.))
            .rounded(px(6.))
            .text_size(px(13.))
            .cursor_pointer()
            .text_color(theme.text)
            .when(active, |this| this.bg(theme.accent_soft))
            .when(!active, |this| this.hover(|s| s.bg(theme.hover)))
            .on_click(cx.listener(move |this, _, _, cx| this.set_filter(filter.clone(), cx)))
            .child(icon(item_icon, px(15.), if active { theme.accent } else { theme.muted }))
            .child(div().flex_1().min_w_0().child(truncated(label)))
            .when(count > 0, |this| {
                this.child(div().text_size(px(11.)).text_color(theme.muted).child(count.to_string()))
            })
    }

    fn render_sidebar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.engine.settings();
        let mut categories: Vec<String> = settings.categories.iter().map(|c| c.name.clone()).collect();
        categories.push(GENERAL.to_string());
        let count = |f: &Filter| self.downloads.iter().filter(|d| f.matches(d)).count();

        let mut items: Vec<AnyElement> = Vec::new();
        let chevron = if self.categories_open { Icon::ChevronDown } else { Icon::ChevronRight };
        items.push(
            div()
                .flex()
                .items_center()
                .child(
                    div()
                        .id("toggle-categories")
                        .pl(px(10.))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.categories_open = !this.categories_open;
                            cx.notify();
                        }))
                        .child(icon(chevron, px(10.), theme.muted)),
                )
                .child(
                    div().flex_1().ml(px(-8.)).child(self.sidebar_item(
                        "all",
                        "All Downloads".into(),
                        Icon::Inbox,
                        Filter::All,
                        self.downloads.len(),
                        false,
                        theme,
                        cx,
                    )),
                )
                .into_any_element(),
        );
        if self.categories_open {
            for category in categories {
                let filter = Filter::Category(category.clone());
                let n = count(&filter);
                let key: &'static str = match category.as_str() {
                    "Compressed" => "cat-compressed",
                    "Documents" => "cat-documents",
                    "Music" => "cat-music",
                    "Programs" => "cat-programs",
                    "Video" => "cat-video",
                    GENERAL => "cat-general",
                    _ => "cat-custom",
                };
                items.push(
                    self.sidebar_item(key, category.clone().into(), Icon::for_category(&category), filter, n, true, theme, cx)
                        .into_any_element(),
                );
            }
        }
        items.push(self.sidebar_item("unfinished", "Unfinished".into(), Icon::Clock, Filter::Unfinished, count(&Filter::Unfinished), false, theme, cx).into_any_element());
        items.push(self.sidebar_item("finished", "Finished".into(), Icon::CheckCircle, Filter::Finished, count(&Filter::Finished), false, theme, cx).into_any_element());

        div()
            .id("sidebar")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .bg(theme.surface)
            .border_r_1()
            .border_color(theme.border)
            .pt(px(10.))
            .gap(px(2.))
            .overflow_y_scroll()
            .child(div().px(px(18.)).pb(px(4.)).child(crate::widgets::section_title("Categories", theme)))
            .children(items)
            .child(div().h(px(12.)))
            .child(div().px(px(18.)).pb(px(4.)).child(crate::widgets::section_title("Queues", theme)))
            .child(
                self.sidebar_item(
                    "queue",
                    if self.queue_running { "Main Queue · running".into() } else { "Main Queue".into() },
                    Icon::List,
                    Filter::Queue,
                    count(&Filter::Queue),
                    false,
                    theme,
                    cx,
                ),
            )
    }

    fn render_header(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let (sorted, ascending) = self.sort;
        div()
            .flex()
            .flex_none()
            .h(px(28.))
            .px(px(8.))
            .items_center()
            .bg(theme.chrome)
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(12.))
            .text_color(theme.muted)
            .children(COLUMNS.iter().map(|&(column, title, width)| {
                let is_sorted = sorted == column;
                div()
                    .id(SharedString::from(format!("col-{title}")))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .h_full()
                    .px(px(6.))
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme.text))
                    .when_some(width, |this, w| this.w(px(w)).flex_none())
                    .when(width.is_none(), |this| this.flex_1().min_w(px(180.)))
                    .when(is_sorted, |this| this.text_color(theme.text).font_weight(FontWeight::MEDIUM))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_sort(column, cx)))
                    .child(div().truncate().child(title))
                    .when(is_sorted, |this| {
                        this.child(icon(
                            if ascending { Icon::ChevronUp } else { Icon::ChevronDown },
                            px(10.),
                            theme.muted,
                        ))
                    })
            }))
    }

    fn render_cell(&self, column: Column, d: &DownloadSnapshot, theme: &Theme) -> AnyElement {
        let muted = theme.muted;
        match column {
            Column::Name => {
                let (row_icon, color) = match d.status {
                    DownloadStatus::Failed => (Icon::Alert, theme.danger),
                    DownloadStatus::Completed => (Icon::for_category(&d.category), theme.success),
                    s if s.is_active() => (Icon::ArrowDown, theme.accent),
                    _ => (Icon::for_category(&d.category), muted),
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_w_0()
                    .child(icon(row_icon, px(15.), color))
                    .child(truncated(d.file_name.clone()))
                    .into_any_element()
            }
            Column::Queue => div()
                .when(d.in_queue, |this| this.child(icon(Icon::List, px(13.), muted)))
                .into_any_element(),
            Column::Size => div().truncate().child(format::size_cell(d)).into_any_element(),
            Column::Status => {
                let text = format::status_cell(d);
                let color = match d.status {
                    DownloadStatus::Completed => theme.success,
                    DownloadStatus::Failed => theme.danger,
                    _ => theme.text,
                };
                let show_bar = matches!(d.status, DownloadStatus::Downloading | DownloadStatus::Stopped)
                    && d.downloaded > 0
                    && d.progress().is_some();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(div().w_full().text_color(color).child(truncated(text)))
                    .when(show_bar, |this| {
                        let bar_color = if d.status == DownloadStatus::Downloading { theme.accent } else { theme.faint };
                        this.child(progress_bar(d.progress(), px(3.), bar_color, theme))
                    })
                    .into_any_element()
            }
            Column::TimeLeft => div().truncate().child(format::eta_cell(d)).into_any_element(),
            Column::Rate => div()
                .truncate()
                .child(if d.status == DownloadStatus::Downloading { format::speed(d.speed) } else { String::new() })
                .into_any_element(),
            Column::LastTry => div()
                .truncate()
                .text_color(muted)
                .child(format::date(d.last_try.or(d.completed_at).or(Some(d.added_at))))
                .into_any_element(),
            Column::Description => div()
                .text_color(muted)
                .child(truncated(d.description.clone()))
                .into_any_element(),
        }
    }

    fn render_rows(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = Theme::of(window);
        range
            .filter_map(|ix| {
                let &row = self.rows.get(ix)?;
                let d = &self.downloads[row];
                let id = d.id;
                let selected = self.selected.contains(&id);
                let bg = if selected {
                    theme.accent_soft
                } else if ix % 2 == 1 {
                    theme.row_alt
                } else {
                    theme.bg
                };
                Some(
                    div()
                        .id(("row", id as usize))
                        .flex()
                        .items_center()
                        .h(px(ROW_HEIGHT))
                        .w_full()
                        .px(px(8.))
                        .bg(bg)
                        .text_size(px(13.))
                        .text_color(theme.text)
                        .when(!selected, |this| this.hover(|s| s.bg(theme.hover)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                this.on_row_mouse_down(id, event, window, cx)
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                this.on_row_right_click(id, event, window, cx)
                            }),
                        )
                        .children(COLUMNS.iter().map(|&(column, _, width)| {
                            div()
                                .px(px(6.))
                                .overflow_hidden()
                                .when_some(width, |this, w| this.w(px(w)).flex_none())
                                .when(width.is_none(), |this| this.flex_1().min_w(px(180.)))
                                .child(self.render_cell(column, d, &theme))
                        }))
                        .into_any_element(),
                )
            })
            .collect()
    }

    fn render_empty(&self, theme: &Theme) -> impl IntoElement {
        let (title, hint) = match self.filter {
            Filter::All => ("No downloads yet", "Copy a link and press ⌘N, or click “Add URL”."),
            Filter::Queue => ("The queue is empty", "Choose “Download Later” or right-click a download to queue it."),
            _ => ("Nothing here", "Downloads in this view will appear here."),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.))
            .child(icon(Icon::Inbox, px(40.), theme.faint))
            .child(div().text_size(px(15.)).font_weight(FontWeight::MEDIUM).text_color(theme.text).child(title))
            .child(div().text_size(px(13.)).text_color(theme.muted).child(hint))
            .when(self.filter == Filter::All, |this| {
                this.child(
                    div().pt(px(6.)).child(
                        Button::new("empty-add", "Add URL")
                            .primary()
                            .icon(Icon::Add)
                            .on_click(|_, _, cx| app::open_add_dialog(AddPrefill::FromClipboard, cx)),
                    ),
                )
            })
    }

    fn render_status_bar(&self, theme: &Theme) -> impl IntoElement {
        let active = self.downloads.iter().filter(|d| d.status.is_active()).count();
        let connections: usize = self.downloads.iter().map(|d| d.active_connections).sum();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(16.))
            .h(px(26.))
            .px(px(12.))
            .bg(theme.chrome)
            .border_t_1()
            .border_color(theme.border)
            .text_size(px(12.))
            .text_color(theme.muted)
            .child(format!(
                "{} download{} · {} active",
                self.downloads.len(),
                if self.downloads.len() == 1 { "" } else { "s" },
                active
            ))
            .when(connections > 0, |this| this.child(format!("{connections} connections")))
            .when(self.queue_running, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .text_color(theme.accent)
                        .child(icon(Icon::QueuePlay, px(12.), theme.accent))
                        .child("Queue running"),
                )
            })
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .when(self.total_speed > 0, |this| this.text_color(theme.text))
                    .child(icon(Icon::ArrowDown, px(12.), if self.total_speed > 0 { theme.accent } else { theme.faint }))
                    .child(if self.total_speed > 0 { format::speed(self.total_speed) } else { "Idle".into() }),
            )
    }

    fn render_context_menu(&self, menu: &ContextMenu, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let selection = self.selected_downloads();
        let single = selection.len() == 1;
        let completed = selection.iter().all(|d| d.status == DownloadStatus::Completed);
        let can_resume = selection.iter().any(|d| !d.status.is_active() && d.status != DownloadStatus::Completed);
        let can_stop = selection.iter().any(|d| d.status.is_active());
        let all_queued = selection.iter().all(|d| d.in_queue);
        let any_running = selection.iter().any(|d| d.status.is_active());
        let theme = *theme;

        type Handler = Box<dyn Fn(&mut MainWindow, &mut Window, &mut Context<MainWindow>)>;
        let mut entries: Vec<Option<(&'static str, Icon, bool, Handler)>> = Vec::new();
        entries.push(Some(("Open", Icon::External, single && completed, Box::new(|this, _, cx| {
            let ids: Vec<_> = this.selected.iter().copied().collect();
            ids.into_iter().for_each(|id| this.open(id, cx));
        }))));
        entries.push(Some(("Show in Finder", Icon::Folder, single, Box::new(|this, _, cx| this.reveal(cx)))));
        entries.push(Some(("Progress Window", Icon::Info, single && !completed, Box::new(|this, _, cx| {
            if let Some(&id) = this.selected.iter().next() {
                app::open_progress_window(id, cx);
            }
        }))));
        entries.push(None);
        entries.push(Some(("Resume", Icon::Play, can_resume, Box::new(|this, _, cx| this.resume(cx)))));
        entries.push(Some(("Stop", Icon::Pause, can_stop, Box::new(|this, _, cx| this.stop(cx)))));
        entries.push(Some(("Redownload", Icon::Refresh, !any_running, Box::new(|this, _, cx| this.redownload(cx)))));
        entries.push(None);
        if all_queued {
            entries.push(Some(("Remove from Queue", Icon::List, true, Box::new(|this, _, cx| this.set_queued(false, cx)))));
            entries.push(Some(("Move Up in Queue", Icon::ChevronUp, single, Box::new(|this, _, cx| this.move_in_queue(true, cx)))));
            entries.push(Some(("Move Down in Queue", Icon::ChevronDown, single, Box::new(|this, _, cx| this.move_in_queue(false, cx)))));
        } else {
            entries.push(Some(("Add to Queue", Icon::List, !completed, Box::new(|this, _, cx| this.set_queued(true, cx)))));
        }
        entries.push(None);
        entries.push(Some(("Copy Address", Icon::Copy, true, Box::new(|this, _, cx| this.copy_address(cx)))));
        entries.push(Some(("Delete…", Icon::Trash, true, Box::new(|this, window, cx| this.delete(window, cx)))));

        let items = entries.into_iter().enumerate().map(|(ix, entry)| match entry {
            None => div().my(px(4.)).mx(px(8.)).child(separator(&theme)).into_any_element(),
            Some((label, item_icon, enabled, handler)) => div()
                .id(("ctx", ix))
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(26.))
                .px(px(10.))
                .mx(px(4.))
                .rounded(px(5.))
                .text_size(px(13.))
                .text_color(if enabled { theme.text } else { theme.faint })
                .when(enabled, |this| {
                    this.cursor_pointer()
                        .hover(|s| s.bg(theme.accent).text_color(theme.on_accent))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.context_menu = None;
                            handler(this, window, cx);
                            cx.notify();
                        }))
                })
                .child(icon(item_icon, px(14.), if enabled { theme.muted } else { theme.faint }))
                .child(label)
                .into_any_element(),
        });

        deferred(
            anchored().position(menu.position).snap_to_window_with_margin(px(8.)).child(
                div()
                    .id("context-menu")
                    .occlude()
                    .min_w(px(210.))
                    .py(px(4.))
                    .rounded(px(8.))
                    .bg(theme.chrome)
                    .border_1()
                    .border_color(theme.border)
                    .shadow_lg()
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        this.context_menu = None;
                        cx.notify();
                    }))
                    .children(items),
            ),
        )
        .with_priority(1)
    }
}

#[cfg(feature = "dev-snapshots")]
impl MainWindow {
    pub(crate) fn dev_select_first(&mut self, cx: &mut Context<Self>) {
        if let Some(&row) = self.rows.first() {
            self.selected = BTreeSet::from([self.downloads[row].id]);
            cx.notify();
        }
    }

    pub(crate) fn dev_filter(&mut self, name: &str, cx: &mut Context<Self>) {
        let filter = match name {
            "unfinished" => Filter::Unfinished,
            "finished" => Filter::Finished,
            "queue" => Filter::Queue,
            "all" => Filter::All,
            category => Filter::Category(category.to_string()),
        };
        self.set_filter(filter, cx);
    }
}

impl Focusable for MainWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(window);
        let list: AnyElement = if self.rows.is_empty() {
            self.render_empty(&theme).into_any_element()
        } else {
            uniform_list(
                "downloads",
                self.rows.len(),
                cx.processor(|this, range, window, cx| this.render_rows(range, window, cx)),
            )
            .size_full()
            .track_scroll(self.scroll.clone())
            .into_any_element()
        };

        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::open_selected))
            .on_action(cx.listener(|this, _: &ResumeSelected, _, cx| this.resume(cx)))
            .on_action(cx.listener(|this, _: &StopSelected, _, cx| this.stop(cx)))
            .on_action(cx.listener(|this, _: &DeleteSelected, window, cx| this.delete(window, cx)))
            .on_action(cx.listener(|this, _: &RevealSelected, _, cx| this.reveal(cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.text)
            .font_family(".SystemUIFont")
            .child(self.render_toolbar(&theme, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_sidebar(&theme, cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(self.render_header(&theme, cx))
                            .child(
                                div()
                                    .id("list-area")
                                    .flex_1()
                                    .min_h_0()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, window, _| {
                                            window.focus(&this.focus_handle);
                                        }),
                                    )
                                    .child(list),
                            ),
                    ),
            )
            .child(self.render_status_bar(&theme))
            .when_some(self.context_menu.as_ref(), |this, menu| {
                this.child(self.render_context_menu(menu, &theme, cx))
            })
    }
}

/// Small "About" window.
pub struct AboutWindow;

impl Render for AboutWindow {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(window);
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .bg(theme.bg)
            .text_color(theme.text)
            .child(icon(Icon::Bolt, px(48.), theme.accent))
            .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).child("Tachyon Download Manager"))
            .child(div().text_size(px(12.)).text_color(theme.muted).child(format!("Version {}", env!("CARGO_PKG_VERSION"))))
            .child(
                div()
                    .px(px(28.))
                    .pt(px(6.))
                    .text_size(px(12.))
                    .text_color(theme.muted)
                    .text_center()
                    .child("Multi-connection download accelerator for macOS. Built in Rust with GPUI."),
            )
    }
}

pub fn show_about(cx: &mut App) {
    let bounds = app::centered(size(px(380.), px(240.)), cx);
    let mut options = app::dialog_options("About Tachyon", bounds);
    options.window_bounds = Some(WindowBounds::Windowed(bounds));
    cx.open_window(options, |_, cx| cx.new(|_| AboutWindow)).ok();
}
