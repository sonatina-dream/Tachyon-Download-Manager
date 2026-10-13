//! Countdown before quitting, sleeping or shutting down after downloads finish,
//! so the user can still cancel.

use std::process::Command;
use std::time::Duration;

use gpui::{App, Context, FontWeight, Task, Window, div, prelude::*, px};
use tachyon_engine::PostAction;

use crate::assets::Icon;
use crate::theme::Theme;
use crate::widgets::{Button, icon, separator};

const COUNTDOWN_SECS: u32 = 30;

pub struct PostActionDialog {
    action: PostAction,
    remaining: u32,
    _countdown: Task<()>,
}

impl PostActionDialog {
    pub fn new(action: PostAction, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let countdown = cx.spawn_in(window, async move |this, cx| {
            loop {
                gpui::Timer::after(Duration::from_secs(1)).await;
                let done = this
                    .update_in(cx, |this, window, cx| {
                        this.remaining = this.remaining.saturating_sub(1);
                        cx.notify();
                        if this.remaining == 0 {
                            this.perform(window, cx);
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
        });
        Self {
            action,
            remaining: COUNTDOWN_SECS,
            _countdown: countdown,
        }
    }

    fn perform(&mut self, window: &mut Window, cx: &mut App) {
        window.remove_window();
        perform(self.action, cx);
    }
}

pub fn perform(action: PostAction, cx: &mut App) {
    let result = match action {
        PostAction::Nothing => return,
        PostAction::QuitApp => {
            cx.quit();
            return;
        }
        PostAction::Sleep => Command::new("/usr/bin/pmset").arg("sleepnow").status(),
        PostAction::ShutDown => Command::new("/usr/bin/osascript")
            .args(["-e", "tell application \"System Events\" to shut down"])
            .status(),
    };
    if let Err(e) = result {
        log::error!("could not run {action:?}: {e}");
    }
}

impl Render for PostActionDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(window);
        let what = match self.action {
            PostAction::QuitApp => "Tachyon will quit",
            PostAction::Sleep => "Your Mac will go to sleep",
            PostAction::ShutDown => "Your Mac will shut down",
            PostAction::Nothing => "",
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
                    .flex_1()
                    .gap(px(14.))
                    .p(px(20.))
                    .child(icon(Icon::Clock, px(32.), theme.warning))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child("All downloads finished"))
                            .child(
                                div()
                                    .text_color(theme.muted)
                                    .child(format!("{what} in {} seconds.", self.remaining)),
                            ),
                    ),
            )
            .child(separator(&theme))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .px(px(20.))
                    .py(px(12.))
                    .bg(theme.chrome)
                    .child(Button::new("cancel", "Cancel").on_click(|_, window, _| window.remove_window()))
                    .child(
                        Button::new("now", "Do It Now")
                            .primary()
                            .on_click(cx.listener(|this, _, window, cx| this.perform(window, cx))),
                    ),
            )
    }
}
