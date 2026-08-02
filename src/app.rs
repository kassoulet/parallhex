//! Application state and the gpui view shell.

use std::fs::File;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, Background, BorderStyle, Bounds, ClickEvent, ClipboardItem, Context, CursorStyle, Element,
    ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, GlobalElementId, Hsla, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Render, RenderImage, ScrollDelta, ScrollWheelEvent, ShapedLine,
    SharedString, Style, TextRun, UTF16Selection, Window, canvas, div, fill, font, point,
    prelude::*, px, quad, relative, rgb, rgba, size, transparent_black,
};

use memmap2::{Mmap, MmapOptions};

use crate::color::{self, Colormap};
use crate::config;
use crate::entropy;
use crate::panes;
use crate::{
    Backspace, ClearSelection, CopySelectionAscii, CopySelectionHex, Delete, JumpCancel,
    JumpSubmit, JumpToOffset, NavigateDown, NavigateEnd, NavigateHome, NavigateLeft,
    NavigatePageDown, NavigatePageUp, NavigateRight, NavigateUp, OpenFile, Paste, ResetSettings,
    ResetView, ZoomIn, ZoomOut,
};

/// Size of the horizontal whole-file preview strip in the top bar.
const STRIP_W: f32 = 320.0;
const STRIP_H: f32 = 36.0;

/// The navigation key pressed this frame (one-hot).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NavigationAction {
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

impl NavigationAction {
    #[cfg_attr(not(test), allow(dead_code))]
    #[allow(clippy::fn_params_excessive_bools)] // one-hot test helper
    fn new(
        left: bool,
        right: bool,
        up: bool,
        down: bool,
        page_up: bool,
        page_down: bool,
        home: bool,
    ) -> Self {
        if left {
            Self::Left
        } else if right {
            Self::Right
        } else if up {
            Self::Up
        } else if down {
            Self::Down
        } else if page_up {
            Self::PageUp
        } else if page_down {
            Self::PageDown
        } else if home {
            Self::Home
        } else {
            // Only reachable when a navigation key is held: End.
            Self::End
        }
    }
}

// ---------------------------------------------------------------------------
// Jump dialog text field
// ---------------------------------------------------------------------------

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
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: SharedString::new_static(""),
            selected_range: 0..0,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
        }
    }

    fn set_content(&mut self, s: &str) {
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

// ---------------------------------------------------------------------------
// The main view
// ---------------------------------------------------------------------------

/// Which slider the pointer is currently dragging.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SliderKind {
    HexZoom,
    PixelZoom,
    EntropyWindow,
}

/// Which column divider the pointer is currently dragging.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DividerKind {
    OverviewPixels,
    PixelsHex,
}

/// Clamp ranges for the two drag-resizable column widths.
const OVERVIEW_W_MIN: f32 = 140.0;
const OVERVIEW_W_MAX: f32 = 2000.0;
const PIXELS_W_MIN: f32 = 200.0;
const PIXELS_W_MAX: f32 = 3000.0;

/// The many booleans track per-column drag/interaction bookkeeping; a
/// state machine would add more complexity than it removes.
#[allow(clippy::struct_excessive_bools)]
pub struct ParallHexApp {
    pub file_path: Option<PathBuf>,
    pub mmap: Option<Arc<Mmap>>,
    pub file_size: usize,

    // Hex-viewer parameters
    pub bytes_per_row: usize,
    pub entropy_window: usize,
    pub pixel_colormap: Colormap,

    // One-shot: jump the central scroll area back to the top.
    pub scroll_reset: bool,
    // One-shot: scroll the central view to a specific file offset.
    pub scroll_to_offset: Option<usize>,

    // Cached Shannon entropy per `entropy_window`-sized block (whole file).
    // Arc so canvas paint closures can grab a cheap snapshot.
    pub entropies: Arc<Vec<f32>>,

    // Whole-file 2D overview (left panel) and horizontal preview strip,
    // generated as gpui RenderImages from downsampled byte data.
    pub overview_image: Option<Arc<RenderImage>>,
    pub overview_cells: Option<(usize, usize)>,
    pub overview_gen_size: Option<(usize, usize)>,
    pub strip_image: Option<Arc<RenderImage>>,
    pub strip_dirty: bool,

    // Three-column layout: per-column zoom, the shared scroll position (in
    // rows) and each column's last-frame canvas bounds (filled by the
    // canvases' prepaint callbacks, used for hit-testing).
    pub hex_zoom: f32,
    pub pixel_zoom: f32,
    pub scroll_rows: f32,
    pub hex_bounds: Bounds<Pixels>,
    pub pixels_bounds: Bounds<Pixels>,
    pub overview_bounds: Bounds<Pixels>,
    pub strip_bounds: Bounds<Pixels>,

    // Visible fraction of the file in the central view, for the overview
    // markers.
    pub view_frac: f32,
    pub view_frac_h: f32,
    pub view_height: f32,

    // Selection & hover state (shared by all panes).
    pub hovered_offset: Option<usize>,
    pub selected_offset: Option<usize>,
    pub selection_range: Option<Range<usize>>,
    pub drag_start: Option<usize>,

    // Per-column drag bookkeeping.
    pub hex_mouse_down: bool,
    pub pan_active: bool,
    pub pixels_dragging: bool,
    pub last_pixels_y: Option<f32>,
    pub overview_dragging: bool,

    // Offset under the pointer while hovering the overview previews
    // (top-bar strip / left overview).
    pub overview_hover_offset: Option<usize>,

    // Jump-to-offset dialog (Ctrl+G).
    pub show_jump_dialog: bool,
    pub jump_error: Option<String>,
    pub jump_field: Entity<JumpField>,

    // Persisted layout prefs (written a couple of seconds after a change).
    pub overview_width: f32,
    pub pixels_width: f32,
    pub saved_cfg: config::Config,
    pub last_save: Instant,

    // Drag state for the column divider resize handles.
    pub resizing_divider: Option<DividerKind>,
    pub divider_start_x: f32,
    pub divider_start_w: f32,

    // Header sliders.
    pub hex_slider_bounds: Bounds<Pixels>,
    pub pixels_slider_bounds: Bounds<Pixels>,
    pub entropy_slider_bounds: Bounds<Pixels>,
    pub dragging_slider: Option<SliderKind>,

    // Colormap selector dropdown in the pixels-column header.
    pub colormap_menu_open: bool,
    // True while a mouse-down that began on the dropdown toggle is in
    // flight, so the root's outside-click handler doesn't immediately close
    // the menu that the toggle's click is about to open or toggle closed.
    pub colormap_toggle_down: bool,

    pub mono_family: SharedString,
    pub message: Option<String>,
    pub focus_handle: FocusHandle,

    // Copy deferred from a right-click handler until cx is available.
    pub pending_copy: Option<String>,
}

impl ParallHexApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, initial_file: Option<PathBuf>) -> Self {
        let prefs = config::load();
        let focus_handle = cx.focus_handle();
        let jump_field = cx.new(JumpField::new);

        let mono_family = pick_monospace_family(window);
        let mut app = Self {
            file_path: None,
            mmap: None,
            file_size: 0,
            bytes_per_row: match prefs.bytes_per_row {
                16 | 32 | 64 => prefs.bytes_per_row,
                _ => 32,
            },
            entropy_window: prefs.entropy_window.clamp(16, 4096),
            pixel_colormap: prefs.pixel_colormap,
            scroll_reset: false,
            scroll_to_offset: None,
            entropies: Arc::new(Vec::new()),
            overview_image: None,
            overview_cells: None,
            overview_gen_size: None,
            strip_image: None,
            strip_dirty: false,
            hex_zoom: prefs
                .hex_zoom
                .clamp(panes::HEX_ZOOM_MIN, panes::HEX_ZOOM_MAX),
            pixel_zoom: prefs
                .pixel_zoom
                .clamp(panes::PIXEL_ZOOM_MIN, panes::PIXEL_ZOOM_MAX),
            scroll_rows: 0.0,
            hex_bounds: Bounds::default(),
            pixels_bounds: Bounds::default(),
            overview_bounds: Bounds::default(),
            strip_bounds: Bounds::default(),
            view_frac: 0.0,
            view_frac_h: 1.0,
            view_height: 600.0,
            hovered_offset: None,
            selected_offset: None,
            selection_range: None,
            drag_start: None,
            hex_mouse_down: false,
            pan_active: false,
            pixels_dragging: false,
            last_pixels_y: None,
            overview_dragging: false,
            overview_hover_offset: None,
            show_jump_dialog: false,
            jump_error: None,
            jump_field,
            overview_width: prefs.overview_width.clamp(OVERVIEW_W_MIN, OVERVIEW_W_MAX),
            pixels_width: prefs.pixels_width.clamp(PIXELS_W_MIN, PIXELS_W_MAX),
            saved_cfg: prefs,
            last_save: Instant::now(),
            resizing_divider: None,
            divider_start_x: 0.0,
            divider_start_w: 0.0,
            hex_slider_bounds: Bounds::default(),
            pixels_slider_bounds: Bounds::default(),
            entropy_slider_bounds: Bounds::default(),
            dragging_slider: None,
            colormap_menu_open: false,
            colormap_toggle_down: false,
            mono_family,
            message: None,
            focus_handle,
            pending_copy: None,
        };

        let jump_field = app.jump_field.clone();
        cx.subscribe(&jump_field, |this, _, event, cx| match event {
            JumpFieldEvent::Submit(text) => this.jump_submit(text, cx),
            JumpFieldEvent::Cancel => {
                this.show_jump_dialog = false;
                this.jump_error = None;
                cx.notify();
            }
        })
        .detach();
        cx.on_release(|this, _cx| {
            config::save(&this.current_config());
        })
        .detach();

        if let Some(path) = initial_file {
            app.load_file(path);
        }
        app
    }

    pub(crate) fn data(&self) -> Option<&[u8]> {
        self.mmap.as_ref().map(|m| &m[..])
    }

    fn recompute_entropies(&mut self) {
        self.entropies = Arc::new(match self.data() {
            Some(d) => entropy::block_entropies(d, self.entropy_window),
            None => Vec::new(),
        });
    }

    fn file_name_str(&self) -> String {
        self.file_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or_else(
                || "<no file>".to_owned(),
                |s| s.to_string_lossy().into_owned(),
            )
    }

    /// Snapshot the current layout prefs for saving (widths rounded to whole
    /// pixels so an unchanged layout doesn't rewrite the file).
    fn current_config(&self) -> config::Config {
        config::Config {
            bytes_per_row: self.bytes_per_row,
            entropy_window: self.entropy_window,
            hex_zoom: self.hex_zoom,
            pixel_zoom: self.pixel_zoom,
            pixel_colormap: self.pixel_colormap,
            overview_width: self.overview_width.round(),
            pixels_width: self.pixels_width.round(),
        }
    }

    /// Restore every persisted setting to its default.
    fn reset_all_settings(&mut self, cx: &mut Context<Self>) {
        let defaults = config::Config::default();
        self.bytes_per_row = defaults.bytes_per_row;
        self.entropy_window = defaults.entropy_window;
        self.hex_zoom = defaults.hex_zoom;
        self.pixel_zoom = defaults.pixel_zoom;
        self.overview_width = defaults.overview_width;
        self.pixels_width = defaults.pixels_width;
        self.scroll_reset = true;
        self.colormap_menu_open = false;
        self.recompute_entropies();
        self.overview_gen_size = None;
        self.strip_dirty = true;
        config::save(&defaults);
        self.saved_cfg = defaults;
        self.last_save = Instant::now();
        self.message = Some("Settings reset to defaults.".to_owned());
        cx.notify();
    }

    fn load_file(&mut self, path: PathBuf) {
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                self.message = Some(format!("Failed to open {}: {e}", path.display()));
                return;
            }
        };
        let len = match file.metadata() {
            Ok(m) => m.len() as usize,
            Err(e) => {
                self.message = Some(format!("Failed to read metadata: {e}"));
                return;
            }
        };
        if len == 0 {
            self.message = Some("File is empty.".to_owned());
            return;
        }
        let mmap = match unsafe { MmapOptions::new().map(&file) } {
            Ok(m) => m,
            Err(e) => {
                self.message = Some(format!("Failed to memory-map file: {e}"));
                return;
            }
        };
        self.file_path = Some(path);
        self.file_size = len;
        self.mmap = Some(Arc::new(mmap));
        self.recompute_entropies();
        self.overview_gen_size = None;
        self.overview_image = None;
        self.overview_cells = None;
        self.strip_image = None;
        self.strip_dirty = true;
        self.scroll_reset = true;
        self.scroll_rows = 0.0;
        self.scroll_to_offset = None;
        self.hovered_offset = None;
        self.selected_offset = None;
        self.selection_range = None;
        self.drag_start = None;
        self.overview_hover_offset = None;
        self.show_jump_dialog = false;
        self.jump_error = None;
        self.colormap_menu_open = false;
        self.message = None;
    }

    fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_file() {
            self.load_file(path);
        }
    }

    // ----- keyboard actions -----

    fn on_open_file(&mut self, _: &OpenFile, _: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog();
        cx.notify();
    }

    fn on_jump_to_offset(&mut self, _: &JumpToOffset, window: &mut Window, cx: &mut Context<Self>) {
        self.open_jump_dialog(window, cx);
    }

    fn on_reset_view(&mut self, _: &ResetView, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_reset = true;
        cx.notify();
    }

    fn on_reset_settings(&mut self, _: &ResetSettings, _: &mut Window, cx: &mut Context<Self>) {
        self.reset_all_settings(cx);
    }

    fn on_zoom_in(&mut self, _: &ZoomIn, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_jump_dialog {
            return;
        }
        self.zoom_under_pointer(window, panes::ZOOM_STEP);
        cx.notify();
    }

    fn on_zoom_out(&mut self, _: &ZoomOut, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_jump_dialog {
            return;
        }
        self.zoom_under_pointer(window, 1.0 / panes::ZOOM_STEP);
        cx.notify();
    }

    /// `+`/`-` zoom the column under the pointer (hex row height or pixel
    /// size), clamped to its range.
    fn zoom_under_pointer(&mut self, window: &Window, factor: f32) {
        let p = window.mouse_position();
        if self.hex_bounds.contains(&p) {
            self.hex_zoom = panes::zoom_step(
                self.hex_zoom,
                factor,
                panes::HEX_ZOOM_MIN,
                panes::HEX_ZOOM_MAX,
            );
        } else if self.pixels_bounds.contains(&p) {
            self.pixel_zoom = panes::zoom_step(
                self.pixel_zoom,
                factor,
                panes::PIXEL_ZOOM_MIN,
                panes::PIXEL_ZOOM_MAX,
            );
        }
    }

    fn on_nav_left(&mut self, _: &NavigateLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(NavigationAction::Left, cx);
    }
    fn on_nav_right(&mut self, _: &NavigateRight, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(NavigationAction::Right, cx);
    }
    fn on_nav_up(&mut self, _: &NavigateUp, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(NavigationAction::Up, cx);
    }
    fn on_nav_down(&mut self, _: &NavigateDown, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(NavigationAction::Down, cx);
    }
    fn on_nav_page_up(&mut self, _: &NavigatePageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(NavigationAction::PageUp, cx);
    }
    fn on_nav_page_down(&mut self, _: &NavigatePageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(NavigationAction::PageDown, cx);
    }
    fn on_nav_home(&mut self, _: &NavigateHome, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(NavigationAction::Home, cx);
    }
    fn on_nav_end(&mut self, _: &NavigateEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(NavigationAction::End, cx);
    }

    /// Move the selection with the keyboard and schedule a scroll-to-center.
    fn navigate(&mut self, action: NavigationAction, cx: &mut Context<Self>) {
        // The jump dialog owns the keyboard while it's open.
        if self.show_jump_dialog {
            return;
        }
        let len = match self.data() {
            Some(d) => d.len(),
            None => return,
        };
        if len == 0 {
            return;
        }
        let bpr = self.bytes_per_row.max(1);
        let page_rows = (self.view_height / panes::hex_row_h(self.hex_zoom)).max(1.0) as usize;
        let page_bytes = page_rows * bpr;

        // First navigation with no selection yet: honor Home/End, otherwise
        // place the cursor at offset 0.
        let cur = if let Some(c) = self.selected_offset {
            c.min(len - 1)
        } else {
            let start = if action == NavigationAction::End {
                len - 1
            } else {
                0
            };
            self.selected_offset = Some(start);
            self.hovered_offset = Some(start);
            self.scroll_to_offset = Some(start);
            cx.notify();
            return;
        };
        let next = Self::nav_next(action, cur, bpr, page_bytes, len);
        self.selected_offset = Some(next);
        self.hovered_offset = Some(next);
        self.scroll_to_offset = Some(next);
        cx.notify();
    }

    /// Pure navigation math: compute the offset reached by `action` from
    /// `cur`, clamped to `[0, len)`.
    fn nav_next(
        action: NavigationAction,
        cur: usize,
        bpr: usize,
        page_bytes: usize,
        len: usize,
    ) -> usize {
        match action {
            NavigationAction::Left => cur.saturating_sub(1),
            NavigationAction::Right => (cur + 1).min(len - 1),
            NavigationAction::Up => cur.saturating_sub(bpr),
            NavigationAction::Down => (cur + bpr).min(len - 1),
            NavigationAction::PageUp => cur.saturating_sub(page_bytes),
            NavigationAction::PageDown => (cur + page_bytes).min(len - 1),
            NavigationAction::Home => 0,
            NavigationAction::End => len - 1,
        }
    }

    fn on_copy_hex(&mut self, _: &CopySelectionHex, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_selection("hex", cx);
    }

    fn on_copy_ascii(&mut self, _: &CopySelectionAscii, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_selection("ascii", cx);
    }

    fn on_clear_selection(&mut self, _: &ClearSelection, _: &mut Window, cx: &mut Context<Self>) {
        self.selection_range = None;
        cx.notify();
    }

    fn copy_selection(&mut self, kind: &str, cx: &mut Context<Self>) {
        if let Some(d) = self.data() {
            let range = if let Some(r) = self.selection_range.clone() {
                r
            } else {
                let Some(o) = self.hovered_offset.or(self.selected_offset) else {
                    return;
                };
                o..(o + 1)
            };
            let start = range.start;
            let end = range.end.min(d.len());
            if start < end {
                let s = if kind == "hex" {
                    d[start..end]
                        .iter()
                        .map(|b| format!("{b:02X}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    d[start..end].iter().map(|&b| color::printable(b)).collect()
                };
                cx.write_to_clipboard(ClipboardItem::new_string(s));
            }
        }
    }

    fn on_jump_submit(&mut self, _: &JumpSubmit, _: &mut Window, cx: &mut Context<Self>) {
        if !self.show_jump_dialog {
            return;
        }
        let text = self.jump_field.read(cx).content.to_string();
        self.jump_submit(&text, cx);
    }

    fn on_jump_cancel(&mut self, _: &JumpCancel, _: &mut Window, cx: &mut Context<Self>) {
        if !self.show_jump_dialog {
            return;
        }
        self.show_jump_dialog = false;
        self.jump_error = None;
        cx.notify();
    }

    /// Apply a submitted jump offset (shared by the Enter action and the
    /// dialog's Jump button).
    fn jump_submit(&mut self, text: &str, cx: &mut Context<Self>) {
        match Self::parse_offset(text) {
            Some(o) if o < self.file_size => {
                self.scroll_to_offset = Some(o);
                self.selected_offset = Some(o);
                self.hovered_offset = Some(o);
                self.show_jump_dialog = false;
                self.jump_error = None;
            }
            Some(o) => {
                self.jump_error = Some(format!(
                    "Offset 0x{o:X} is out of range (file is 0x{:X} bytes).",
                    self.file_size
                ));
            }
            None => {
                self.jump_error = Some("Invalid offset.".to_owned());
            }
        }
        cx.notify();
    }

    /// Parse a user-supplied offset as hex: `0x` prefix optional, underscores
    /// and whitespace allowed (e.g. `"0x1_000"`, `"1F"`).
    fn parse_offset(input: &str) -> Option<usize> {
        let s = input.trim().replace('_', "");
        if s.is_empty() {
            return None;
        }
        let hex = s
            .strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .unwrap_or(&s);
        usize::from_str_radix(hex, 16).ok()
    }

    fn open_jump_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cur = self.selected_offset.unwrap_or(0);
        self.jump_field.update(cx, |field, cx| {
            field.set_content(&format!("0x{cur:X}"));
            cx.notify();
        });
        self.show_jump_dialog = true;
        self.jump_error = None;
        self.colormap_menu_open = false;
        window.focus(&self.jump_field.read(cx).focus_handle(cx));
        cx.notify();
    }

    // ----- mouse handlers (hex column) -----

    fn hex_offset_at_pos(&self, window: &mut Window, pos: Point<Pixels>) -> Option<usize> {
        let local = self.hex_bounds.localize(&pos)?;
        let bpr = self.bytes_per_row.max(1);
        let len = self.file_size;
        let total_rows = len.div_ceil(bpr);
        if total_rows == 0 {
            return None;
        }
        let font_size = px(panes::HEX_FONT_SIZE * self.hex_zoom);
        let font = font(self.mono_family.clone());
        let char_w = panes::hex_char_width(window, &font, font_size);
        let geo = panes::RowGeo::new(char_w, bpr);
        panes::hex_offset_at(
            local,
            &geo,
            self.scroll_rows,
            panes::hex_block_h(self.hex_zoom),
            total_rows,
            len,
        )
    }

    fn on_hex_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window) {
        if event.button == MouseButton::Left {
            self.hex_mouse_down = true;
        }
        if event.button == MouseButton::Left
            && (event.modifiers.control || event.modifiers.platform || event.modifiers.alt)
        {
            // Ctrl/Alt + primary drag pans (same gesture as the pixels col).
            self.pan_active = true;
            return;
        }
        if event.button == MouseButton::Middle {
            self.pan_active = true;
            return;
        }
        if event.button == MouseButton::Right {
            // Copy the selection (or hovered byte) as hex; Alt+right-click
            // clears the selection.
            if event.modifiers.alt {
                self.selection_range = None;
            } else {
                let off = self
                    .hex_offset_at_pos(window, event.position)
                    .or(self.hovered_offset)
                    .or(self.selected_offset);
                if let Some(off) = off {
                    if self.selection_range.is_none() {
                        self.selection_range = Some(off..off + 1);
                        self.selected_offset = Some(off);
                    }
                    let s = self.data().map(|d| {
                        let r = self.selection_range.clone().unwrap_or(off..off + 1);
                        let start = r.start.min(d.len());
                        let end = r.end.min(d.len());
                        if start < end {
                            d[start..end]
                                .iter()
                                .map(|b| format!("{b:02X}"))
                                .collect::<Vec<_>>()
                                .join(" ")
                        } else {
                            String::new()
                        }
                    });
                    if let Some(s) = s {
                        // Deferred copy needs cx; stored here via message.
                        self.pending_copy = Some(s);
                    }
                }
            }
            return;
        }
        if let Some(off) = self.hex_offset_at_pos(window, event.position) {
            self.drag_start = Some(off);
            self.selection_range = None;
            self.selected_offset = Some(off);
        }
    }

    fn on_hex_mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window) {
        let off = self.hex_offset_at_pos(window, event.position);
        match off {
            Some(off) => {
                self.hovered_offset = Some(off);
                if self.hex_mouse_down
                    && event.dragging()
                    && !self.pan_active
                    && let Some(start) = self.drag_start
                {
                    let (a, b) = (start.min(off), start.max(off) + 1);
                    self.selection_range = Some(a..b.min(self.file_size));
                    self.selected_offset = Some(off);
                }
            }
            None => self.hovered_offset = None,
        }
    }

    fn on_hex_mouse_up(&mut self, event: &MouseUpEvent) {
        if event.button == MouseButton::Left {
            self.hex_mouse_down = false;
            self.drag_start = None;
            self.pan_active = false;
        }
        if event.button == MouseButton::Middle {
            self.pan_active = false;
        }
    }

    fn on_hex_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        if event.modifiers.control || event.modifiers.platform {
            // Ctrl+wheel zooms the hex cells.
            let factor = wheel_zoom_factor(&event.delta);
            if factor != 1.0 {
                self.hex_zoom = panes::zoom_step(
                    self.hex_zoom,
                    factor,
                    panes::HEX_ZOOM_MIN,
                    panes::HEX_ZOOM_MAX,
                );
                cx.notify();
            }
        } else {
            self.scroll_by_wheel(&event.delta);
        }
    }

    // ----- mouse handlers (pixels column) -----

    fn pixels_row_h(&self) -> f32 {
        2.0 * self
            .pixel_zoom
            .clamp(panes::PIXEL_ZOOM_MIN, panes::PIXEL_ZOOM_MAX)
            + 1.0
    }

    fn pixels_offset_at(&self, _window: &Window, pos: Point<Pixels>) -> Option<usize> {
        let local = self.pixels_bounds.localize(&pos)?;
        let bpr = self.bytes_per_row.max(1);
        let len = self.file_size;
        if len == 0 {
            return None;
        }
        let px_size = self
            .pixel_zoom
            .clamp(panes::PIXEL_ZOOM_MIN, panes::PIXEL_ZOOM_MAX);
        let row_h = self.pixels_row_h();
        let row = self.scroll_rows + (local.y.to_f64() as f32) / row_h;
        let col = ((local.x.to_f64() as f32) / px_size).floor();
        if row < 0.0 || col < 0.0 {
            return None;
        }
        let off = (row as usize * bpr + col as usize).min(len.saturating_sub(1));
        Some(off)
    }

    fn on_pixels_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window) {
        self.pixels_dragging = true;
        self.last_pixels_y = Some(event.position.y.to_f64() as f32);
        if event.button == MouseButton::Left
            && let Some(off) = self.pixels_offset_at(window, event.position)
        {
            self.selected_offset = Some(off);
            self.hovered_offset = Some(off);
        }
    }

    fn on_pixels_mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window) {
        let y = event.position.y.to_f64() as f32;
        if self.pixels_dragging
            && event.dragging()
            && let Some(last) = self.last_pixels_y
        {
            // Content follows the cursor: dragging down (dy > 0) shows
            // earlier rows, so the scroll offset decreases.
            self.scroll_rows -= (y - last) / self.pixels_row_h();
            self.clamp_scroll();
        }
        self.last_pixels_y = Some(y);
        if let Some(off) = self.pixels_offset_at(window, event.position) {
            self.hovered_offset = Some(off);
        } else {
            self.hovered_offset = None;
        }
    }

    fn on_pixels_mouse_up(&mut self) {
        self.pixels_dragging = false;
        self.last_pixels_y = None;
    }

    fn on_pixels_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        if event.modifiers.control || event.modifiers.platform {
            let factor = wheel_zoom_factor(&event.delta);
            if factor != 1.0 {
                self.pixel_zoom = panes::zoom_step(
                    self.pixel_zoom,
                    factor,
                    panes::PIXEL_ZOOM_MIN,
                    panes::PIXEL_ZOOM_MAX,
                );
                cx.notify();
            }
        } else {
            self.scroll_by_wheel(&event.delta);
        }
    }

    // ----- scroll helpers -----

    fn scroll_by_wheel(&mut self, delta: &ScrollDelta) {
        let block_h = panes::hex_block_h(self.hex_zoom);
        let pixels = delta.pixel_delta(px(16.0));
        // Positive wheel-up delta decreases the scroll offset (toward the
        // top of the file).
        self.scroll_rows -= pixels.y.to_f64() as f32 / block_h;
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        let bpr = self.bytes_per_row.max(1);
        let total_rows = self.file_size.div_ceil(bpr);
        let block_h = panes::hex_block_h(self.hex_zoom);
        let content_h = total_rows as f32 * block_h;
        let view_h = self.view_height;
        let max_scroll = if content_h > view_h {
            (content_h - view_h) / block_h
        } else {
            0.0
        };
        self.scroll_rows = self.scroll_rows.clamp(0.0, max_scroll);
    }

    // ----- overview / strip -----

    fn overview_offset_at(&self, pos: Point<Pixels>) -> Option<usize> {
        let local = self.overview_bounds.localize(&pos)?;
        let len = self.file_size;
        let (cells_w, cells_h) = self.overview_cells?;
        if len == 0 || cells_w == 0 || cells_h == 0 {
            return None;
        }
        let bounds_w = self.overview_bounds.size.width.to_f64() as f32;
        let bounds_h = self.overview_bounds.size.height.to_f64() as f32;
        let x = local.x.to_f64() as f32;
        let y = local.y.to_f64() as f32;
        // The overview image packs two sub-cells per byte-row in y, so a
        // half-cell precision hit-test maps back to the enclosing row.
        let col =
            ((x / bounds_w.max(1.0)) * cells_w as f32).clamp(0.0, cells_w as f32 - 1.0) as usize;
        let sub_row = ((y / bounds_h.max(1.0)) * (2 * cells_h) as f32)
            .clamp(0.0, (2 * cells_h) as f32 - 1.0) as usize;
        let row = sub_row / 2;
        let idx = (row * cells_w + col).min(cells_w * cells_h - 1);
        Some((idx * len / (cells_w * cells_h)).min(len - 1))
    }

    fn strip_offset_at(&self, pos: Point<Pixels>) -> Option<usize> {
        if self.file_size == 0 {
            return None;
        }
        let t = ((pos.x - self.strip_bounds.left()).to_f64() as f32
            / self.strip_bounds.size.width.to_f64() as f32)
            .clamp(0.0, 1.0);
        Some(((t * self.file_size as f32) as usize).min(self.file_size.saturating_sub(1)))
    }

    fn on_overview_move(&mut self, pos: Point<Pixels>, dragging: bool) {
        if self.overview_dragging && dragging {
            if let Some(off) = self.overview_offset_at(pos) {
                self.jump_to(off);
            }
        } else if let Some(off) = self.overview_offset_at(pos) {
            self.overview_hover_offset = Some(off);
        }
    }

    fn on_overview_mouse_down(&mut self, pos: Point<Pixels>) {
        self.overview_dragging = true;
        if let Some(off) = self.overview_offset_at(pos) {
            self.jump_to(off);
        }
    }

    fn on_overview_mouse_up(&mut self) {
        self.overview_dragging = false;
    }

    fn on_strip_move(&mut self, pos: Point<Pixels>, dragging: bool) {
        if self.overview_dragging && dragging {
            if let Some(off) = self.strip_offset_at(pos) {
                self.jump_to(off);
            }
        } else if let Some(off) = self.strip_offset_at(pos) {
            self.overview_hover_offset = Some(off);
        }
    }

    fn on_strip_mouse_down(&mut self, pos: Point<Pixels>) {
        self.overview_dragging = true;
        if let Some(off) = self.strip_offset_at(pos) {
            self.jump_to(off);
        }
    }

    fn on_strip_mouse_up(&mut self) {
        self.overview_dragging = false;
    }

    fn jump_to(&mut self, off: usize) {
        self.scroll_to_offset = Some(off);
        self.selected_offset = Some(off);
        self.hovered_offset = Some(off);
    }

    // ----- column divider drag -----

    /// Begin dragging a column divider: remember the pointer x and the width
    /// being changed so the drag delta can be applied exactly.
    fn on_divider_mouse_down(&mut self, kind: DividerKind, pos: Point<Pixels>) {
        self.resizing_divider = Some(kind);
        self.divider_start_x = pos.x.to_f64() as f32;
        self.divider_start_w = match kind {
            DividerKind::OverviewPixels => self.overview_width,
            DividerKind::PixelsHex => self.pixels_width,
        };
    }

    /// Continue a divider drag from the pointer position. Returns true when a
    /// width actually changed. Also invoked from the root while a resize is
    /// in flight, so the drag keeps working once the pointer leaves the thin
    /// divider strip.
    fn on_divider_mouse_move(&mut self, pos: Point<Pixels>) -> bool {
        let Some(kind) = self.resizing_divider else {
            return false;
        };
        let dx = pos.x.to_f64() as f32 - self.divider_start_x;
        let (min, max) = match kind {
            DividerKind::OverviewPixels => (OVERVIEW_W_MIN, OVERVIEW_W_MAX),
            DividerKind::PixelsHex => (PIXELS_W_MIN, PIXELS_W_MAX),
        };
        let w = divider_width(self.divider_start_w, dx, min, max);
        match kind {
            DividerKind::OverviewPixels => {
                let changed = (w - self.overview_width).abs() > 0.5;
                self.overview_width = w;
                changed
            }
            DividerKind::PixelsHex => {
                let changed = (w - self.pixels_width).abs() > 0.5;
                self.pixels_width = w;
                changed
            }
        }
    }

    fn on_divider_mouse_up(&mut self) {
        self.resizing_divider = None;
    }

    // ----- bottom status bar -----

    /// Bottom status bar: the live offset/byte/entropy readout, selection
    /// and scroll summaries, the zoom state, the jump preview while typing
    /// in the jump dialog, and transient messages.
    fn status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let readout = self.byte_readout();
        let jump_preview = self.jump_preview(cx);
        let selection = self.selection_summary();
        let scroll = self.scroll_summary();

        div()
            .w_full()
            .flex()
            .items_center()
            .gap_4()
            .px_3()
            .py_1()
            .bg(rgb(0x1a1b26))
            .border_t_1()
            .border_color(rgb(0x2a2f45))
            .text_size(px(11.))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .text_color(rgb(0x9ece6a))
                            .child(readout.unwrap_or_else(|| "no file loaded".to_owned())),
                    )
                    .when(selection.is_some(), |d| {
                        d.child(div().child(selection.clone().unwrap()))
                    })
                    .when(jump_preview.is_some(), |d| {
                        let (text, is_err) = jump_preview.clone().unwrap();
                        d.child(
                            div()
                                .text_color(if is_err { rgb(0xe0af68) } else { rgb(0x9ece6a) })
                                .child(text),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(div().text_color(rgb(0x565f89)).child(format!(
                        "hex ×{:.2} · px {}",
                        self.hex_zoom,
                        self.pixel_zoom.round() as u32
                    )))
                    .when(scroll.is_some(), |d| {
                        d.child(
                            div()
                                .text_color(rgb(0x565f89))
                                .child(scroll.clone().unwrap()),
                        )
                    })
                    .when(self.message.is_some(), |d| {
                        d.child(
                            div()
                                .text_color(rgb(0xe0af68))
                                .child(self.message.clone().unwrap()),
                        )
                    }),
            )
    }

    /// Selection range summary for the status bar.
    fn selection_summary(&self) -> Option<String> {
        let range = self.selection_range.as_ref()?;
        let len = self.file_size;
        let start = range.start.min(len);
        let end = range.end.min(len);
        (start < end).then(|| format!("sel 0x{start:X}–0x{end:X} ({} B)", end - start))
    }

    /// Visible row range + file percentage for the status bar.
    fn scroll_summary(&self) -> Option<String> {
        if self.file_size == 0 {
            return None;
        }
        let bpr = self.bytes_per_row.max(1);
        let total_rows = self.file_size.div_ceil(bpr);
        let block_h = panes::hex_block_h(self.hex_zoom);
        let first = self.scroll_rows.floor().max(0.0) as usize;
        let vis = ((self.view_height / block_h).ceil() as usize).max(1);
        let last = (first + vis).min(total_rows);
        let pct = (self.view_frac * 100.0).round() as u32;
        Some(format!("rows {first}–{last} / {total_rows} · {pct}%"))
    }

    // ----- top info bar -----

    /// The top info bar: app title, file name/size and the action controls
    /// (open, bytes-per-row, entropy window, reset/jump) plus the horizontal
    /// whole-file preview strip. Live readouts live in the bottom status bar.
    #[allow(clippy::too_many_lines)] // single-purpose element builder
    fn top_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let file_size = self.file_size;
        let file_name = self.file_name_str();
        let has_file = self.mmap.is_some();

        let row2 = div()
            .flex()
            .items_center()
            .gap_2()
            .child(button(cx, "Open File…", |this, window, cx| {
                this.on_open_file(&OpenFile, window, cx);
            }))
            .child(div().child("Bytes/Row:"))
            .children([16usize, 32, 64].into_iter().map(|bpr| {
                let sel = self.bytes_per_row == bpr;
                let mut b = div()
                    .id(("bpr", bpr))
                    .px_1()
                    .rounded_md()
                    .child(format!("{bpr}"))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.bytes_per_row != bpr {
                            this.bytes_per_row = bpr;
                            this.scroll_reset = true;
                            this.clamp_scroll();
                            cx.notify();
                        }
                    }));
                b = if sel {
                    b.bg(rgb(0x3b4261)).text_color(rgb(0xffffff))
                } else {
                    b.hover(|s| s.bg(rgb(0x232738)))
                };
                b
            }))
            .child(self.slider(cx, SliderKind::EntropyWindow))
            .child(div().child("Entropy win"))
            .child(button(cx, "Reset view", |this, window, cx| {
                this.on_reset_view(&ResetView, window, cx);
            }))
            .child(button(
                cx,
                "Jump to offset… (Ctrl+G)",
                |this, window, cx| {
                    this.on_jump_to_offset(&JumpToOffset, window, cx);
                },
            ))
            .child(button(cx, "Reset all settings", |this, window, cx| {
                this.on_reset_settings(&ResetSettings, window, cx);
            }));

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .py_2()
            .bg(rgb(0x1a1b26))
            .border_b_1()
            .border_color(rgb(0x2a2f45))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().text_xl().text_color(rgb(0x7aa2f7)).child("ParallHex"))
                    .child(div().child(format!(
                        "{file_name} · {file_size} bytes ({})",
                        color::human_size(file_size)
                    )))
                    .when(has_file, |d| {
                        d.flex_1()
                            .child(div().flex().justify_end().child(self.strip(cx)))
                    }),
            )
            .child(row2)
    }

    /// Hovered / selected byte readout shown in the bottom status bar.
    fn byte_readout(&self) -> Option<String> {
        let off = self
            .overview_hover_offset
            .or(self.hovered_offset)
            .or(self.selected_offset)?;
        let d = self.data()?;
        if off >= d.len() {
            return None;
        }
        let b = d[off];
        let h = panes::entropy_at(&self.entropies, self.entropy_window, off);
        Some(format!(
            "0x{off:08X} · 0x{b:02X} '{}' · H={h:.3}",
            color::printable(b)
        ))
    }

    /// Live jump-dialog preview while typing.
    fn jump_preview(&self, cx: &mut Context<Self>) -> Option<(String, bool)> {
        if !self.show_jump_dialog {
            return None;
        }
        let content = self.jump_field.read(cx).content.to_string();
        match Self::parse_offset(&content) {
            Some(o) if o < self.file_size => {
                let d = self.data();
                let b = d.map_or(0, |d| d[o]);
                let h = panes::entropy_at(&self.entropies, self.entropy_window, o);
                Some((
                    format!(
                        "Jump: 0x{o:08X}  Byte: 0x{b:02X} '{}'  H={h:.3}",
                        color::printable(b)
                    ),
                    false,
                ))
            }
            Some(o) => Some((
                format!(
                    "Out of range: 0x{o:X} (file is 0x{:X} bytes).",
                    self.file_size
                ),
                true,
            )),
            None => Some(("Jump: invalid offset".to_owned(), true)),
        }
    }

    /// The horizontal whole-file preview strip (greyscale / entropy).
    fn strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let strip_image = self.strip_image.clone();
        let file_size = self.file_size;
        let view_frac = self.view_frac;
        let view_frac_h = self.view_frac_h;
        div()
            .w(px(STRIP_W))
            .h(px(STRIP_H))
            .rounded_md()
            .overflow_hidden()
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                this.on_strip_move(ev.position, ev.dragging());
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    this.on_strip_mouse_down(ev.position);
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseUpEvent, _: &mut Window, cx| {
                    this.on_strip_mouse_up();
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseUpEvent, _: &mut Window, cx| {
                    this.on_strip_mouse_up();
                    cx.notify();
                }),
            )
            .child(canvas(
                move |bounds, _window, cx| {
                    entity.update(cx, |this, _| {
                        this.strip_bounds = bounds;
                    });
                },
                move |bounds, (), window, cx| {
                    if let Some(img) = &strip_image {
                        panes::paint_strip(
                            window,
                            cx,
                            bounds,
                            img,
                            file_size,
                            view_frac,
                            view_frac_h,
                        );
                    } else {
                        window.paint_quad(quad_dark(bounds));
                    }
                },
            ))
    }

    // ----- column builders -----

    /// A draggable 6px divider between two columns. The pointer-down starts
    /// the resize; the root's mouse-move handler continues it while the
    /// pointer is anywhere in the window; pointer-up (on or off the strip)
    /// ends it.
    fn column_divider(cx: &mut Context<Self>, kind: DividerKind) -> impl IntoElement {
        div()
            .id(("divider", kind as usize))
            .w(px(6.))
            .h_full()
            .flex_shrink_0()
            .cursor(CursorStyle::ResizeLeftRight)
            .bg(rgb(0x1a1b26))
            .hover(|s| s.bg(rgb(0x3b4261)))
            .active(|s| s.bg(rgb(0x3b4261)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    this.on_divider_mouse_down(kind, ev.position);
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                if this.on_divider_mouse_move(ev.position) {
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                    this.on_divider_mouse_up();
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                    this.on_divider_mouse_up();
                    cx.notify();
                }),
            )
    }

    /// Left column: a vertical whole-file overview.
    #[allow(clippy::too_many_lines)] // single-purpose element builder
    fn overview_column(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let data = self.mmap.clone();
        let entropies = self.entropies.clone();
        let entropy_window = self.entropy_window;
        let file_size = self.file_size;
        let view_frac = self.view_frac;
        let view_frac_h = self.view_frac_h;

        let header = column_header(
            "Overview",
            (file_size > 0).then(|| panes::range_label(0, file_size)),
            div().child(
                div()
                    .text_color(rgb(0x565f89))
                    .child("whole file · grey/entropy"),
            ),
        );

        div()
            .w(px(self.overview_width))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(0x12121c))
            .border_r_1()
            .border_color(rgb(0x232740))
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                        this.on_overview_move(ev.position, ev.dragging());
                        cx.notify();
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                            this.on_overview_mouse_down(ev.position);
                            cx.notify();
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseUpEvent, _: &mut Window, cx| {
                            this.on_overview_mouse_up();
                            cx.notify();
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseUpEvent, _: &mut Window, cx| {
                            this.on_overview_mouse_up();
                            cx.notify();
                        }),
                    )
                    .child(canvas(
                        {
                            let entity = entity.clone();
                            move |bounds, _window, cx| {
                                // Regenerate the thumbnail when the panel resizes.
                                let w = (bounds.size.width.to_f64() as usize).clamp(64, 512);
                                let h = (bounds.size.height.to_f64() as usize).clamp(32, 1024);
                                let dirty = entity.read(cx).overview_gen_size != Some((w, h));
                                if dirty {
                                    let img = data.as_deref().map(|d| {
                                        panes::build_overview_image(
                                            d,
                                            &entropies,
                                            entropy_window,
                                            w,
                                            h,
                                        )
                                    });
                                    entity.update(cx, |this, _| {
                                        if let Some((image, cells)) = img {
                                            this.overview_image = Some(image);
                                            this.overview_cells = Some(cells);
                                        } else {
                                            this.overview_image = None;
                                            this.overview_cells = None;
                                        }
                                    });
                                    entity.update(cx, |this, _| {
                                        this.overview_gen_size = Some((w, h));
                                    });
                                }
                                entity.update(cx, |this, _| {
                                    this.overview_bounds = bounds;
                                });
                            }
                        },
                        {
                            let entity = entity.clone();
                            move |bounds, (), window, cx| {
                                let image = entity.read(cx).overview_image.clone();
                                match image {
                                    Some(img) => panes::paint_overview(
                                        window,
                                        cx,
                                        bounds,
                                        &img,
                                        file_size,
                                        view_frac,
                                        view_frac_h,
                                    ),
                                    None => {
                                        window.paint_quad(quad_dark(bounds));
                                    }
                                }
                            }
                        },
                    ))
                    .size_full(),
            )
    }

    /// Middle column: per-byte colormap + entropy bands.
    fn pixels_column(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let data = self.mmap.clone();
        let entropies = self.entropies.clone();
        let entropy_window = self.entropy_window;
        let bpr = self.bytes_per_row.max(1);
        let len = self.file_size;
        let scroll_rows = self.scroll_rows;
        let pixel_zoom = self.pixel_zoom;
        let hovered = self.hovered_offset;
        let sel = self.selection_range.clone();
        let colormap = self.pixel_colormap;

        let range = (len > 0).then(|| {
            let row_h = self.pixels_row_h();
            let first = self.scroll_rows.floor().max(0.0) as usize;
            let vis_rows = ((self.view_height / row_h).ceil() as usize + 1)
                .min(len.div_ceil(bpr).saturating_sub(first));
            panes::range_label(first * bpr, ((first + vis_rows) * bpr).min(len))
        });

        div()
            .w(px(self.pixels_width))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(0x10101a))
            .border_r_1()
            .border_color(rgb(0x232740))
            .child(self.pixels_header(cx, range))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                            this.on_pixels_mouse_down(ev, window);
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, window, cx| {
                        this.on_pixels_mouse_move(ev, window);
                        cx.notify();
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseUpEvent, _: &mut Window, cx| {
                            this.on_pixels_mouse_up();
                            cx.notify();
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseUpEvent, _: &mut Window, cx| {
                            this.on_pixels_mouse_up();
                            cx.notify();
                        }),
                    )
                    .on_scroll_wheel(cx.listener(move |this, ev: &ScrollWheelEvent, _, cx| {
                        this.on_pixels_scroll(ev, cx);
                        cx.notify();
                    }))
                    .child(canvas(
                        move |bounds, _window, cx| {
                            entity.update(cx, |this, _| {
                                this.pixels_bounds = bounds;
                            });
                        },
                        move |bounds, (), window, cx| {
                            if let Some(d) = &data {
                                panes::paint_pixels(
                                    window,
                                    cx,
                                    bounds,
                                    d,
                                    bpr,
                                    scroll_rows,
                                    pixel_zoom,
                                    hovered,
                                    sel.as_ref(),
                                    &entropies,
                                    entropy_window,
                                    colormap,
                                );
                            }
                        },
                    ))
                    .size_full(),
            )
    }

    /// The pixels-column header: title + zoom controls, a range row that
    /// hosts the colormap dropdown toggle, and (when open) the expanded
    /// colormap options row.
    fn pixels_header(&mut self, cx: &mut Context<Self>, range: Option<String>) -> impl IntoElement {
        let open = self.colormap_menu_open;
        let current_label = self.pixel_colormap.label();
        // Consume the range eagerly: the builder closures below can only
        // borrow it, and it must outlive the returned element.
        let range = range.unwrap_or_default();

        let toggle = div()
            .id("colormap-toggle")
            .px_2()
            .py_1()
            .rounded_md()
            .flex()
            .items_center()
            .gap_1()
            .bg(if open { rgb(0x3b4261) } else { rgb(0x24283b) })
            .text_color(rgb(0xc0caf5))
            .cursor_pointer()
            .active(|s| s.opacity(0.7))
            .hover(|s| s.bg(rgb(0x3b4261)))
            .on_any_mouse_down(
                cx.listener(move |this, _: &MouseDownEvent, _: &mut Window, _cx| {
                    this.colormap_toggle_down = true;
                }),
            )
            .on_click(
                cx.listener(move |this, _: &ClickEvent, _: &mut Window, cx| {
                    this.colormap_menu_open = !this.colormap_menu_open;
                    cx.notify();
                }),
            )
            .child(swatch(self.pixel_colormap))
            .child(div().child(format!("Map: {current_label}")))
            .child(div().child("▾"));

        div()
            .w_full()
            .px_2()
            .py_1()
            .flex()
            .flex_col()
            .gap_1()
            .border_b_1()
            .border_color(rgb(0x232740))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_color(rgb(0x9d7cd8)).child("Pixels"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .text_color(rgb(0x565f89))
                                    .child(format!("{} px", self.pixel_zoom.round() as u32)),
                            )
                            .child(self.slider(cx, SliderKind::PixelZoom))
                            .child(button(cx, "Reset", move |this, _window, cx| {
                                this.pixel_zoom = panes::PIXEL_ZOOM_DEFAULT;
                                cx.notify();
                            })),
                    ),
            )
            .when(!range.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_color(rgb(0x565f89))
                                .text_size(px(11.))
                                .child(range.clone()),
                        )
                        .child(toggle),
                )
            })
            .when(open, |d| d.child(self.colormap_menu(cx)))
    }

    /// The expanded colormap options row (Greyscale / Entropy / Byte class).
    /// Clicking an option applies it to the pixels column and collapses the
    /// row back to the toggle.
    fn colormap_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.pixel_colormap;
        div()
            .flex()
            .items_center()
            .gap_1()
            .children(Colormap::ALL.into_iter().enumerate().map(|(idx, cm)| {
                let selected = cm == current;
                let mut pill = div()
                    .id(("colormap", idx))
                    .px_1()
                    .py_1()
                    .rounded_md()
                    .text_size(px(11.))
                    .cursor_pointer()
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _: &mut Window, cx| {
                            this.pixel_colormap = cm;
                            this.colormap_menu_open = false;
                            cx.notify();
                        }),
                    );
                pill = if selected {
                    pill.bg(rgb(0x7aa2f7)).text_color(rgb(0x0f1017))
                } else {
                    pill.bg(rgb(0x24283b))
                        .text_color(rgb(0xc0caf5))
                        .hover(|s| s.bg(rgb(0x3b4261)))
                };
                pill.child(cm.label())
            }))
    }

    /// Right column: class-colored hex + ASCII cells (master scroll).
    #[allow(clippy::too_many_lines)] // single-purpose element builder
    fn hex_column(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let data = self.mmap.clone();
        let bpr = self.bytes_per_row.max(1);
        let len = self.file_size;
        let scroll_rows = self.scroll_rows;
        let hex_zoom = self.hex_zoom;
        let hovered = self.hovered_offset;
        let sel = self.selection_range.clone();
        let font = panes::mono_font(&self.mono_family);

        let range = (len > 0).then(|| {
            let block_h = panes::hex_block_h(self.hex_zoom);
            let first = self.scroll_rows.floor().max(0.0) as usize;
            let vis_rows = ((self.view_height / block_h).ceil() as usize + 1)
                .min(len.div_ceil(bpr).saturating_sub(first));
            panes::range_label(first * bpr, ((first + vis_rows) * bpr).min(len))
        });

        let header_extra = div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .text_color(rgb(0x565f89))
                    .child(format!("×{hex_zoom:.2}")),
            )
            .child(self.slider(cx, SliderKind::HexZoom))
            .child(button(cx, "Reset zoom", move |this, _window, cx| {
                this.hex_zoom = panes::HEX_ZOOM_DEFAULT;
                cx.notify();
            }));

        div()
            .flex_1()
            .min_w(px(200.))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(0x0c0d14))
            .child(column_header("Hex", range, header_extra))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                            this.on_hex_mouse_down(ev, window);
                            if let Some(copy) = this.pending_copy.take() {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy));
                            }
                            cx.notify();
                        }),
                    )
                    .on_any_mouse_down(cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                        this.on_hex_mouse_down(ev, window);
                        if let Some(copy) = this.pending_copy.take() {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy));
                        }
                        cx.notify();
                    }))
                    .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, window, cx| {
                        this.on_hex_mouse_move(ev, window);
                        cx.notify();
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseUpEvent, _, cx| {
                            this.on_hex_mouse_up(ev);
                            cx.notify();
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseUpEvent, _, cx| {
                            this.on_hex_mouse_up(ev);
                            cx.notify();
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Middle,
                        cx.listener(move |this, ev: &MouseUpEvent, _, cx| {
                            this.on_hex_mouse_up(ev);
                            cx.notify();
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Middle,
                        cx.listener(move |this, ev: &MouseUpEvent, _, cx| {
                            this.on_hex_mouse_up(ev);
                            cx.notify();
                        }),
                    )
                    .on_scroll_wheel(cx.listener(move |this, ev: &ScrollWheelEvent, _, cx| {
                        this.on_hex_scroll(ev, cx);
                        cx.notify();
                    }))
                    .child(canvas(
                        move |bounds, _window, cx| {
                            entity.update(cx, |this, cx| {
                                this.hex_bounds = bounds;
                                this.view_height = bounds.size.height.to_f64() as f32;
                                // Resolve one-shot scroll requests, then clamp.
                                let block_h = panes::hex_block_h(this.hex_zoom);
                                let total_rows = this.file_size.div_ceil(this.bytes_per_row.max(1));
                                let content_h = total_rows as f32 * block_h;
                                let view_h = this.view_height;
                                let mut changed = false;
                                if this.scroll_reset {
                                    this.scroll_rows = 0.0;
                                    this.scroll_reset = false;
                                    changed = true;
                                }
                                if let Some(off) = this.scroll_to_offset.take() {
                                    let row = (off / this.bytes_per_row.max(1)) as f32;
                                    let target = (row * block_h - view_h * 0.5).max(0.0);
                                    this.scroll_rows = target / block_h.max(1.0);
                                    changed = true;
                                }
                                let max_scroll = if content_h > view_h {
                                    (content_h - view_h) / block_h
                                } else {
                                    0.0
                                };
                                let clamped = this.scroll_rows.clamp(0.0, max_scroll);
                                if (clamped - this.scroll_rows).abs() > 0.0001 {
                                    this.scroll_rows = clamped;
                                    changed = true;
                                }
                                if content_h > 0.0 {
                                    this.view_frac =
                                        ((this.scroll_rows * block_h) / content_h).clamp(0.0, 1.0);
                                    this.view_frac_h = (view_h / content_h).clamp(0.0, 1.0);
                                }
                                if changed {
                                    cx.notify();
                                }
                            });
                        },
                        move |bounds, (), window, cx| {
                            if let Some(d) = &data {
                                panes::paint_hex(
                                    window,
                                    cx,
                                    bounds,
                                    d,
                                    &font,
                                    hex_zoom,
                                    bpr,
                                    scroll_rows,
                                    hovered,
                                    sel.as_ref(),
                                );
                            } else {
                                window.paint_quad(quad_dark(bounds));
                            }
                        },
                    ))
                    .size_full(),
            )
    }

    /// The jump dialog as an overlay covering the window.
    fn jump_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let jump_field = self.jump_field.clone();
        let file_size = self.file_size;
        let error = self.jump_error.clone();
        div()
            .absolute()
            .inset_0()
            .bg(rgba(0x000000a0))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _: &mut Window, cx| {
                    // Clicking the backdrop dismisses the dialog.
                    this.show_jump_dialog = false;
                    this.jump_error = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .w(px(380.))
                    .bg(rgb(0x1f2335))
                    .border_1()
                    .border_color(rgb(0x414868))
                    .rounded_lg()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(
                            |_: &mut ParallHexApp, _: &MouseDownEvent, _: &mut Window, cx| {
                                // Swallow clicks on the card so the backdrop stays.
                                cx.stop_propagation();
                            },
                        ),
                    )
                    .child(div().text_color(rgb(0x7aa2f7)).child("Jump to Offset"))
                    .child(div().child(format!(
                        "Offset (hex, 0x… up to 0x{:X}):",
                        file_size.saturating_sub(1)
                    )))
                    .child(jump_field.clone())
                    .when(error.is_some(), |d| {
                        d.child(
                            div()
                                .text_color(rgb(0xe0af68))
                                .child(error.clone().unwrap()),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(button(cx, "Cancel", move |this, _window, cx| {
                                this.show_jump_dialog = false;
                                this.jump_error = None;
                                cx.notify();
                            }))
                            .child(button(cx, "Jump", move |this, _window, cx| {
                                let text = this.jump_field.read(cx).content.to_string();
                                this.jump_submit(&text, cx);
                            })),
                    ),
            )
    }

    /// A compact slider used by the column headers and the entropy-window
    /// control. The track/thumb are painted on a canvas that records its own
    /// bounds (via `entity.update`) for the pointer handlers.
    fn slider(&mut self, cx: &mut Context<Self>, kind: SliderKind) -> impl IntoElement {
        let entity = cx.entity();
        let value = match kind {
            SliderKind::HexZoom => self.hex_zoom,
            SliderKind::PixelZoom => self.pixel_zoom,
            SliderKind::EntropyWindow => self.entropy_window as f32,
        };
        let (min, max) = match kind {
            SliderKind::HexZoom => (panes::HEX_ZOOM_MIN, panes::HEX_ZOOM_MAX),
            SliderKind::PixelZoom => (panes::PIXEL_ZOOM_MIN, panes::PIXEL_ZOOM_MAX),
            SliderKind::EntropyWindow => (16.0, 4096.0),
        };

        div()
            .w(px(90.))
            .h(px(16.))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    this.dragging_slider = Some(kind);
                    if let Some(v) = slider_value_at(kind, ev.position, this.slider_bounds(kind)) {
                        this.set_slider_value(kind, v, cx);
                    }
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                if this.dragging_slider == Some(kind)
                    && ev.dragging()
                    && let Some(v) = slider_value_at(kind, ev.position, this.slider_bounds(kind))
                {
                    this.set_slider_value(kind, v, cx);
                }
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseUpEvent, _: &mut Window, _| {
                    if this.dragging_slider == Some(kind) {
                        this.dragging_slider = None;
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseUpEvent, _: &mut Window, _| {
                    if this.dragging_slider == Some(kind) {
                        this.dragging_slider = None;
                    }
                }),
            )
            .child(canvas(
                move |bounds, _window, cx| {
                    entity.update(cx, |this, _| {
                        this.set_slider_bounds(kind, bounds);
                    });
                },
                move |bounds, (), window, _cx| {
                    let w = bounds.size.width.to_f64() as f32;
                    let h = bounds.size.height.to_f64() as f32;
                    let t = slider_t(value, min, max);
                    let track = Bounds::new(
                        point(bounds.left() + px(2.), bounds.top() + px(h * 0.5 - 2.)),
                        size(px(w - 4.), px(4.)),
                    );
                    window.paint_quad(quad(
                        track,
                        px(2.),
                        rgb(0x2a2f45),
                        px(0.),
                        transparent_black(),
                        BorderStyle::default(),
                    ));
                    let thumb_x = 2.0 + t * (w - 16.0);
                    let thumb = Bounds::new(
                        point(bounds.left() + px(thumb_x), bounds.top() + px(h * 0.5 - 6.)),
                        size(px(12.), px(12.)),
                    );
                    window.paint_quad(quad(
                        thumb,
                        px(6.),
                        rgb(0x7aa2f7),
                        px(0.),
                        transparent_black(),
                        BorderStyle::default(),
                    ));
                },
            ))
    }

    fn slider_bounds(&self, kind: SliderKind) -> Bounds<Pixels> {
        match kind {
            SliderKind::HexZoom => self.hex_slider_bounds,
            SliderKind::PixelZoom => self.pixels_slider_bounds,
            SliderKind::EntropyWindow => self.entropy_slider_bounds,
        }
    }

    fn set_slider_bounds(&mut self, kind: SliderKind, bounds: Bounds<Pixels>) {
        match kind {
            SliderKind::HexZoom => self.hex_slider_bounds = bounds,
            SliderKind::PixelZoom => self.pixels_slider_bounds = bounds,
            SliderKind::EntropyWindow => self.entropy_slider_bounds = bounds,
        }
    }

    fn set_slider_value(&mut self, kind: SliderKind, v: f32, cx: &mut Context<Self>) {
        match kind {
            SliderKind::HexZoom => {
                self.hex_zoom = v.clamp(panes::HEX_ZOOM_MIN, panes::HEX_ZOOM_MAX);
            }
            SliderKind::PixelZoom => {
                self.pixel_zoom = v.clamp(panes::PIXEL_ZOOM_MIN, panes::PIXEL_ZOOM_MAX);
            }
            SliderKind::EntropyWindow => {
                let w = (v.round() as usize).clamp(16, 4096);
                if w != self.entropy_window {
                    self.entropy_window = w;
                    self.recompute_entropies();
                    self.overview_gen_size = None;
                    self.strip_dirty = true;
                }
            }
        }
        cx.notify();
    }
}

impl Focusable for ParallHexApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ParallHexApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Debounced config save.
        let current = self.current_config();
        if current != self.saved_cfg && self.last_save.elapsed() >= Duration::from_secs(2) {
            config::save(&current);
            self.saved_cfg = current;
            self.last_save = Instant::now();
        }

        // Regenerate the fixed-resolution strip when needed.
        if self.strip_dirty {
            self.strip_dirty = false;
            self.strip_image = self
                .data()
                .map(|d| panes::build_strip_image(d, &self.entropies, self.entropy_window));
        }

        let show_jump = self.show_jump_dialog;
        let no_file = self.mmap.is_none();

        let root = div()
            .id("parallhex-root")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x16161e))
            .text_color(rgb(0xc0caf5))
            .key_context("ParallHex")
            .track_focus(&self.focus_handle(cx))
            .on_action(cx.listener(Self::on_open_file))
            .on_action(cx.listener(Self::on_jump_to_offset))
            .on_action(cx.listener(Self::on_reset_view))
            .on_action(cx.listener(Self::on_reset_settings))
            .on_action(cx.listener(Self::on_zoom_in))
            .on_action(cx.listener(Self::on_zoom_out))
            .on_action(cx.listener(Self::on_nav_left))
            .on_action(cx.listener(Self::on_nav_right))
            .on_action(cx.listener(Self::on_nav_up))
            .on_action(cx.listener(Self::on_nav_down))
            .on_action(cx.listener(Self::on_nav_page_up))
            .on_action(cx.listener(Self::on_nav_page_down))
            .on_action(cx.listener(Self::on_nav_home))
            .on_action(cx.listener(Self::on_nav_end))
            .on_action(cx.listener(Self::on_copy_hex))
            .on_action(cx.listener(Self::on_copy_ascii))
            .on_action(cx.listener(Self::on_clear_selection))
            .on_action(cx.listener(Self::on_jump_submit))
            .on_action(cx.listener(Self::on_jump_cancel))
            .on_any_mouse_down(
                cx.listener(move |this, _: &MouseDownEvent, _: &mut Window, cx| {
                    // Clicking anywhere outside the dropdown toggle collapses the
                    // colormap menu. The toggle flags itself on mouse-down so its
                    // own click (handled next) can toggle the menu instead.
                    let on_toggle = this.colormap_toggle_down;
                    this.colormap_toggle_down = false;
                    if this.colormap_menu_open && !on_toggle {
                        this.colormap_menu_open = false;
                        cx.notify();
                    }
                    // Clear any divider resize left over from a release that
                    // happened outside the window.
                    if this.resizing_divider.is_some() {
                        this.resizing_divider = None;
                        cx.notify();
                    }
                }),
            )
            // The root is always under the pointer, so it keeps a divider
            // resize alive after the cursor leaves the thin divider strip.
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                if this.on_divider_mouse_move(ev.position) {
                    cx.notify();
                }
            }))
            .capture_any_mouse_up(cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                if this.resizing_divider.is_some() {
                    this.resizing_divider = None;
                    cx.notify();
                }
            }))
            .child(self.top_bar(cx))
            .when(no_file, |d| {
                d.flex_1().flex().items_center().justify_center().child(
                    div()
                        .text_color(rgb(0x565f89))
                        .child("No file loaded.\n\nOpen a binary file to explore its bytes."),
                )
            })
            .when(!no_file, |d| {
                d.flex_1()
                    .flex()
                    .flex_row()
                    .min_h_0()
                    .child(self.overview_column(cx))
                    .child(Self::column_divider(cx, DividerKind::OverviewPixels))
                    .child(self.pixels_column(cx))
                    .child(Self::column_divider(cx, DividerKind::PixelsHex))
                    .child(self.hex_column(cx))
            })
            .child(self.status_bar(cx))
            .when(show_jump, |d| d.child(self.jump_dialog(cx)));

        let _ = window;
        root
    }
}

/// The width a divider drag should produce: the width at drag start plus the
/// pointer delta, rounded to whole pixels and clamped to the column's range.
fn divider_width(start_w: f32, dx: f32, min: f32, max: f32) -> f32 {
    (start_w + dx).round().clamp(min, max)
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// A reusable button wired up through `on_click` (so it works without the
/// element holding keyboard focus). The callback receives the view, the
/// window and a context, mirroring the `Context::listener` signature.
fn button(
    cx: &mut Context<ParallHexApp>,
    label: &'static str,
    on_click: impl Fn(&mut ParallHexApp, &mut Window, &mut Context<ParallHexApp>) + 'static,
) -> impl IntoElement {
    div()
        .id(label)
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgb(0x24283b))
        .text_color(rgb(0xc0caf5))
        .cursor_pointer()
        .active(|s| s.opacity(0.7))
        .hover(|s| s.bg(rgb(0x3b4261)))
        .on_click(cx.listener(move |this, _ev: &ClickEvent, window, cx| {
            on_click(this, window, cx);
        }))
        .child(label)
}

/// A small color swatch previewing what a colormap looks like, shown in the
/// pixels-column dropdown toggle.
fn swatch(cm: Colormap) -> impl IntoElement {
    let color = match cm {
        Colormap::Greyscale => rgb(0x9aa5ce),
        Colormap::Entropy => color::entropy_color(4.0),
        Colormap::ByteClass => color::class_color(0x41),
    };
    div().w(px(10.)).h(px(10.)).rounded_md().bg(color)
}

/// A dark background quad for empty canvas areas.
fn quad_dark(bounds: Bounds<Pixels>) -> gpui::PaintQuad {
    let bg: Background = rgb(0x0c0d14).into();
    quad(
        bounds,
        px(0.),
        bg,
        px(0.),
        transparent_black(),
        BorderStyle::default(),
    )
}

/// Map a slider drag position (window coords) to a value, using the stored
/// bounds from the previous frame.
fn slider_value_at(kind: SliderKind, pos: Point<Pixels>, bounds: Bounds<Pixels>) -> Option<f32> {
    if bounds.size.width.to_f64() <= 0.0 {
        return None;
    }
    let w = bounds.size.width.to_f64() as f32;
    let t = ((pos.x - bounds.left()).to_f64() as f32 / w).clamp(0.0, 1.0);
    let (min, max) = match kind {
        SliderKind::HexZoom => (panes::HEX_ZOOM_MIN, panes::HEX_ZOOM_MAX),
        SliderKind::PixelZoom => (panes::PIXEL_ZOOM_MIN, panes::PIXEL_ZOOM_MAX),
        SliderKind::EntropyWindow => (16.0, 4096.0),
    };
    Some(min * (max / min).powf(t))
}

/// Normalized slider position (0..1) for a value on a log scale.
fn slider_t(value: f32, min: f32, max: f32) -> f32 {
    ((value.clamp(min, max) / min).ln() / (max / min).ln()).clamp(0.0, 1.0)
}

/// Convert a scroll delta into a zoom factor (1.0 = no change).
fn wheel_zoom_factor(delta: &ScrollDelta) -> f32 {
    let lines = match delta {
        ScrollDelta::Lines(p) => p.y,
        ScrollDelta::Pixels(p) => p.y.to_f64() as f32 / 16.0,
    };
    if lines == 0.0 {
        1.0
    } else {
        (1.0 + 0.15 * lines.abs()).powf(lines.signum())
    }
}

/// Draw a column header: bold title, muted range label, trailing widgets.
fn column_header(
    title: &'static str,
    range: Option<String>,
    trailing: impl IntoElement,
) -> impl IntoElement {
    div()
        .w_full()
        .px_2()
        .py_1()
        .flex()
        .flex_col()
        .gap_1()
        .border_b_1()
        .border_color(rgb(0x232740))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(div().text_color(rgb(0x9d7cd8)).child(title))
                .child(trailing),
        )
        .when(range.is_some(), |d| {
            d.child(
                div()
                    .text_color(rgb(0x565f89))
                    .text_size(px(11.))
                    .child(range.unwrap()),
            )
        })
}

/// Pick the best available monospace font family from the system font list.
fn pick_monospace_family(window: &Window) -> SharedString {
    let names = window.text_system().all_font_names();
    let lower: Vec<String> = names.iter().map(|s| s.to_lowercase()).collect();
    let prefs = [
        "hack",
        "jetbrains mono",
        "fira mono",
        "fira code",
        "dejavu sans mono",
        "liberation mono",
        "ubuntu mono",
        "menlo",
        "monaco",
        "consolas",
        "cascadia mono",
        "sf mono",
    ];
    // First preference present in the system font list.
    for pref in prefs {
        if let Some(idx) = lower.iter().position(|n| n.contains(pref)) {
            return SharedString::from(names[idx].clone());
        }
    }
    // Fall back to any family whose name contains "mono".
    if let Some(idx) = lower.iter().position(|n| n.contains("mono")) {
        return SharedString::from(names[idx].clone());
    }
    // Last resort: the default UI font (glyph widths will be proportional).
    SharedString::from("Helvetica")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{NavigationAction, ParallHexApp, divider_width};

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum NavKey {
        Left,
        Right,
        Up,
        Down,
        PageUp,
        PageDown,
        Home,
        End,
    }

    fn action(key: NavKey) -> NavigationAction {
        NavigationAction::new(
            key == NavKey::Left,
            key == NavKey::Right,
            key == NavKey::Up,
            key == NavKey::Down,
            key == NavKey::PageUp,
            key == NavKey::PageDown,
            key == NavKey::Home,
        )
    }

    fn next(key: NavKey, cur: usize, bpr: usize, page_bytes: usize, len: usize) -> usize {
        ParallHexApp::nav_next(action(key), cur.min(len - 1), bpr, page_bytes, len)
    }

    #[test]
    fn arrows_clamp_at_boundaries() {
        let len = 1000usize;
        let bpr = 32usize;
        assert_eq!(next(NavKey::Left, 0, bpr, 0, len), 0);
        assert_eq!(next(NavKey::Up, 0, bpr, 0, len), 0);
        assert_eq!(next(NavKey::Right, len - 1, bpr, 0, len), len - 1);
        assert_eq!(next(NavKey::Down, len - 1, bpr, 0, len), len - 1);
        assert_eq!(next(NavKey::Right, len - 2, bpr, 0, len), len - 1);
        assert_eq!(next(NavKey::Down, len - 32, bpr, 0, len), len - 1);
        assert_eq!(next(NavKey::Down, len - 64, bpr, 0, len), len - 64 + bpr);
    }

    #[test]
    fn page_keys_clamp_at_boundaries() {
        let len = 1000usize;
        let bpr = 32usize;
        let page_bytes = 448usize;
        assert_eq!(next(NavKey::PageUp, 10, bpr, page_bytes, len), 0);
        assert_eq!(
            next(NavKey::PageDown, len - 5, bpr, page_bytes, len),
            len - 1
        );
        assert_eq!(
            next(NavKey::PageDown, 100, bpr, page_bytes, len),
            100 + page_bytes
        );
        assert_eq!(
            next(NavKey::PageUp, 500, bpr, page_bytes, len),
            500 - page_bytes
        );
    }

    #[test]
    fn home_end_jump_to_boundaries() {
        let len = 1000usize;
        assert_eq!(next(NavKey::Home, 500, 32, 448, len), 0);
        assert_eq!(next(NavKey::End, 500, 32, 448, len), len - 1);
        assert_eq!(next(NavKey::End, 0, 32, 448, len), len - 1);
    }

    #[test]
    fn stale_selection_is_clamped_before_moving() {
        let len = 1000usize;
        assert_eq!(next(NavKey::Right, 5000, 32, 448, len), len - 1);
        assert_eq!(next(NavKey::PageDown, 5000, 32, 448, len), len - 1);
        assert_eq!(next(NavKey::Up, 5000, 32, 448, len), len - 1 - 32);
    }

    #[test]
    fn zoom_step_multiplies_and_clamps() {
        assert_eq!(
            crate::panes::zoom_step(1.0, crate::panes::ZOOM_STEP, 0.5, 4.0),
            1.25
        );
        assert_eq!(
            crate::panes::zoom_step(4.0, crate::panes::ZOOM_STEP, 0.5, 4.0),
            4.0
        );
        assert_eq!(crate::panes::zoom_step(0.5, 0.8, 0.5, 4.0), 0.5);
        assert_eq!(
            crate::panes::zoom_step(20.0, crate::panes::ZOOM_STEP, 1.0, 24.0),
            24.0
        );
    }

    #[test]
    fn parse_hex_with_prefix() {
        assert_eq!(ParallHexApp::parse_offset("0x1F"), Some(31));
        assert_eq!(ParallHexApp::parse_offset("0X1000"), Some(4096));
    }

    #[test]
    fn parse_hex_without_prefix() {
        assert_eq!(ParallHexApp::parse_offset("1F"), Some(31));
        assert_eq!(ParallHexApp::parse_offset("DEADBEEF"), Some(0xDEAD_BEEF));
    }

    #[test]
    fn parse_allows_underscores_and_whitespace() {
        assert_eq!(ParallHexApp::parse_offset(" 0x1_000 "), Some(4096));
    }

    #[test]
    fn parse_invalid_returns_none() {
        assert_eq!(ParallHexApp::parse_offset(""), None);
        assert_eq!(ParallHexApp::parse_offset("xyz"), None);
        assert_eq!(ParallHexApp::parse_offset("0x"), None);
        assert_eq!(ParallHexApp::parse_offset("-5"), None);
    }

    #[test]
    fn divider_width_follows_delta() {
        assert_eq!(divider_width(200.0, 100.0, 140.0, 2000.0), 300.0);
        assert_eq!(divider_width(320.0, -50.0, 200.0, 3000.0), 270.0);
    }

    #[test]
    fn divider_width_rounds_to_pixels() {
        assert_eq!(divider_width(200.0, 3.4, 140.0, 2000.0), 203.0);
        assert_eq!(divider_width(200.0, 3.6, 140.0, 2000.0), 204.0);
    }

    #[test]
    fn divider_width_clamps_to_range() {
        // Overshooting far past the max clamps, not overflows.
        assert_eq!(divider_width(300.0, 100_000.0, 140.0, 2000.0), 2000.0);
        assert_eq!(divider_width(300.0, -100_000.0, 140.0, 2000.0), 140.0);
        // The pixels column has a wider range.
        assert_eq!(divider_width(320.0, 4000.0, 200.0, 3000.0), 3000.0);
    }
}
