//! IDM-style Scheduler for the main download queue.

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, AppContext, Context, Entity, FontWeight, SharedString, Task, Window, div, prelude::*, px,
};
use tachyon_engine::{DayTime, DownloadId, DownloadSnapshot, Engine, PostAction, QueueSettings};

use crate::app;
use crate::assets::Icon;
use crate::format;
use crate::theme::Theme;
use crate::widgets::text_input::TextInput;
use crate::widgets::{Button, Checkbox, icon, radio_group, section_title, separator, stepper, tabs, truncated};

const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

pub struct SchedulerWindow {
    engine: Arc<Engine>,
    draft: QueueSettings,
    pub(crate) tab: usize,
    start_at: Entity<TextInput>,
    stop_at: Entity<TextInput>,
    queue: Vec<DownloadSnapshot>,
    selected: Option<DownloadId>,
    running: bool,
    error: Option<String>,
    saved: bool,
    _poll: Task<()>,
}

fn format_time(t: DayTime) -> String {
    format!("{:02}:{:02}", t.hour, t.minute)
}

fn parse_time(text: &str) -> Option<DayTime> {
    let (h, m) = text.trim().split_once(':')?;
    let (hour, minute) = (h.trim().parse::<u8>().ok()?, m.trim().parse::<u8>().ok()?);
    (hour < 24 && minute < 60).then_some(DayTime { hour, minute })
}

impl SchedulerWindow {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let engine = app::engine(cx);
        let draft = engine.settings().queue;
        let start_text = format_time(draft.schedule.start_at);
        let stop_text = format_time(draft.schedule.stop_at);
        let start_at = cx.new(|cx| TextInput::new(cx, "HH:MM").with_text(start_text));
        let stop_at = cx.new(|cx| TextInput::new(cx, "HH:MM").with_text(stop_text));
        cx.observe_window_appearance(window, |_, _, cx| cx.notify()).detach();
        let poll = cx.spawn(async move |this, cx| {
            loop {
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
                gpui::Timer::after(Duration::from_millis(700)).await;
            }
        });
        Self {
            engine,
            draft,
            tab: 0,
            start_at,
            stop_at,
            queue: Vec::new(),
            selected: None,
            running: false,
            error: None,
            saved: false,
            _poll: poll,
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let mut queue: Vec<DownloadSnapshot> = self.engine.snapshot().into_iter().filter(|d| d.in_queue).collect();
        queue.sort_by_key(|d| d.queue_order);
        self.queue = queue;
        self.running = self.engine.is_queue_running();
        cx.notify();
    }

    fn apply(&mut self, cx: &mut Context<Self>) -> bool {
        let start = parse_time(&self.start_at.read(cx).text());
        let stop = parse_time(&self.stop_at.read(cx).text());
        let (Some(start), Some(stop)) = (start, stop) else {
            self.error = Some("Times must look like 02:30 (24-hour clock).".into());
            self.saved = false;
            cx.notify();
            return false;
        };
        if self.draft.schedule.days == 0 && (self.draft.schedule.start_enabled || self.draft.schedule.stop_enabled) {
            self.error = Some("Pick at least one day.".into());
            self.saved = false;
            cx.notify();
            return false;
        }
        self.draft.schedule.start_at = start;
        self.draft.schedule.stop_at = stop;
        let mut settings = self.engine.settings();
        settings.queue = self.draft.clone();
        match self.engine.update_settings(settings) {
            Ok(()) => {
                self.error = None;
                self.saved = true;
            }
            Err(e) => {
                self.error = Some(e.to_string());
                self.saved = false;
            }
        }
        cx.notify();
        self.error.is_none()
    }

    fn render_schedule(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let schedule = &self.draft.schedule;
        let days = schedule.days;
        let theme_copy = *theme;
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(section_title("Start", theme))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(Checkbox::new("start-enabled", "Start the queue at", schedule.start_enabled).on_toggle(
                        cx.listener(|this, v: &bool, _, cx| {
                            this.draft.schedule.start_enabled = *v;
                            cx.notify();
                        }),
                    ))
                    .child(div().w(px(70.)).child(self.start_at.clone())),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .pl(px(24.))
                    .children(DAYS.iter().enumerate().map(|(i, day)| {
                        let on = days & (1 << i) != 0;
                        div()
                            .id(("day", i))
                            .w(px(40.))
                            .h(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(5.))
                            .border_1()
                            .text_size(px(12.))
                            .cursor_pointer()
                            .border_color(if on { theme_copy.accent } else { theme_copy.border_strong })
                            .bg(if on { theme_copy.accent_soft } else { theme_copy.bg })
                            .text_color(if on { theme_copy.accent } else { theme_copy.muted })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.draft.schedule.days ^= 1 << i;
                                cx.notify();
                            }))
                            .child(*day)
                    }))
                    .child(Button::new("every-day", "Every day").compact().ghost().on_click(cx.listener(
                        |this, _, _, cx| {
                            this.draft.schedule.days = 0b111_1111;
                            cx.notify();
                        },
                    ))),
            )
            .child(div().h(px(4.)))
            .child(section_title("Stop", theme))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(Checkbox::new("stop-enabled", "Stop the queue at", schedule.stop_enabled).on_toggle(
                        cx.listener(|this, v: &bool, _, cx| {
                            this.draft.schedule.stop_enabled = *v;
                            cx.notify();
                        }),
                    ))
                    .child(div().w(px(70.)).child(self.stop_at.clone())),
            )
            .child(div().h(px(4.)))
            .child(section_title("Queue", theme))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child("Download")
                    .child(stepper("concurrent", self.draft.max_concurrent, 1, 16, theme, cx.listener(
                        |this, v: &u32, _, cx| {
                            this.draft.max_concurrent = *v;
                            cx.notify();
                        },
                    )))
                    .child("files at the same time"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child("Retry a failed file")
                    .child(stepper("queue-retries", self.draft.retries, 0, 20, theme, cx.listener(
                        |this, v: &u32, _, cx| {
                            this.draft.retries = *v;
                            cx.notify();
                        },
                    )))
                    .child("times"),
            )
            .child(div().h(px(4.)))
            .child(section_title("When the queue finishes", theme))
            .child(radio_group(
                "queue-post",
                PostAction::ALL.iter().map(|a| (*a, SharedString::from(a.label()))).collect(),
                self.draft.post_action,
                theme,
                cx.listener(|this, a: &PostAction, _, cx| {
                    this.draft.post_action = *a;
                    cx.notify();
                }),
            ))
            .into_any_element()
    }

    fn render_files(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let theme_copy = *theme;
        let has_selection = self.selected.is_some_and(|id| self.queue.iter().any(|d| d.id == id));
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .id("queue-files")
                    .h(px(260.))
                    .overflow_y_scroll()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme.border)
                    .when(self.queue.is_empty(), |this| {
                        this.child(
                            div()
                                .p(px(14.))
                                .text_color(theme.muted)
                                .child("No files in the queue. Use “Download Later” or right-click a download → Add to Queue."),
                        )
                    })
                    .children(self.queue.iter().enumerate().map(|(ix, d)| {
                        let id = d.id;
                        let selected = self.selected == Some(id);
                        div()
                            .id(("queued", ix))
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .h(px(28.))
                            .px(px(10.))
                            .cursor_pointer()
                            .bg(if selected { theme_copy.accent_soft } else if ix % 2 == 1 { theme_copy.row_alt } else { theme_copy.bg })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = Some(id);
                                cx.notify();
                            }))
                            .child(div().w(px(22.)).text_color(theme_copy.muted).child(format!("{}", ix + 1)))
                            .child(icon(Icon::for_category(&d.category), px(14.), theme_copy.muted))
                            .child(div().flex_1().min_w_0().child(truncated(d.file_name.clone())))
                            .child(div().w(px(80.)).text_color(theme_copy.muted).child(format::size_cell(d)))
                            .child(div().w(px(80.)).text_color(theme_copy.muted).child(format::status_cell(d)))
                    })),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(Button::new("up", "Move Up").icon(Icon::ChevronUp).disabled(!has_selection).on_click(
                        cx.listener(|this, _, _, cx| {
                            if let Some(id) = this.selected {
                                this.engine.move_in_queue(id, true);
                                this.refresh(cx);
                            }
                        }),
                    ))
                    .child(Button::new("down", "Move Down").icon(Icon::ChevronDown).disabled(!has_selection).on_click(
                        cx.listener(|this, _, _, cx| {
                            if let Some(id) = this.selected {
                                this.engine.move_in_queue(id, false);
                                this.refresh(cx);
                            }
                        }),
                    ))
                    .child(Button::new("remove", "Remove from Queue").icon(Icon::X).disabled(!has_selection).on_click(
                        cx.listener(|this, _, _, cx| {
                            if let Some(id) = this.selected.take() {
                                this.engine.set_queued(&[id], false);
                                this.refresh(cx);
                            }
                        }),
                    )),
            )
            .into_any_element()
    }
}

impl Render for SchedulerWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(window);
        let body = match self.tab {
            0 => self.render_schedule(&theme, cx),
            _ => self.render_files(&theme, cx),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.text)
            .text_size(px(13.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(20.))
                    .pt(px(14.))
                    .pb(px(10.))
                    .child(icon(Icon::List, px(18.), theme.accent))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Main Queue"))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(if self.running { theme.success } else { theme.muted })
                            .child(if self.running { "Running" } else { "Stopped" }),
                    )
                    .child(div().flex_1())
                    .child(tabs("sched-tabs", &["Schedule", "Files in the queue"], self.tab, &theme, cx.listener(
                        |this, ix: &usize, _, cx| {
                            this.tab = *ix;
                            cx.notify();
                        },
                    ))),
            )
            .child(separator(&theme))
            .child(div().id("sched-body").flex_1().min_h_0().overflow_y_scroll().p(px(20.)).child(body))
            .child(separator(&theme))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .px(px(20.))
                    .py(px(12.))
                    .bg(theme.chrome)
                    .child(if self.running {
                        Button::new("stop-queue", "Stop Queue")
                            .icon(Icon::QueueStop)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.engine.stop_queue();
                                this.refresh(cx);
                            }))
                            .into_any_element()
                    } else {
                        Button::new("start-queue", "Start Now")
                            .icon(Icon::QueuePlay)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.apply(cx) {
                                    this.engine.start_queue();
                                    this.refresh(cx);
                                }
                            }))
                            .into_any_element()
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(12.))
                            .when_some(self.error.clone(), |this, e| this.text_color(theme.danger).child(e))
                            .when(self.error.is_none() && self.saved, |this| this.text_color(theme.success).child("Saved")),
                    )
                    .child(Button::new("close", "Close").on_click(|_, window, _| window.remove_window()))
                    .child(Button::new("apply", "Apply").primary().on_click(cx.listener(|this, _, _, cx| {
                        this.apply(cx);
                    }))),
            )
    }
}
