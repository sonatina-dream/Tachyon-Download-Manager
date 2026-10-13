//! IDM-style Options dialog.

use std::sync::Arc;

use gpui::{
    AnyElement, AppContext, Context, Entity, FontWeight, PathPromptOptions, SharedString, Window,
    div, prelude::*, px,
};
use tachyon_engine::{CategoryDef, Engine, Settings, default_categories};

use crate::app;
use crate::format;
use crate::assets::Icon;
use crate::theme::Theme;
use crate::widgets::text_input::TextInput;
use crate::widgets::{Button, Checkbox, icon, section_title, separator, stepper, tabs, truncated};

const TABS: [&str; 5] = ["General", "Connection", "Save To", "File Types", "Browser"];
const CONNECTION_PRESETS: [u32; 7] = [1, 2, 4, 8, 16, 24, 32];

pub struct OptionsWindow {
    engine: Arc<Engine>,
    draft: Settings,
    pub(crate) tab: usize,
    download_dir: Entity<TextInput>,
    proxy: Entity<TextInput>,
    user_agent: Entity<TextInput>,
    speed_limit: Entity<TextInput>,
    min_split: Entity<TextInput>,
    retry_delay: Entity<TextInput>,
    connect_timeout: Entity<TextInput>,
    read_timeout: Entity<TextInput>,
    port: Entity<TextInput>,
    extensions: Vec<(String, Entity<TextInput>)>,
    error: Option<String>,
    saved: bool,
}

fn input(cx: &mut Context<OptionsWindow>, placeholder: &str, text: impl Into<SharedString>) -> Entity<TextInput> {
    let placeholder = placeholder.to_string();
    let text = text.into();
    cx.new(move |cx| TextInput::new(cx, placeholder).with_text(text))
}

fn number(cx: &mut Context<OptionsWindow>, value: u64) -> Entity<TextInput> {
    cx.new(move |cx| TextInput::new(cx, "0").with_text(value.to_string()).numeric())
}

impl OptionsWindow {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let engine = app::engine(cx);
        let draft = engine.settings();
        cx.observe_window_appearance(window, |_, _, cx| cx.notify()).detach();
        let extensions = draft
            .categories
            .iter()
            .map(|c| (c.name.clone(), input(cx, "ext1 ext2 …", c.extensions.join(" "))))
            .collect();
        Self {
            download_dir: input(cx, "Folder", format::display_path(&draft.download_dir)),
            proxy: input(cx, "System proxy", draft.proxy.clone()),
            user_agent: input(cx, "User agent", draft.user_agent.clone()),
            speed_limit: number(cx, draft.global_speed_limit / 1024),
            min_split: number(cx, draft.min_split_size / 1024),
            retry_delay: number(cx, draft.retry_delay_secs),
            connect_timeout: number(cx, draft.connect_timeout_secs),
            read_timeout: number(cx, draft.read_timeout_secs),
            port: number(cx, u64::from(draft.integration_port)),
            extensions,
            engine,
            draft,
            tab: 0,
            error: None,
            saved: false,
        }
    }

    fn parse_number(&self, entity: &Entity<TextInput>, what: &str, cx: &Context<Self>) -> Result<u64, String> {
        let text = entity.read(cx).text();
        let text = text.trim();
        if text.is_empty() {
            return Ok(0);
        }
        text.parse::<u64>().map_err(|_| format!("{what} must be a whole number."))
    }

    /// Reads all fields into a new `Settings`.
    fn collect(&self, cx: &Context<Self>) -> Result<Settings, String> {
        let mut s = self.draft.clone();
        let dir = self.download_dir.read(cx).text();
        if dir.trim().is_empty() {
            return Err("Choose a download folder.".into());
        }
        s.download_dir = format::expand_home(dir.trim());
        s.proxy = self.proxy.read(cx).text().trim().to_string();
        if !s.proxy.is_empty() && url::Url::parse(&s.proxy).is_err() {
            return Err("The proxy must be a URL such as http://host:8080 or socks5://host:1080.".into());
        }
        let ua = self.user_agent.read(cx).text();
        if !ua.trim().is_empty() {
            s.user_agent = ua.trim().to_string();
        }
        s.global_speed_limit = self.parse_number(&self.speed_limit, "The speed limit", cx)? * 1024;
        s.min_split_size = self.parse_number(&self.min_split, "The minimum segment size", cx)?.max(256) * 1024;
        s.retry_delay_secs = self.parse_number(&self.retry_delay, "The retry delay", cx)?;
        s.connect_timeout_secs = self.parse_number(&self.connect_timeout, "The connection timeout", cx)?;
        s.read_timeout_secs = self.parse_number(&self.read_timeout, "The read timeout", cx)?;
        let port = self.parse_number(&self.port, "The port", cx)?;
        s.integration_port = u16::try_from(port)
            .ok()
            .filter(|p| *p >= 1024)
            .ok_or("The port must be between 1024 and 65535.")?;
        s.categories = self
            .extensions
            .iter()
            .map(|(name, entity)| {
                let previous = self.draft.categories.iter().find(|c| &c.name == name);
                CategoryDef {
                    name: name.clone(),
                    extensions: entity
                        .read(cx)
                        .text()
                        .split([' ', ',', ';'])
                        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
                        .filter(|e| !e.is_empty())
                        .collect(),
                    folder: previous.and_then(|c| c.folder.clone()),
                }
            })
            .collect();
        Ok(s)
    }

    fn apply(&mut self, cx: &mut Context<Self>) -> bool {
        let result = self.collect(cx).and_then(|settings| {
            self.engine.update_settings(settings.clone()).map_err(|e| e.to_string())?;
            Ok(settings)
        });
        match result {
            Ok(settings) => {
                self.draft = settings;
                self.error = None;
                self.saved = true;
                cx.notify();
                true
            }
            Err(e) => {
                self.error = Some(e);
                self.saved = false;
                cx.notify();
                false
            }
        }
    }

    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| {
                    this.download_dir.update(cx, |i, cx| i.set_text(format::display_path(&path), cx));
                })
                .ok();
            }
        })
        .detach();
    }

    fn row(label: &'static str, field: impl IntoElement, theme: &Theme) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .min_h(px(30.))
            .child(div().w(px(190.)).flex_none().text_color(theme.muted).child(label))
            .child(div().flex_1().min_w_0().flex().items_center().gap(px(8.)).child(field))
    }

    fn hint(text: impl Into<SharedString>, theme: &Theme) -> impl IntoElement {
        div().text_size(px(12.)).text_color(theme.muted).child(text.into())
    }

    fn render_general(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let d = &self.draft;
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(section_title("Dialogs", theme))
            .child(Checkbox::new("show-progress", "Show the progress window when a download starts", d.show_progress_window).on_toggle(
                cx.listener(|this, v: &bool, _, cx| {
                    this.draft.show_progress_window = *v;
                    cx.notify();
                }),
            ))
            .child(Checkbox::new("show-complete", "Show the download complete dialog", d.show_complete_dialog).on_toggle(
                cx.listener(|this, v: &bool, _, cx| {
                    this.draft.show_complete_dialog = *v;
                    cx.notify();
                }),
            ))
            .child(Checkbox::new("clipboard", "Offer to download links copied to the clipboard", d.monitor_clipboard).on_toggle(
                cx.listener(|this, v: &bool, _, cx| {
                    this.draft.monitor_clipboard = *v;
                    cx.notify();
                }),
            ))
            .child(div().h(px(6.)))
            .child(section_title("Network", theme))
            .child(Self::row("Proxy", self.proxy.clone(), theme))
            .child(Self::hint("Leave empty to use the macOS system proxy. Example: socks5://127.0.0.1:1080", theme))
            .child(Self::row("User agent", self.user_agent.clone(), theme))
            .into_any_element()
    }

    fn render_connection(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let d = &self.draft;
        let current = d.max_connections;
        let theme_copy = *theme;
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(section_title("Acceleration", theme))
            .child(Self::row(
                "Connections per download",
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(stepper("max-conn", current, 1, Settings::MAX_CONNECTIONS_LIMIT, theme, cx.listener(
                        |this, v: &u32, _, cx| {
                            this.draft.max_connections = *v;
                            cx.notify();
                        },
                    )))
                    .children(CONNECTION_PRESETS.iter().map(|&n| {
                        let active = n == current;
                        div()
                            .id(("preset", n as usize))
                            .px(px(7.))
                            .h(px(22.))
                            .flex()
                            .items_center()
                            .rounded(px(5.))
                            .text_size(px(12.))
                            .cursor_pointer()
                            .text_color(if active { theme_copy.on_accent } else { theme_copy.muted })
                            .when(active, |this| this.bg(theme_copy.accent))
                            .when(!active, |this| this.hover(|s| s.bg(theme_copy.hover)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.draft.max_connections = n;
                                cx.notify();
                            }))
                            .child(n.to_string())
                    })),
                theme,
            ))
            .child(Self::hint(
                "Each download is split dynamically: when a connection finishes, it takes over half of the largest remaining part.",
                theme,
            ))
            .child(Self::row(
                "Minimum segment size",
                div().flex().items_center().gap(px(8.)).child(div().w(px(90.)).child(self.min_split.clone())).child("KB"),
                theme,
            ))
            .child(Self::row(
                "Speed limit (all downloads)",
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().w(px(90.)).child(self.speed_limit.clone()))
                    .child("KB/s  (0 = unlimited)"),
                theme,
            ))
            .child(div().h(px(6.)))
            .child(section_title("Reliability", theme))
            .child(Self::row(
                "Retries per connection",
                stepper("retries", d.max_retries, 0, 99, theme, cx.listener(|this, v: &u32, _, cx| {
                    this.draft.max_retries = *v;
                    cx.notify();
                })),
                theme,
            ))
            .child(Self::row(
                "Delay between retries",
                div().flex().items_center().gap(px(8.)).child(div().w(px(90.)).child(self.retry_delay.clone())).child("seconds"),
                theme,
            ))
            .child(Self::row(
                "Connection timeout",
                div().flex().items_center().gap(px(8.)).child(div().w(px(90.)).child(self.connect_timeout.clone())).child("seconds"),
                theme,
            ))
            .child(Self::row(
                "Read timeout",
                div().flex().items_center().gap(px(8.)).child(div().w(px(90.)).child(self.read_timeout.clone())).child("seconds"),
                theme,
            ))
            .into_any_element()
    }

    fn render_save_to(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let d = &self.draft;
        let base = format::expand_home(self.download_dir.read(cx).text().trim());
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(section_title("Location", theme))
            .child(Self::row(
                "Download folder",
                div()
                    .flex()
                    .flex_1()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().child(self.download_dir.clone()))
                    .child(Button::new("browse", "Choose…").icon(Icon::Folder).on_click(cx.listener(
                        |this, _, window, cx| this.browse(window, cx),
                    ))),
                theme,
            ))
            .child(Checkbox::new("subfolders", "Save each category in its own subfolder", d.use_category_folders).on_toggle(
                cx.listener(|this, v: &bool, _, cx| {
                    this.draft.use_category_folders = *v;
                    cx.notify();
                }),
            ))
            .child(div().h(px(6.)))
            .child(section_title("Where each category goes", theme))
            .children(d.categories.iter().map(|c| {
                let folder = c.folder.clone().unwrap_or_else(|| {
                    if d.use_category_folders { base.join(&c.name) } else { base.clone() }
                });
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(24.))
                    .child(icon(crate::assets::Icon::for_category(&c.name), px(14.), theme.muted))
                    .child(div().w(px(100.)).child(c.name.clone()))
                    .child(div().flex_1().min_w_0().text_color(theme.muted).child(truncated(format::display_path(&folder))))
            }))
            .into_any_element()
    }

    fn render_file_types(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(section_title("Extensions by category", theme))
            .child(Self::hint(
                "New downloads are sorted into a category by extension. Separate extensions with spaces.",
                theme,
            ))
            .children(self.extensions.iter().map(|(name, entity)| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(icon(crate::assets::Icon::for_category(name), px(14.), theme.muted))
                    .child(div().w(px(96.)).flex_none().child(name.clone()))
                    .child(div().flex_1().min_w_0().child(entity.clone()))
            }))
            .child(div().pt(px(4.)).child(Button::new("reset-types", "Restore Defaults").on_click(cx.listener(
                |this, _, _, cx| {
                    let defaults = default_categories();
                    for (name, entity) in &this.extensions {
                        if let Some(def) = defaults.iter().find(|d| &d.name == name) {
                            let text = def.extensions.join(" ");
                            entity.update(cx, |i, cx| i.set_text(text, cx));
                        }
                    }
                },
            ))))
            .into_any_element()
    }

    fn render_browser(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let d = &self.draft;
        let extension = app::extension_dir();
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(section_title("Browser integration", theme))
            .child(Checkbox::new("integration", "Accept downloads from the Tachyon browser extension", d.browser_integration).on_toggle(
                cx.listener(|this, v: &bool, _, cx| {
                    this.draft.browser_integration = *v;
                    cx.notify();
                }),
            ))
            .child(Self::row(
                "Local port",
                div().flex().items_center().gap(px(8.)).child(div().w(px(90.)).child(self.port.clone())).child(
                    div().text_color(theme.muted).child("on 127.0.0.1 only"),
                ),
                theme,
            ))
            .child(Checkbox::new("auto-start", "Start captured downloads without asking", d.start_captured_immediately).on_toggle(
                cx.listener(|this, v: &bool, _, cx| {
                    this.draft.start_captured_immediately = *v;
                    cx.notify();
                }),
            ))
            .child(div().h(px(6.)))
            .child(section_title("Install the extension (Chrome, Edge, Brave, Arc)", theme))
            .child(Self::hint("1. Open chrome://extensions and turn on Developer mode.", theme))
            .child(Self::hint("2. Click “Load unpacked” and choose the extension folder.", theme))
            .child(Self::hint("3. Right-click any link and choose “Download with Tachyon”, or let it take over downloads.", theme))
            .child(div().pt(px(4.)).child(match extension {
                Some(path) => Button::new("show-ext", "Show Extension Folder")
                    .icon(Icon::Folder)
                    .on_click(move |_, _, cx| cx.reveal_path(&path))
                    .into_any_element(),
                None => Self::hint("The extension folder wasn't found next to the app.", theme).into_any_element(),
            }))
            .into_any_element()
    }
}

impl Render for OptionsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(window);
        let body = match self.tab {
            0 => self.render_general(&theme, cx),
            1 => self.render_connection(&theme, cx),
            2 => self.render_save_to(&theme, cx),
            3 => self.render_file_types(&theme, cx),
            _ => self.render_browser(&theme, cx),
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
                    .justify_center()
                    .pt(px(14.))
                    .pb(px(10.))
                    .child(tabs("options-tabs", &TABS, self.tab, &theme, cx.listener(|this, ix: &usize, _, cx| {
                        this.tab = *ix;
                        cx.notify();
                    }))),
            )
            .child(separator(&theme))
            .child(div().id("options-body").flex_1().min_h_0().overflow_y_scroll().p(px(20.)).child(body))
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
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(12.))
                            .when_some(self.error.clone(), |this, e| this.text_color(theme.danger).child(e))
                            .when(self.error.is_none() && self.saved, |this| {
                                this.text_color(theme.success).font_weight(FontWeight::MEDIUM).child("Saved")
                            }),
                    )
                    .child(Button::new("cancel", "Cancel").on_click(|_, window, _| window.remove_window()))
                    .child(Button::new("apply", "Apply").on_click(cx.listener(|this, _, _, cx| {
                        this.apply(cx);
                    })))
                    .child(Button::new("ok", "OK").primary().on_click(cx.listener(|this, _, window, cx| {
                        if this.apply(cx) {
                            window.remove_window();
                        }
                    }))),
            )
    }
}
