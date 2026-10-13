//! IDM's "Download complete" dialog.

use std::sync::Arc;

use gpui::{Context, FontWeight, Window, div, prelude::*, px};
use tachyon_engine::{DownloadId, DownloadSnapshot, Engine};

use crate::app;
use crate::assets::Icon;
use crate::format;
use crate::theme::Theme;
use crate::widgets::{Button, Checkbox, field_row, icon, separator, truncated};

pub struct CompleteDialog {
    engine: Arc<Engine>,
    download: Option<DownloadSnapshot>,
    dont_show_again: bool,
}

impl CompleteDialog {
    pub fn new(id: DownloadId, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let engine = app::engine(cx);
        Self {
            download: engine.get(id),
            engine,
            dont_show_again: false,
        }
    }

    fn close(&mut self, window: &mut Window) {
        if self.dont_show_again {
            let mut settings = self.engine.settings();
            settings.show_complete_dialog = false;
            if let Err(e) = self.engine.update_settings(settings) {
                log::warn!("could not save settings: {e}");
            }
        }
        window.remove_window();
    }
}

impl Render for CompleteDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(window);
        let Some(d) = self.download.clone() else {
            return div().size_full().bg(theme.bg);
        };
        let path = d.path();
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
                    .gap(px(16.))
                    .p(px(20.))
                    .child(icon(Icon::CheckCircle, px(40.), theme.success))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(4.))
                            .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child("Download complete"))
                            .child(field_row("Address", truncated(d.url.clone()), &theme))
                            .child(field_row("File name", truncated(d.file_name.clone()), &theme))
                            .child(field_row("Folder", truncated(format::display_path(&d.directory)), &theme))
                            .child(field_row("Size", d.size.map(format::bytes).unwrap_or_default(), &theme)),
                    ),
            )
            .child(
                div().px(px(20.)).pb(px(14.)).child(
                    Checkbox::new("dont-show", "Don't show this dialog again", self.dont_show_again).on_toggle(
                        cx.listener(|this, checked: &bool, _, cx| {
                            this.dont_show_again = *checked;
                            cx.notify();
                        }),
                    ),
                ),
            )
            .child(div().flex_1())
            .child(separator(&theme))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .px(px(20.))
                    .py(px(12.))
                    .bg(theme.chrome)
                    .child(Button::new("folder", "Show in Finder").icon(Icon::Folder).on_click({
                        let path = path.clone();
                        cx.listener(move |this, _, window, cx| {
                            cx.reveal_path(&path);
                            this.close(window);
                        })
                    }))
                    .child(Button::new("close", "Close").on_click(cx.listener(|this, _, window, _| this.close(window))))
                    .child(Button::new("open", "Open").primary().icon(Icon::External).on_click(cx.listener(
                        move |this, _, window, cx| {
                            cx.open_with_system(&path);
                            this.close(window);
                        },
                    ))),
            )
    }
}
