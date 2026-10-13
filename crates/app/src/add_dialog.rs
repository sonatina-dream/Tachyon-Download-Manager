//! "Add URL" → "Download File Info", IDM's two-step add flow in one window.

use std::sync::Arc;

use gpui::{
    AnyElement, AppContext, Context, Entity, Focusable, FontWeight, PathPromptOptions, SharedString,
    Subscription, Window, div, prelude::*, px, size,
};
use tachyon_engine::{
    AddRequest, Credentials, Engine, GENERAL, ProbeInfo, RequestSpec, derive_file_name,
};

use crate::app;
use crate::assets::Icon;
use crate::format::{self, display_path, expand_home};
use crate::theme::Theme;
use crate::widgets::text_input::{InputEvent, TextInput};
use crate::widgets::{Button, Checkbox, icon, truncated};

pub enum AddPrefill {
    /// Use the clipboard if it holds a link.
    FromClipboard,
    Url(String),
    /// Handed over by the browser extension.
    Captured(Box<AddRequest>),
}

#[derive(PartialEq, Eq)]
enum Stage {
    EnterUrl,
    Probing,
    Info,
}

pub struct AddDialog {
    engine: Arc<Engine>,
    stage: Stage,
    url: Entity<TextInput>,
    use_auth: bool,
    username: Entity<TextInput>,
    password: Entity<TextInput>,
    file_name: Entity<TextInput>,
    folder: Entity<TextInput>,
    description: Entity<TextInput>,
    category: String,
    category_chosen: bool,
    folder_chosen: bool,
    /// Headers (referrer, cookies…) that came with a captured link.
    base_request: RequestSpec,
    captured_name: Option<String>,
    probe: Option<ProbeInfo>,
    probe_error: Option<String>,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

fn is_http_url(text: &str) -> bool {
    url::Url::parse(text.trim())
        .map(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some())
        .unwrap_or(false)
}

impl AddDialog {
    pub fn new(prefill: AddPrefill, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let engine = app::engine(cx);
        let (initial_url, base_request, captured_name, description) = match prefill {
            AddPrefill::FromClipboard => {
                let clip = cx
                    .read_from_clipboard()
                    .and_then(|c| c.text())
                    .map(|t| t.trim().to_string())
                    .filter(|t| is_http_url(t))
                    .unwrap_or_default();
                (clip, RequestSpec::default(), None, String::new())
            }
            AddPrefill::Url(url) => (url, RequestSpec::default(), None, String::new()),
            AddPrefill::Captured(request) => {
                let request = *request;
                (request.request.url.clone(), request.request, request.file_name, request.description)
            }
        };
        let captured = !base_request.url.is_empty();

        let url = cx.new(|cx| TextInput::new(cx, "https://example.com/file.zip").with_text(initial_url));
        let username = cx.new(|cx| TextInput::new(cx, "User name"));
        let password = cx.new(|cx| TextInput::new(cx, "Password").masked());
        let file_name = cx.new(|cx| TextInput::new(cx, "File name"));
        let folder = cx.new(|cx| TextInput::new(cx, "Folder"));
        let description = cx.new(|cx| TextInput::new(cx, "Optional").with_text(description));

        let subscriptions = vec![
            cx.subscribe_in(&url, window, |this, _, event, window, cx| match event {
                InputEvent::Submit => this.check_address(window, cx),
                InputEvent::Cancel => window.remove_window(),
                InputEvent::Changed => {}
            }),
            cx.subscribe_in(&password, window, |this, _, event, window, cx| {
                if let InputEvent::Submit = event {
                    this.check_address(window, cx)
                }
            }),
            cx.subscribe_in(&file_name, window, |this, _, event, window, cx| match event {
                InputEvent::Changed => this.on_name_changed(cx),
                InputEvent::Submit => this.add(true, window, cx),
                InputEvent::Cancel => window.remove_window(),
            }),
            cx.subscribe_in(&folder, window, |this, _, event, window, cx| match event {
                InputEvent::Changed => this.folder_chosen = true,
                InputEvent::Submit => this.add(true, window, cx),
                InputEvent::Cancel => window.remove_window(),
            }),
            cx.subscribe_in(&description, window, |this, _, event, window, cx| match event {
                InputEvent::Submit => this.add(true, window, cx),
                InputEvent::Cancel => window.remove_window(),
                InputEvent::Changed => {}
            }),
        ];

        let mut this = Self {
            engine,
            stage: Stage::EnterUrl,
            url,
            use_auth: false,
            username,
            password,
            file_name,
            folder,
            description,
            category: GENERAL.to_string(),
            category_chosen: false,
            folder_chosen: false,
            base_request,
            captured_name,
            probe: None,
            probe_error: None,
            error: None,
            _subscriptions: subscriptions,
        };
        if captured {
            this.check_address(window, cx);
        } else {
            let url = this.url.clone();
            url.update(cx, |input, cx| input.select_all_text(cx));
            window.focus(&url.read(cx).focus_handle(cx));
            this.resize(window);
        }
        this
    }

    fn resize(&self, window: &mut Window) {
        let height = match self.stage {
            Stage::EnterUrl | Stage::Probing if self.use_auth => 330.,
            Stage::EnterUrl | Stage::Probing => 230.,
            Stage::Info => 450.,
        };
        window.resize(size(px(560.), px(height)));
    }

    fn request_spec(&self, cx: &Context<Self>) -> RequestSpec {
        let mut spec = self.base_request.clone();
        spec.url = self.url.read(cx).text().trim().to_string();
        spec.credentials = self.use_auth.then(|| Credentials {
            username: self.username.read(cx).text(),
            password: self.password.read(cx).text(),
        });
        spec
    }

    /// Validates the address and asks the server about the file.
    pub(crate) fn check_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let spec = self.request_spec(cx);
        if !is_http_url(&spec.url) {
            self.error = Some("Enter a web address starting with http:// or https://".into());
            cx.notify();
            return;
        }
        self.error = None;
        self.stage = Stage::Probing;
        cx.notify();
        let probe = self.engine.probe(spec.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = probe.await;
            this.update_in(cx, |this, window, cx| this.on_probed(spec, result, window, cx)).ok();
        })
        .detach();
    }

    fn on_probed(
        &mut self,
        spec: RequestSpec,
        result: Result<ProbeInfo, tachyon_engine::EngineError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = match &result {
            Ok(info) => {
                self.probe = Some(info.clone());
                self.probe_error = None;
                info.file_name.clone()
            }
            Err(e) => {
                self.probe = None;
                self.probe_error = Some(e.to_string());
                derive_file_name(None, &spec.url, None)
            }
        };
        let name = self.captured_name.clone().unwrap_or(name);
        let (category, folder) = self.engine.default_target(&name);
        self.category = category;
        self.file_name.update(cx, |input, cx| input.set_text(name, cx));
        self.folder.update(cx, |input, cx| input.set_text(display_path(&folder), cx));
        self.folder_chosen = false;
        self.category_chosen = false;
        self.stage = Stage::Info;
        self.resize(window);
        let focus = self.file_name.read(cx).focus_handle(cx);
        window.focus(&focus);
        cx.notify();
    }

    fn on_name_changed(&mut self, cx: &mut Context<Self>) {
        if self.category_chosen || self.stage != Stage::Info {
            return;
        }
        let name = self.file_name.read(cx).text();
        let (category, folder) = self.engine.default_target(&name);
        if category != self.category {
            self.category = category;
            if !self.folder_chosen {
                self.folder.update(cx, |input, cx| input.set_text(display_path(&folder), cx));
                self.folder_chosen = false;
            }
            cx.notify();
        }
    }

    fn choose_category(&mut self, category: String, cx: &mut Context<Self>) {
        self.category = category.clone();
        self.category_chosen = true;
        if !self.folder_chosen {
            let folder = self.engine.folder_for_category(&category);
            self.folder.update(cx, |input, cx| input.set_text(display_path(&folder), cx));
            self.folder_chosen = false;
        }
        cx.notify();
    }

    fn browse_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                    this.folder.update(cx, |input, cx| input.set_text(display_path(&path), cx));
                    this.folder_chosen = true;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub(crate) fn add(&mut self, start: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.stage != Stage::Info {
            return;
        }
        let name = self.file_name.read(cx).text().trim().to_string();
        if name.is_empty() {
            self.error = Some("Enter a file name.".into());
            cx.notify();
            return;
        }
        let folder = self.folder.read(cx).text();
        let request = AddRequest {
            request: self.request_spec(cx),
            file_name: Some(name),
            directory: Some(expand_home(folder.trim())),
            category: Some(self.category.clone()),
            description: self.description.read(cx).text(),
            start,
            queue: !start,
            probed: self.probe.clone(),
        };
        match self.engine.add(request) {
            Ok(id) => {
                if start && self.engine.settings().show_progress_window {
                    app::open_progress_window(id, cx);
                }
                window.remove_window();
            }
            Err(e) => {
                self.error = Some(e.to_string());
                cx.notify();
            }
        }
    }

    fn labeled(label: &'static str, field: impl IntoElement, theme: &Theme) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(div().w(px(84.)).flex_none().text_color(theme.muted).child(label))
            .child(div().flex_1().min_w_0().child(field))
    }

    fn render_enter_url(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child("Enter the address to download"))
            .child(Self::labeled("Address", self.url.clone(), theme))
            .child(
                div().pl(px(96.)).child(
                    Checkbox::new("use-auth", "Use authorization", self.use_auth).on_toggle(cx.listener(
                        |this, checked: &bool, window, cx| {
                            this.use_auth = *checked;
                            this.resize(window);
                            cx.notify();
                        },
                    )),
                ),
            )
            .when(self.use_auth, |this| {
                this.child(Self::labeled("Login", self.username.clone(), theme))
                    .child(Self::labeled("Password", self.password.clone(), theme))
            })
            .into_any_element()
    }

    fn render_info(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let url = self.url.read(cx).text();
        let (size_text, resume_ok) = match &self.probe {
            Some(info) => (
                info.total.map(format::bytes).unwrap_or_else(|| "Unknown size".into()),
                Some(info.resumable),
            ),
            None => ("Unknown size".into(), None),
        };
        let settings = self.engine.settings();
        let mut categories: Vec<String> = settings.categories.iter().map(|c| c.name.clone()).collect();
        categories.push(GENERAL.to_string());

        let info_line = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(12.))
            .py(px(10.))
            .rounded(px(8.))
            .bg(theme.surface)
            .child(icon(Icon::for_category(&self.category), px(28.), theme.accent))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(div().w_full().text_color(theme.muted).text_size(px(12.)).child(truncated(url)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().font_weight(FontWeight::MEDIUM).child(size_text))
                            .child(match resume_ok {
                                Some(true) => div()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .text_size(px(12.))
                                    .text_color(theme.success)
                                    .child(icon(Icon::Check, px(12.), theme.success))
                                    .child("Resume supported"),
                                Some(false) => div()
                                    .text_size(px(12.))
                                    .text_color(theme.warning)
                                    .child("Resume not supported · single connection"),
                                None => div(),
                            }),
                    ),
            );

        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child("Download File Info"))
            .child(info_line)
            .when_some(self.probe_error.as_ref(), |this, e| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .text_color(theme.warning)
                        .child(icon(Icon::Alert, px(14.), theme.warning))
                        .child(format!("Couldn't check the file ({e}). You can still try downloading it.")),
                )
            })
            .child(Self::labeled(
                "Category",
                div().flex().flex_wrap().gap(px(6.)).children(categories.into_iter().map(|c| {
                    let active = c == self.category;
                    let theme = *theme;
                    div()
                        .id(SharedString::from(format!("cat-{c}")))
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .h(px(24.))
                        .px(px(9.))
                        .rounded(px(12.))
                        .border_1()
                        .text_size(px(12.))
                        .cursor_pointer()
                        .border_color(if active { theme.accent } else { theme.border_strong })
                        .bg(if active { theme.accent_soft } else { theme.bg })
                        .text_color(if active { theme.accent } else { theme.text })
                        .child(icon(Icon::for_category(&c), px(12.), if active { theme.accent } else { theme.muted }))
                        .child(c.clone())
                        .on_click(cx.listener(move |this, _, _, cx| this.choose_category(c.clone(), cx)))
                })),
                theme,
            ))
            .child(Self::labeled(
                "Save to",
                div()
                    .flex()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().child(self.folder.clone()))
                    .child(
                        Button::new("browse", "Choose…")
                            .icon(Icon::Folder)
                            .on_click(cx.listener(|this, _, window, cx| this.browse_folder(window, cx))),
                    ),
                theme,
            ))
            .child(Self::labeled("File name", self.file_name.clone(), theme))
            .child(Self::labeled("Description", self.description.clone(), theme))
            .into_any_element()
    }
}

impl Render for AddDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(window);
        let body = match self.stage {
            Stage::EnterUrl => self.render_enter_url(&theme, cx),
            Stage::Probing => div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(self.render_enter_url(&theme, cx))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_color(theme.muted)
                        .child(icon(Icon::Globe, px(14.), theme.accent))
                        .child("Contacting the server…"),
                )
                .into_any_element(),
            Stage::Info => self.render_info(&theme, cx),
        };

        let buttons: Vec<AnyElement> = match self.stage {
            Stage::EnterUrl | Stage::Probing => vec![
                Button::new("cancel", "Cancel")
                    .on_click(|_, window, _| window.remove_window())
                    .into_any_element(),
                Button::new("ok", "OK")
                    .primary()
                    .disabled(self.stage == Stage::Probing)
                    .on_click(cx.listener(|this, _, window, cx| this.check_address(window, cx)))
                    .into_any_element(),
            ],
            Stage::Info => vec![
                Button::new("cancel", "Cancel")
                    .on_click(|_, window, _| window.remove_window())
                    .into_any_element(),
                Button::new("later", "Download Later")
                    .icon(Icon::List)
                    .on_click(cx.listener(|this, _, window, cx| this.add(false, window, cx)))
                    .into_any_element(),
                Button::new("start", "Start Download")
                    .primary()
                    .icon(Icon::ArrowDown)
                    .on_click(cx.listener(|this, _, window, cx| this.add(true, window, cx)))
                    .into_any_element(),
            ],
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
                    .flex_1()
                    .min_h_0()
                    .p(px(20.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(body)
                    .when_some(self.error.as_ref(), |this, e| {
                        this.child(div().text_color(theme.danger).text_size(px(12.)).child(e.clone()))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .px(px(20.))
                    .py(px(12.))
                    .bg(theme.chrome)
                    .border_t_1()
                    .border_color(theme.border)
                    .child(div().flex_1())
                    .children(buttons),
            )
    }
}
