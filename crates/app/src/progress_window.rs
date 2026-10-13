//! Per-download progress dialog, modelled on IDM's: status, speed limiter and
//! completion options, plus a segment map and connection table under "details".

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, AppContext, Context, Entity, FontWeight, SharedString, Subscription, Task, Window,
    div, prelude::*, px, size,
};
use tachyon_engine::{
    CompletionOptions, DownloadDetails, DownloadId, DownloadSnapshot, DownloadStatus, Engine,
    PostAction,
};

use crate::app::{self, PROGRESS_SIZE, PROGRESS_SIZE_EXPANDED};
use crate::assets::Icon;
use crate::format;
use crate::theme::Theme;
use crate::widgets::text_input::{InputEvent, TextInput};
use crate::widgets::{
    Button, Checkbox, field_row, icon, progress_bar, radio_group, section_title, segment_bar,
    separator, tabs, truncated,
};

pub struct ProgressWindow {
    engine: Arc<Engine>,
    id: DownloadId,
    snapshot: Option<DownloadSnapshot>,
    details: DownloadDetails,
    pub(crate) tab: usize,
    expanded: bool,
    limit_enabled: bool,
    limit_input: Entity<TextInput>,
    limit_error: Option<String>,
    _poll: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl ProgressWindow {
    pub fn new(id: DownloadId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let engine = app::engine(cx);
        let snapshot = engine.get(id);
        let limit = snapshot.as_ref().map(|s| s.speed_limit).unwrap_or(0);
        let limit_text = if limit > 0 { (limit / 1024).to_string() } else { "500".to_string() };
        let limit_input = cx.new(|cx| TextInput::new(cx, "KB/s").with_text(limit_text).numeric());

        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                if this.update_in(cx, |this, window, cx| this.refresh(window, cx)).is_err() {
                    break;
                }
                gpui::Timer::after(Duration::from_millis(250)).await;
            }
        });
        let subscriptions = vec![
            cx.subscribe_in(&limit_input, window, |this, _, event, _, cx| {
                if let InputEvent::Submit = event {
                    this.apply_limit(cx);
                }
            }),
            cx.on_release(move |_, cx| app::forget_progress_window(id, cx)),
            cx.observe_window_appearance(window, |_, _, cx| cx.notify()),
        ];
        Self {
            engine,
            id,
            snapshot,
            details: DownloadDetails::default(),
            tab: 0,
            expanded: false,
            limit_enabled: limit > 0,
            limit_input,
            limit_error: None,
            _poll: poll,
            _subscriptions: subscriptions,
        }
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot = self.engine.get(self.id);
        let Some(snapshot) = &self.snapshot else {
            // Deleted from the list.
            window.remove_window();
            return;
        };
        if self.expanded {
            self.details = self.engine.details(self.id);
        }
        let title = match snapshot.progress() {
            Some(p) if snapshot.downloaded > 0 => format!("{:.0}% {}", p * 100.0, snapshot.file_name),
            _ => snapshot.file_name.clone(),
        };
        window.set_window_title(&title);
        cx.notify();
    }

    pub(crate) fn toggle_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.expanded = !self.expanded;
        let (w, h) = if self.expanded { PROGRESS_SIZE_EXPANDED } else { PROGRESS_SIZE };
        window.resize(size(px(w), px(h)));
        self.details = self.engine.details(self.id);
        cx.notify();
    }

    fn apply_limit(&mut self, cx: &mut Context<Self>) {
        if !self.limit_enabled {
            self.engine.set_speed_limit(self.id, 0);
            self.limit_error = None;
            cx.notify();
            return;
        }
        match format::parse_rate_kb(&self.limit_input.read(cx).text()) {
            Some(rate) if rate > 0 => {
                self.engine.set_speed_limit(self.id, rate);
                self.limit_error = None;
            }
            _ => self.limit_error = Some("Enter a speed in KB/s, for example 500.".into()),
        }
        cx.notify();
    }

    fn update_completion(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut CompletionOptions)) {
        if let Some(snapshot) = &mut self.snapshot {
            let mut completion = snapshot.completion.clone();
            f(&mut completion);
            snapshot.completion = completion.clone();
            self.engine.set_completion(self.id, completion);
            cx.notify();
        }
    }

    fn render_status_tab(&self, d: &DownloadSnapshot, theme: &Theme) -> AnyElement {
        let downloaded = match d.progress() {
            Some(p) => format!("{} ({:.2}%)", format::bytes(d.downloaded), p * 100.0),
            None => format::bytes(d.downloaded),
        };
        let resume = match d.resumable {
            Some(true) => div().text_color(theme.success).child("Yes"),
            Some(false) => div().text_color(theme.warning).child("No"),
            None => div().text_color(theme.muted).child("Unknown"),
        };
        let status_color = match d.status {
            DownloadStatus::Failed => theme.danger,
            DownloadStatus::Completed => theme.success,
            _ => theme.text,
        };
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(field_row("Status", div().w_full().text_color(status_color).child(truncated(format::status_line(d))), theme))
            .child(field_row("File size", d.size.map(format::bytes).unwrap_or_else(|| "Unknown".into()), theme))
            .child(field_row("Downloaded", downloaded, theme))
            .child(field_row(
                "Transfer rate",
                if d.status == DownloadStatus::Downloading { format::speed(d.speed) } else { "—".into() },
                theme,
            ))
            .child(field_row(
                "Time left",
                d.eta_secs
                    .filter(|_| d.status == DownloadStatus::Downloading)
                    .map(format::duration)
                    .unwrap_or_else(|| "—".into()),
                theme,
            ))
            .child(field_row("Resume capability", resume, theme))
            .into_any_element()
    }

    fn render_limiter_tab(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                Checkbox::new("use-limit", "Use speed limiter", self.limit_enabled).on_toggle(cx.listener(
                    |this, checked: &bool, _, cx| {
                        this.limit_enabled = *checked;
                        this.apply_limit(cx);
                    },
                )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .when(!self.limit_enabled, |this| this.opacity(0.5))
                    .child(div().text_color(theme.muted).child("Maximum download speed"))
                    .child(div().w(px(90.)).child(self.limit_input.clone()))
                    .child(div().text_color(theme.muted).child("KB/s"))
                    .child(
                        Button::new("apply-limit", "Apply")
                            .compact()
                            .disabled(!self.limit_enabled)
                            .on_click(cx.listener(|this, _, _, cx| this.apply_limit(cx))),
                    ),
            )
            .when_some(self.limit_error.as_ref(), |this, e| {
                this.child(div().text_size(px(12.)).text_color(theme.danger).child(e.clone()))
            })
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.muted)
                    .child("Limits this download only. Set a limit for all downloads in Options → Connection."),
            )
            .into_any_element()
    }

    fn render_completion_tab(&self, d: &DownloadSnapshot, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let completion = d.completion.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                Checkbox::new("show-complete", "Show the download complete dialog", completion.show_dialog)
                    .on_toggle(cx.listener(|this, checked: &bool, _, cx| {
                        let checked = *checked;
                        this.update_completion(cx, |c| c.show_dialog = checked);
                    })),
            )
            .child(
                Checkbox::new("open-file", "Open the file when it's done", completion.open_file).on_toggle(
                    cx.listener(|this, checked: &bool, _, cx| {
                        let checked = *checked;
                        this.update_completion(cx, |c| c.open_file = checked);
                    }),
                ),
            )
            .child(div().pt(px(4.)).child(section_title("When done", theme)))
            .child(radio_group(
                "post-action",
                PostAction::ALL.iter().map(|a| (*a, SharedString::from(a.label()))).collect(),
                completion.post_action,
                theme,
                cx.listener(|this, action: &PostAction, _, cx| {
                    let action = *action;
                    this.update_completion(cx, |c| c.post_action = action);
                }),
            ))
            .into_any_element()
    }

    fn render_details(&self, d: &DownloadSnapshot, theme: &Theme) -> impl IntoElement {
        let connections = self.details.connections.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(section_title("Start positions and download progress by connections", theme))
            .child(segment_bar(self.details.segments.clone(), d.size, theme))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .h(px(24.))
                            .items_center()
                            .px(px(10.))
                            .bg(theme.chrome)
                            .text_size(px(11.))
                            .text_color(theme.muted)
                            .child(div().w(px(40.)).child("N."))
                            .child(div().w(px(110.)).child("Downloaded"))
                            .child(div().flex_1().child("Info")),
                    )
                    .child(
                        div()
                            .id("connections")
                            .h(px(150.))
                            .overflow_y_scroll()
                            .when(connections.is_empty(), |this| {
                                this.child(
                                    div()
                                        .p(px(10.))
                                        .text_size(px(12.))
                                        .text_color(theme.muted)
                                        .child("No active connections."),
                                )
                            })
                            .children(connections.into_iter().map(|c| {
                                let color = match c.state {
                                    tachyon_engine::ConnectionState::Failed(_) => theme.danger,
                                    tachyon_engine::ConnectionState::Retrying { .. } => theme.warning,
                                    tachyon_engine::ConnectionState::Finished
                                    | tachyon_engine::ConnectionState::Idle
                                    | tachyon_engine::ConnectionState::Released(_) => theme.muted,
                                    _ => theme.text,
                                };
                                div()
                                    .flex()
                                    .h(px(22.))
                                    .items_center()
                                    .px(px(10.))
                                    .border_t_1()
                                    .border_color(theme.border)
                                    .text_size(px(12.))
                                    .child(div().w(px(40.)).text_color(theme.muted).child(format!("{}", c.index + 1)))
                                    .child(div().w(px(110.)).child(format::bytes(c.downloaded)))
                                    .child(div().flex_1().min_w_0().text_color(color).child(truncated(c.state.label())))
                            })),
                    ),
            )
    }
}

impl Render for ProgressWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(window);
        let Some(d) = self.snapshot.clone() else {
            return div().size_full().bg(theme.bg).into_any_element();
        };
        let active = d.status.is_active();
        let tab_body = match self.tab {
            0 => self.render_status_tab(&d, &theme),
            1 => self.render_limiter_tab(&theme, cx),
            _ => self.render_completion_tab(&d, &theme, cx),
        };
        let bar_color = match d.status {
            DownloadStatus::Failed => theme.danger,
            DownloadStatus::Completed => theme.success,
            DownloadStatus::Stopped => theme.faint,
            _ => theme.accent,
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
                    .gap(px(12.))
                    .px(px(20.))
                    .pt(px(16.))
                    .pb(px(12.))
                    .child(icon(Icon::for_category(&d.category), px(30.), theme.accent))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().w_full().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(truncated(d.file_name.clone())))
                            .child(div().w_full().text_size(px(12.)).text_color(theme.muted).child(truncated(d.url.clone()))),
                    ),
            )
            .child(
                div().px(px(20.)).child(tabs(
                    "progress-tabs",
                    &["Download status", "Speed Limiter", "Options on completion"],
                    self.tab,
                    &theme,
                    cx.listener(|this, ix: &usize, _, cx| {
                        this.tab = *ix;
                        cx.notify();
                    }),
                )),
            )
            .child(div().h(px(158.)).flex_none().px(px(20.)).pt(px(12.)).child(tab_body))
            .child(
                div()
                    .px(px(20.))
                    .pb(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(div().flex_1().child(progress_bar(d.progress(), px(10.), bar_color, &theme)))
                    .child(
                        div()
                            .w(px(52.))
                            .text_right()
                            .text_size(px(12.))
                            .text_color(theme.muted)
                            .child(d.progress().map(|p| format!("{:.1}%", p * 100.0)).unwrap_or_default()),
                    ),
            )
            .when(self.expanded, |this| this.child(div().px(px(20.)).pb(px(12.)).child(self.render_details(&d, &theme))))
            .child(div().flex_1())
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
                    .child(
                        Button::new("details", if self.expanded { "Hide details" } else { "Show details" })
                            .ghost()
                            .icon(if self.expanded { Icon::ChevronUp } else { Icon::ChevronDown })
                            .on_click(cx.listener(|this, _, window, cx| this.toggle_details(window, cx))),
                    )
                    .child(div().flex_1())
                    .child(if active {
                        Button::new("pause", "Pause")
                            .icon(Icon::Pause)
                            .on_click(cx.listener(|this, _, _, _| this.engine.pause(this.id)))
                            .into_any_element()
                    } else {
                        Button::new("resume", "Resume")
                            .icon(Icon::Play)
                            .disabled(d.status == DownloadStatus::Completed)
                            .on_click(cx.listener(|this, _, _, _| {
                                if let Err(e) = this.engine.start(this.id) {
                                    log::warn!("cannot resume: {e}");
                                }
                            }))
                            .into_any_element()
                    })
                    .child(
                        Button::new("cancel", "Cancel")
                            .on_click(cx.listener(|this, _, window, _| {
                                this.engine.pause(this.id);
                                window.remove_window();
                            })),
                    ),
            )
            .into_any_element()
    }
}
