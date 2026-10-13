pub mod text_input;

use std::rc::Rc;

use gpui::{
    AnyView, App, Bounds, ClickEvent, Context, ElementId, FontWeight, Hsla, Pixels,
    SharedString, Window, canvas, div, fill, point, prelude::*, px, size, svg,
};
use tachyon_engine::SegmentState;

use crate::assets::Icon;
use crate::theme::Theme;

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type ToggleHandler = Rc<dyn Fn(&bool, &mut Window, &mut App)>;

pub fn icon(icon: Icon, size: Pixels, color: Hsla) -> impl IntoElement {
    svg().path(icon.path()).size(size).flex_none().text_color(color)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
}

/// Push button.
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: SharedString,
    icon: Option<Icon>,
    kind: ButtonKind,
    disabled: bool,
    compact: bool,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            kind: ButtonKind::Secondary,
            disabled: false,
            compact: false,
            on_click: None,
        }
    }

    pub fn primary(mut self) -> Self {
        self.kind = ButtonKind::Primary;
        self
    }

    pub fn ghost(mut self) -> Self {
        self.kind = ButtonKind::Ghost;
        self
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(window);
        let (bg, hover, fg, border) = match self.kind {
            ButtonKind::Primary => (theme.accent, theme.accent_hover, theme.on_accent, theme.accent),
            ButtonKind::Secondary => (theme.button, theme.button_hover, theme.text, theme.border_strong),
            ButtonKind::Ghost => (gpui::transparent_black(), theme.hover, theme.text, gpui::transparent_black()),
        };
        let disabled = self.disabled;
        let handler = self.on_click.clone();
        div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .h(px(if self.compact { 24. } else { 28. }))
            .px(px(if self.compact { 8. } else { 14. }))
            .rounded(px(6.))
            .border_1()
            .border_color(border)
            .bg(bg)
            .text_color(fg)
            .text_size(px(13.))
            .font_weight(if self.kind == ButtonKind::Primary { FontWeight::MEDIUM } else { FontWeight::NORMAL })
            .when(self.kind != ButtonKind::Ghost, |this| this.shadow_xs())
            .when(disabled, |this| this.opacity(0.45))
            .when(!disabled, |this| {
                this.cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .active(|s| s.opacity(0.85))
                    .when_some(handler, |this, handler| {
                        this.on_click(move |event, window, cx| handler(event, window, cx))
                    })
            })
            .when_some(self.icon, |this, i| this.child(icon(i, px(14.), fg)))
            .child(self.label)
    }
}

/// Large IDM-style toolbar button: icon above a caption.
#[derive(IntoElement)]
pub struct ToolbarButton {
    id: ElementId,
    label: SharedString,
    icon: Icon,
    tint: Option<Hsla>,
    disabled: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl ToolbarButton {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, icon: Icon) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon,
            tint: None,
            disabled: false,
            tooltip: None,
            on_click: None,
        }
    }

    pub fn tint(mut self, tint: Hsla) -> Self {
        self.tint = Some(tint);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for ToolbarButton {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(window);
        let color = if self.disabled { theme.faint } else { self.tint.unwrap_or(theme.text) };
        let handler = self.on_click.clone();
        let disabled = self.disabled;
        div()
            .id(self.id)
            .flex()
            .flex_col()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(px(3.))
            .min_w(px(62.))
            .h(px(54.))
            .px(px(6.))
            .rounded(px(8.))
            .text_size(px(11.))
            .text_color(if disabled { theme.faint } else { theme.text })
            .when(!disabled, |this| {
                this.cursor_pointer()
                    .hover(|s| s.bg(theme.hover))
                    .active(|s| s.bg(theme.pressed))
                    .when_some(handler, |this, handler| {
                        this.on_click(move |event, window, cx| handler(event, window, cx))
                    })
            })
            .when_some(self.tooltip, |this, text| this.tooltip(Tooltip::text(text)))
            .child(icon(self.icon, px(22.), color))
            .child(div().whitespace_nowrap().child(self.label))
    }
}

/// Checkbox with a label.
#[derive(IntoElement)]
pub struct Checkbox {
    id: ElementId,
    label: SharedString,
    checked: bool,
    on_toggle: Option<ToggleHandler>,
}

impl Checkbox {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, checked: bool) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            checked,
            on_toggle: None,
        }
    }

    pub fn on_toggle(mut self, f: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for Checkbox {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(window);
        let checked = self.checked;
        let handler = self.on_toggle.clone();
        div()
            .id(self.id)
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(13.))
            .text_color(theme.text)
            .cursor_pointer()
            .when_some(handler, |this, handler| {
                this.on_click(move |_, window, cx| handler(&!checked, window, cx))
            })
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(16.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(if checked { theme.accent } else { theme.border_strong })
                    .bg(if checked { theme.accent } else { theme.bg })
                    .when(checked, |this| this.child(icon(Icon::Check, px(12.), theme.on_accent))),
            )
            .child(self.label)
    }
}

/// One-of-many choice rendered as radio buttons.
pub fn radio_group<T: Copy + PartialEq + 'static>(
    id: &'static str,
    options: Vec<(T, SharedString)>,
    selected: T,
    theme: &Theme,
    on_select: impl Fn(&T, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_select = Rc::new(on_select);
    let theme = *theme;
    div().flex().flex_col().gap(px(6.)).children(options.into_iter().enumerate().map(
        move |(ix, (value, label))| {
            let on_select = on_select.clone();
            let active = value == selected;
            div()
                .id((id, ix))
                .flex()
                .items_center()
                .gap(px(8.))
                .text_size(px(13.))
                .text_color(theme.text)
                .cursor_pointer()
                .on_click(move |_, window, cx| on_select(&value, window, cx))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(16.))
                        .rounded_full()
                        .border_1()
                        .border_color(if active { theme.accent } else { theme.border_strong })
                        .bg(theme.bg)
                        .when(active, |this| this.child(div().size(px(8.)).rounded_full().bg(theme.accent))),
                )
                .child(label)
        },
    ))
}

/// Segmented tab strip.
pub fn tabs(
    id: &'static str,
    labels: &[&'static str],
    selected: usize,
    theme: &Theme,
    on_select: impl Fn(&usize, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_select = Rc::new(on_select);
    let theme = *theme;
    div()
        .flex()
        .flex_none()
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(8.))
        .bg(theme.track)
        .children(labels.iter().enumerate().map(|(ix, label)| {
            let on_select = on_select.clone();
            let active = ix == selected;
            div()
                .id((id, ix))
                .flex()
                .items_center()
                .justify_center()
                .px(px(12.))
                .h(px(24.))
                .rounded(px(6.))
                .text_size(px(12.))
                .cursor_pointer()
                .text_color(if active { theme.text } else { theme.muted })
                .when(active, |this| this.bg(theme.bg).shadow_xs().font_weight(FontWeight::MEDIUM))
                .when(!active, |this| this.hover(|s| s.text_color(theme.text)))
                .on_click(move |_, window, cx| on_select(&ix, window, cx))
                .child(*label)
        }))
}

pub fn progress_bar(progress: Option<f64>, height: Pixels, color: Hsla, theme: &Theme) -> impl IntoElement {
    let fraction = progress.unwrap_or(0.0).clamp(0.0, 1.0) as f32;
    div()
        .w_full()
        .h(height)
        .rounded(height / 2.)
        .bg(theme.track)
        .overflow_hidden()
        .child(div().h_full().w(gpui::relative(fraction)).rounded(height / 2.).bg(color))
}

/// IDM's segment map: one bar for the file with each segment's received part filled in.
pub fn segment_bar(segments: Vec<SegmentState>, total: Option<u64>, theme: &Theme) -> impl IntoElement {
    let theme = *theme;
    let total = total.or_else(|| segments.iter().map(|s| s.end).max()).unwrap_or(0);
    canvas(
        |_, _, _| (),
        move |bounds: Bounds<Pixels>, _, window, _| {
            window.paint_quad(fill(bounds, theme.track));
            if total == 0 || total == u64::MAX {
                return;
            }
            let width = f64::from(bounds.size.width);
            let x_of = |byte: u64| bounds.left() + px((byte as f64 / total as f64 * width) as f32);
            for segment in &segments {
                let end = segment.pos.min(segment.end);
                if end <= segment.start {
                    continue;
                }
                let (x0, x1) = (x_of(segment.start), x_of(end));
                let w = (x1 - x0).max(px(1.));
                window.paint_quad(fill(
                    Bounds::new(point(x0, bounds.top()), size(w, bounds.size.height)),
                    theme.accent,
                ));
            }
            // Boundaries between segments.
            for segment in segments.iter().skip(1) {
                window.paint_quad(fill(
                    Bounds::new(point(x_of(segment.start), bounds.top()), size(px(1.), bounds.size.height)),
                    theme.bg,
                ));
            }
        },
    )
    .w_full()
    .h(px(14.))
    .rounded(px(3.))
    .overflow_hidden()
}

/// "Label:  value" row used by the progress and properties dialogs.
pub fn field_row(label: impl Into<SharedString>, value: impl IntoElement, theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .min_h(px(22.))
        .text_size(px(13.))
        .child(
            div()
                .w(px(120.))
                .flex_none()
                .text_color(theme.muted)
                .child(label.into()),
        )
        .child(div().flex_1().min_w_0().overflow_hidden().truncate().text_color(theme.text).child(value))
}

pub fn section_title(text: impl Into<SharedString>, theme: &Theme) -> impl IntoElement {
    div()
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.muted)
        .child(text.into().to_uppercase())
}

pub fn separator(theme: &Theme) -> impl IntoElement {
    div().h(px(1.)).w_full().flex_none().bg(theme.border)
}

pub struct Tooltip {
    text: SharedString,
}

impl Tooltip {
    pub fn text(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView {
        let text = text.into();
        move |_, cx| cx.new(|_| Tooltip { text: text.clone() }).into()
    }
}

impl Render for Tooltip {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(window);
        div().pl(px(8.)).pt(px(10.)).child(
            div()
                .px(px(8.))
                .py(px(4.))
                .rounded(px(6.))
                .bg(theme.chrome)
                .border_1()
                .border_color(theme.border)
                .shadow_md()
                .text_size(px(12.))
                .text_color(theme.text)
                .child(self.text.clone()),
        )
    }
}

/// Number with − / + buttons.
pub fn stepper(
    id: &'static str,
    value: u32,
    min: u32,
    max: u32,
    theme: &Theme,
    on_change: impl Fn(&u32, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_change = Rc::new(on_change);
    let theme = *theme;
    let step = move |delta: i64, key: &'static str| {
        let on_change = on_change.clone();
        let next = (i64::from(value) + delta).clamp(i64::from(min), i64::from(max)) as u32;
        let enabled = next != value;
        div()
            .id((id, if delta < 0 { 0usize } else { 1usize }))
            .flex()
            .items_center()
            .justify_center()
            .w(px(26.))
            .h_full()
            .text_color(if enabled { theme.text } else { theme.faint })
            .when(enabled, |this| {
                this.cursor_pointer()
                    .hover(|s| s.bg(theme.hover))
                    .on_click(move |_, window, cx| on_change(&next, window, cx))
            })
            .child(key)
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(28.))
        .rounded(px(6.))
        .border_1()
        .border_color(theme.border_strong)
        .bg(theme.bg)
        .overflow_hidden()
        .child(step(-1, "−"))
        .child(
            div()
                .min_w(px(40.))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .border_l_1()
                .border_r_1()
                .border_color(theme.border)
                .text_size(px(13.))
                .child(value.to_string()),
        )
        .child(step(1, "+"))
}

/// Single line of text that fills its parent's width and ends in "…" when too long.
///
/// GPUI 0.2's `truncate()` caches the unconstrained measurement inside flex rows and
/// then never truncates, so this element truncates against its painted bounds instead.
pub struct TruncatedText {
    text: SharedString,
}

pub fn truncated(text: impl Into<SharedString>) -> TruncatedText {
    TruncatedText { text: text.into() }
}

impl IntoElement for TruncatedText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Element for TruncatedText {
    type RequestLayoutState = ();
    type PrepaintState = Option<gpui::ShapedLine>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        let mut style = gpui::Style::default();
        style.size.width = gpui::relative(1.).into();
        style.min_size.width = px(0.).into();
        style.size.height = window.line_height().into();
        style.flex_grow = 1.;
        style.flex_shrink = 1.;
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let mut runs = vec![style.to_run(self.text.len())];
        let text = window
            .text_system()
            .line_wrapper(style.font(), font_size)
            .truncate_line(self.text.clone(), bounds.size.width, "…", &mut runs);
        Some(window.text_system().shape_line(text, font_size, &runs, None))
    }

    fn paint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        line: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(line) = line.take() {
            line.paint(bounds.origin, window.line_height(), window, cx).ok();
        }
    }
}
