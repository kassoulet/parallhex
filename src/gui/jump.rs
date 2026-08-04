//! The jump-to-offset dialog's single-line text field.
//!
//! gpui 0.2 has no built-in text input, so this implements `EntityInputHandler`
//! plus a hand-written `Element` that shapes the line and paints the caret.

use std::ops::Range;

use gpui::{
    App, Bounds, Context, CursorStyle, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, Hsla, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ShapedLine,
    SharedString, Style, TextRun, UTF16Selection, Window, div, fill, point, prelude::*, px,
    relative, rgb, rgba, size,
};

use crate::gui::{Backspace, Delete, JumpCancel, JumpSubmit, NavigateLeft, NavigateRight, Paste};

/// Events emitted by the jump field to its parent view.
#[derive(Clone, Debug)]
pub(crate) enum JumpFieldEvent {
    Submit(String),
    Cancel,
}

impl EventEmitter<JumpFieldEvent> for JumpField {}

/// A minimal single-line text field used by the jump-to-offset dialog. It
/// implements `EntityInputHandler` so the platform IME / typing works, and
/// handles Backspace/Delete/arrow keys via actions.
pub(crate) struct JumpField {
    focus_handle: FocusHandle,
    content: SharedString,
    selected_range: Range<usize>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
}

impl JumpField {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: SharedString::new_static(""),
            selected_range: 0..0,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
        }
    }

    /// The current text, for the parent view's live preview and submit.
    pub(crate) fn content(&self) -> &str {
        &self.content
    }

    pub(crate) fn set_content(&mut self, s: &str) {
        self.content = s.to_string().into();
        self.selected_range = s.len()..s.len();
    }

    fn cursor_offset(&self) -> usize {
        if self.selected_range.is_empty() {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn move_cursor_left(&mut self, cx: &mut Context<Self>) {
        let start = self.previous_boundary(self.cursor_offset());
        self.selected_range = start..start;
        cx.notify();
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .char_indices()
            .take_while(|(i, _)| *i < offset)
            .map(|(i, c)| i + c.len_utf8())
            .last()
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .char_indices()
            .find_map(|(i, _c)| (i > offset).then_some(i))
            .unwrap_or(self.content.len())
    }

    fn move_cursor_right(&mut self, cx: &mut Context<Self>) {
        let next = self.next_boundary(self.cursor_offset());
        self.selected_range = next..next;
        cx.notify();
    }

    fn on_backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.selected_range =
                self.previous_boundary(self.cursor_offset())..self.cursor_offset();
        }
        self.replace_range(&self.selected_range.clone(), "");
        cx.notify();
    }

    fn on_delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let end = self.next_boundary(self.cursor_offset());
            self.selected_range = self.cursor_offset()..end;
        }
        self.replace_range(&self.selected_range.clone(), "");
        cx.notify();
    }

    fn on_navigate_left(&mut self, _: &NavigateLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor_left(cx);
    }

    fn on_navigate_right(&mut self, _: &NavigateRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor_right(cx);
    }

    fn on_paste(&mut self, _: &Paste, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = cx.read_from_clipboard()
            && let Some(text) = item.text()
        {
            let range = self.selected_range.clone();
            self.replace_range(&range, &text);
            cx.notify();
        }
    }

    fn on_submit(&mut self, _: &JumpSubmit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(JumpFieldEvent::Submit(self.content.to_string()));
    }

    #[allow(clippy::unused_self)] // signature shape required by cx.listener
    fn on_cancel(&mut self, _: &JumpCancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(JumpFieldEvent::Cancel);
    }

    /// Replace `range` with `text` and move the cursor to the end of it.
    fn replace_range(&mut self, range: &Range<usize>, text: &str) {
        let start = range.start.min(self.content.len());
        let end = range.end.min(self.content.len());
        self.content = (self.content[0..start].to_owned() + text + &self.content[end..]).into();
        let cursor = start + text.len();
        self.selected_range = cursor..cursor;
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.is_selecting = true;
        let index = self
            .last_bounds
            .as_ref()
            .and_then(|bounds| self.index_for_point(event.position, *bounds))
            .unwrap_or(self.content.len());
        self.selected_range = index..index;
        cx.notify();
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting
            && let Some(bounds) = self.last_bounds
            && let Some(index) = self.index_for_point(event.position, bounds)
        {
            let start = self.selected_range.start;
            self.selected_range = start.min(index)..start.max(index);
            cx.notify();
        }
    }

    fn index_for_point(&self, point: Point<Pixels>, bounds: Bounds<Pixels>) -> Option<usize> {
        let local = bounds.localize(&point)?;
        let layout = self.last_layout.as_ref()?;
        layout.index_for_x(local.x)
    }
}

impl Focusable for JumpField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for JumpField {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = range_from_utf16(&self.content, range_utf16);
        adjusted_range.replace(range_to_utf16(&self.content, range.clone()));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: range_to_utf16(&self.content, self.selected_range.clone()),
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16.map_or_else(
            || self.selected_range.clone(),
            |r| range_from_utf16(&self.content, r),
        );
        self.replace_range(&range, text);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16.map_or_else(
            || self.selected_range.clone(),
            |r| range_from_utf16(&self.content, r),
        );
        self.replace_range(&range, text);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(bounds)
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let index = self.index_for_point(point, bounds)?;
        Some(range_to_utf16(&self.content, index..index).start)
    }
}

/// Convert a UTF-16-based range to a UTF-8 byte range.
fn range_from_utf16(s: &str, range: Range<usize>) -> Range<usize> {
    let mut start = s.len();
    let mut end = s.len();
    let mut units = 0usize;
    for (i, ch) in s.char_indices() {
        if units == range.start {
            start = i;
        }
        if units == range.end {
            end = i;
            break;
        }
        units += ch.len_utf16();
    }
    start..end
}

/// Convert a UTF-8 byte range to a UTF-16-based range.
fn range_to_utf16(s: &str, range: Range<usize>) -> Range<usize> {
    let mut units = 0usize;
    let mut start = 0usize;
    let mut end = s.encode_utf16().count();
    for (i, ch) in s.char_indices() {
        if i == range.start {
            start = units;
        }
        if i == range.end {
            end = units;
            break;
        }
        units += ch.len_utf16();
    }
    start..end
}

/// The element that renders the field's text + cursor and wires the IME
/// input handler.
struct JumpFieldElement {
    field: Entity<JumpField>,
}

struct JumpFieldPrepaint {
    line: Option<ShapedLine>,
    cursor: Option<gpui::PaintQuad>,
}

impl IntoElement for JumpFieldElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for JumpFieldElement {
    type RequestLayoutState = ();
    type PrepaintState = JumpFieldPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = px(22.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let field = self.field.read(cx);
        let content = field.content.clone();
        let cursor = field.cursor_offset();
        let placeholder = content.is_empty();

        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let text_color = if placeholder {
            Hsla::from(rgba(0x565f8980))
        } else {
            style.color
        };
        let run = TextRun {
            len: if placeholder { 6 } else { content.len() },
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let display = if placeholder {
            SharedString::new_static("0x1000")
        } else {
            content.clone()
        };
        let line = window
            .text_system()
            .shape_line(display, font_size, &[run], None);

        let cursor_x = line.x_for_index(cursor);
        let cursor_quad = fill(
            Bounds::new(
                point(bounds.left() + cursor_x, bounds.top()),
                size(px(2.), bounds.size.height),
            ),
            rgb(0x7aa2f7),
        );

        window.handle_input(
            &field.focus_handle,
            ElementInputHandler::new(bounds, self.field.clone()),
            cx,
        );

        JumpFieldPrepaint {
            line: Some(line),
            cursor: Some(cursor_quad),
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let line = prepaint.line.take();
        self.field.update(cx, |field, _cx| {
            field.last_layout.clone_from(&line);
            field.last_bounds = Some(bounds);
        });
        if let Some(cursor) = prepaint.cursor.take()
            && self.field.read(cx).focus_handle.is_focused(window)
        {
            window.paint_quad(cursor);
        }
        if let Some(line) = line {
            let _ = line.paint(bounds.origin, window.line_height(), window, cx);
        }
    }
}

impl Render for JumpField {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("JumpField")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::on_backspace))
            .on_action(cx.listener(Self::on_delete))
            .on_action(cx.listener(Self::on_navigate_left))
            .on_action(cx.listener(Self::on_navigate_right))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_submit))
            .on_action(cx.listener(Self::on_cancel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .bg(rgb(0x0f1017))
            .border_1()
            .border_color(rgb(0x3b4261))
            .rounded_md()
            .w_full()
            .h(px(26.))
            .px_2()
            .child(JumpFieldElement { field: cx.entity() })
    }
}
