//! Application state and the gpui view shell.

use std::fs::File;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, Background, BorderStyle, Bounds, ClickEvent, ClipboardItem, Context, CursorStyle,
    Decorations, Entity, FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, RenderImage, ResizeEdge, ScrollDelta,
    ScrollWheelEvent, SharedString, Window, canvas, div, font, point, prelude::*, px, quad, rgb,
    rgba, size, transparent_black,
};

use memmap2::{Mmap, MmapOptions};

use crate::color::{self, Colormap};
use crate::config;
use crate::entropy;
use crate::jump::{JumpField, JumpFieldEvent};
use crate::panes;
use crate::{
    ClearSelection, CopySelectionAscii, CopySelectionHex, JumpCancel, JumpSubmit, JumpToOffset,
    NavigateDown, NavigateEnd, NavigateHome, NavigateLeft, NavigatePageDown, NavigatePageUp,
    NavigateRight, NavigateUp, OpenFile, Quit, ResetColumns, ResetSettings, ResetView, ZoomIn,
    ZoomOut,
};

/// Size of the horizontal whole-file preview strip in the top bar.
const STRIP_W: f32 = 320.0;
const STRIP_H: f32 = 36.0;

/// Button labels naming the `secondary` accelerator, which `main.rs` binds to
/// Cmd on macOS and Ctrl elsewhere — the label has to follow the binding.
const JUMP_BUTTON_LABEL: &str = if cfg!(target_os = "macos") {
    "Jump to offset… (Cmd+G)"
} else {
    "Jump to offset… (Ctrl+G)"
};

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

// ---------------------------------------------------------------------------
// The main view
// ---------------------------------------------------------------------------

/// How a copied byte range is rendered to text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CopyKind {
    /// Space-separated uppercase hex pairs, e.g. `DE AD BE EF`.
    Hex,
    /// Printable ASCII, non-printable bytes as `.`.
    Ascii,
}

/// Render `range` of `data` for the clipboard, or `None` when the range is
/// empty after clamping to the file. Shared by the copy actions and the hex
/// column's right-click copy so the two can't drift apart.
fn selection_text(data: &[u8], range: &Range<usize>, kind: CopyKind) -> Option<String> {
    let start = range.start.min(data.len());
    let end = range.end.min(data.len());
    if start >= end {
        return None;
    }
    let bytes = &data[start..end];
    Some(match kind {
        CopyKind::Hex => bytes
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" "),
        CopyKind::Ascii => bytes.iter().map(|&b| color::printable(b)).collect(),
    })
}

/// Which slider the pointer is currently dragging.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SliderKind {
    PixelZoom,
    EntropyWindow,
}

/// Which column a per-panel control belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Panel {
    Overview,
    Zoom,
    Hex,
}

/// Which column divider the pointer is currently dragging.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DividerKind {
    OverviewZoom,
    ZoomHex,
}

/// Clamp ranges for the two drag-resizable column widths.
const OVERVIEW_W_MIN: f32 = 140.0;
const OVERVIEW_W_MAX: f32 = 2000.0;
const ZOOM_W_MIN: f32 = 200.0;
const ZOOM_W_MAX: f32 = 3000.0;

/// The many booleans track per-column drag/interaction bookkeeping; a
/// state machine would add more complexity than it removes.
#[allow(clippy::struct_excessive_bools)]
pub struct ParallHexApp {
    pub file_path: Option<PathBuf>,
    pub mmap: Option<Arc<Mmap>>,
    pub file_size: usize,

    pub entropy_window: usize,

    // Each panel colors its bytes independently (SPECS §3.C).
    pub overview_colormap: Colormap,
    pub zoom_colormap: Colormap,
    pub hex_colormap: Colormap,

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

    // Three-column layout. The zoom column is the only one that zooms; the
    // shared scroll position is a *byte anchor* because each panel derives its
    // own row length from its own width, so rows no longer line up (SPECS
    // §4.2). `hex_bpr` / `zoom_bpr` are recomputed in each canvas's prepaint
    // from its measured width, and reused for hit-testing and navigation.
    pub pixel_zoom: f32,
    pub scroll_offset: usize,
    pub hex_bpr: usize,
    pub zoom_bpr: usize,
    pub hex_bounds: Bounds<Pixels>,
    pub pixels_bounds: Bounds<Pixels>,
    pub overview_bounds: Bounds<Pixels>,
    pub strip_bounds: Bounds<Pixels>,
    pub scrollbar_bounds: Bounds<Pixels>,

    // Each panel's currently visible byte range, recorded in its prepaint. A
    // panel draws the *next* panel's range as a band, so the overview shows
    // where the zoom column is looking and the zoom column shows where the hex
    // column is (SPECS §4.1).
    pub zoom_view: Range<usize>,
    pub hex_view: Range<usize>,
    // Anchor as a fraction of the file, for the status bar percentage.
    pub view_frac: f32,
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
    // True while the hex column's scrollbar thumb is being dragged.
    pub scrollbar_dragging: bool,

    // Offset under the pointer while hovering the overview previews
    // (top-bar strip / left overview).
    pub overview_hover_offset: Option<usize>,

    // Jump-to-offset dialog (Ctrl+G).
    pub show_jump_dialog: bool,
    pub jump_error: Option<String>,
    pub jump_field: Entity<JumpField>,

    // Persisted layout prefs (written a couple of seconds after a change).
    pub overview_width: f32,
    pub zoom_width: f32,
    pub saved_cfg: config::Config,
    pub last_save: Instant,

    // Last captured window geometry, persisted so position/size can be
    // restored on the next launch. Captured live each frame (rounded to
    // whole pixels) so a move/resize lands in the config file.
    pub window_bounds: Option<(f32, f32, f32, f32)>,
    pub window_maximized: bool,

    // Drag state for the column divider resize handles.
    pub resizing_divider: Option<DividerKind>,
    pub divider_start_x: f32,
    pub divider_start_w: f32,

    // Header sliders.
    pub pixels_slider_bounds: Bounds<Pixels>,
    pub entropy_slider_bounds: Bounds<Pixels>,
    pub dragging_slider: Option<SliderKind>,

    // Colormap selector dropdown in the pixels-column header.
    pub open_colormap_menu: Option<Panel>,
    // True while a mouse-down that landed anywhere inside a colormap picker is
    // in flight, so the root's outside-click handler doesn't close the menu on
    // mouse-*down* — which would destroy the option pill before its click
    // completes, making the picker look dead.
    pub colormap_click_inside: bool,

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
            entropy_window: prefs.entropy_window.clamp(16, 4096),
            overview_colormap: prefs.overview_colormap,
            zoom_colormap: prefs.zoom_colormap,
            hex_colormap: prefs.hex_colormap,
            scroll_to_offset: None,
            entropies: Arc::new(Vec::new()),
            overview_image: None,
            overview_cells: None,
            overview_gen_size: None,
            strip_image: None,
            strip_dirty: false,
            pixel_zoom: prefs
                .pixel_zoom
                .clamp(panes::PIXEL_ZOOM_MIN, panes::PIXEL_ZOOM_MAX),
            scroll_offset: 0,
            hex_bpr: 32,
            zoom_bpr: 64,
            hex_bounds: Bounds::default(),
            pixels_bounds: Bounds::default(),
            overview_bounds: Bounds::default(),
            strip_bounds: Bounds::default(),
            scrollbar_bounds: Bounds::default(),
            zoom_view: 0..0,
            hex_view: 0..0,
            view_frac: 0.0,
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
            scrollbar_dragging: false,
            overview_hover_offset: None,
            show_jump_dialog: false,
            jump_error: None,
            jump_field,
            overview_width: prefs.overview_width.clamp(OVERVIEW_W_MIN, OVERVIEW_W_MAX),
            zoom_width: prefs.zoom_width.clamp(ZOOM_W_MIN, ZOOM_W_MAX),
            window_bounds: prefs.window_bounds,
            window_maximized: prefs.window_maximized,
            saved_cfg: prefs,
            last_save: Instant::now(),
            resizing_divider: None,
            divider_start_x: 0.0,
            divider_start_w: 0.0,
            pixels_slider_bounds: Bounds::default(),
            entropy_slider_bounds: Bounds::default(),
            dragging_slider: None,
            open_colormap_menu: None,
            colormap_click_inside: false,
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

    fn colormap(&self, panel: Panel) -> Colormap {
        match panel {
            Panel::Overview => self.overview_colormap,
            Panel::Zoom => self.zoom_colormap,
            Panel::Hex => self.hex_colormap,
        }
    }

    fn set_colormap(&mut self, panel: Panel, cm: Colormap) {
        match panel {
            Panel::Overview => {
                self.overview_colormap = cm;
                // The thumbnails bake the colormap into their pixels, so both
                // have to be regenerated when it changes.
                self.overview_gen_size = None;
                self.strip_dirty = true;
            }
            Panel::Zoom => self.zoom_colormap = cm,
            Panel::Hex => self.hex_colormap = cm,
        }
    }

    /// Recompute the zoom column's layout from its measured canvas: how many
    /// bytes fit per row at the target block size, and which byte range that
    /// makes visible (the overview draws this as its band).
    fn measure_zoom(&mut self, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        self.pixels_bounds = bounds;
        // Redistribute the bytes so a row spans the panel exactly: as many
        // target-sized blocks as fit, then widened to fill the width.
        let w = bounds.size.width.to_f64() as f32;
        let bpr = panes::zoom_bytes_per_row(w, self.zoom_target());
        let changed = bpr != self.zoom_bpr;
        self.zoom_bpr = bpr;
        let block = panes::zoom_block_w(w, bpr);
        let rows = panes::visible_rows(bounds.size.height.to_f64() as f32, block);
        let first = panes::first_row_centred(self.scroll_offset, bpr, rows);
        self.zoom_view = first..(first + rows * bpr).min(self.file_size);
        if changed {
            // The paint closure captured the old row length; ask for a frame
            // with the new one.
            cx.notify();
        }
    }

    /// The zoom column's *target* block size, as set by the slider.
    fn zoom_target(&self) -> f32 {
        self.pixel_zoom
            .clamp(panes::PIXEL_ZOOM_MIN, panes::PIXEL_ZOOM_MAX)
    }

    /// Actual size of one byte's block. The bytes are redistributed across the
    /// panel so a row spans its full width, so this is the target widened to
    /// divide the width exactly. Blocks are square, so it is the row height too.
    fn zoom_row_h(&self) -> f32 {
        panes::zoom_block_w(
            self.pixels_bounds.size.width.to_f64() as f32,
            self.zoom_bpr.max(1),
        )
    }

    /// Clamp the shared anchor to the hex column's last row (SPECS §4.2).
    fn clamp_anchor(&mut self) {
        self.scroll_offset = self
            .scroll_offset
            .min(panes::max_anchor(self.file_size, self.hex_bpr.max(8)));
    }

    /// Scroll by whole rows of `panel`, negative for up.
    fn scroll_rows_by(&mut self, panel: Panel, rows: i32) {
        let bpr = match panel {
            Panel::Zoom => self.zoom_bpr.max(1),
            _ => self.hex_bpr.max(8),
        };
        let delta = rows.unsigned_abs() as usize * bpr;
        self.scroll_offset = if rows < 0 {
            self.scroll_offset.saturating_sub(delta)
        } else {
            self.scroll_offset.saturating_add(delta)
        };
        self.clamp_anchor();
    }

    fn recompute_entropies(&mut self) {
        self.entropies = Arc::new(match self.data() {
            Some(d) => entropy::block_entropies(d, self.entropy_window),
            None => Vec::new(),
        });
    }

    /// Rebuild the fixed-resolution top-bar strip when the file, entropy window
    /// or its colormap changed.
    fn refresh_strip(&mut self) {
        if !self.strip_dirty {
            return;
        }
        self.strip_dirty = false;
        let entropies = self.entropies.clone();
        let window = self.entropy_window;
        let colormap = self.overview_colormap;
        self.strip_image = self
            .data()
            .map(|d| panes::build_strip_image(d, &entropies, window, colormap));
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
            entropy_window: self.entropy_window,
            pixel_zoom: self.pixel_zoom,
            overview_colormap: self.overview_colormap,
            zoom_colormap: self.zoom_colormap,
            hex_colormap: self.hex_colormap,
            overview_width: self.overview_width.round(),
            zoom_width: self.zoom_width.round(),
            window_bounds: self.window_bounds,
            window_maximized: self.window_maximized,
        }
    }

    /// Remember the window's position/size so it can be persisted and
    /// restored next launch. While maximized, the last *windowed* bounds are
    /// kept as the restore size (gpui's `WindowBounds::Maximized` treats the
    /// given bounds as the un-maximize geometry). Values are rounded to
    /// whole pixels so an unchanged window doesn't keep rewriting the file.
    fn capture_window_geometry(&mut self, window: &Window) {
        self.window_maximized = window.is_maximized();
        // While maximized or fullscreen the window bounds are the screen
        // size, not a restore geometry — keep the last windowed bounds.
        if self.window_maximized || window.is_fullscreen() {
            return;
        }
        let bounds = window.bounds();
        let (left, top, width, height) = (
            bounds.origin.x.to_f64() as f32,
            bounds.origin.y.to_f64() as f32,
            bounds.size.width.to_f64() as f32,
            bounds.size.height.to_f64() as f32,
        );
        self.window_bounds = Some((left.round(), top.round(), width.round(), height.round()));
    }

    /// Restore every persisted setting to its default. The window geometry
    /// is captured live each frame and kept across the reset (it's a window
    /// manager concern, not a UI preference).
    fn reset_all_settings(&mut self, cx: &mut Context<Self>) {
        let defaults = config::Config {
            window_bounds: self.window_bounds,
            window_maximized: self.window_maximized,
            ..config::Config::default()
        };
        self.entropy_window = defaults.entropy_window;
        self.pixel_zoom = defaults.pixel_zoom;
        // Every field of `defaults` is written to disk below, so each one must
        // also be applied here or the file would disagree with the UI until
        // the next debounced save undid the reset.
        self.overview_colormap = defaults.overview_colormap;
        self.zoom_colormap = defaults.zoom_colormap;
        self.hex_colormap = defaults.hex_colormap;
        self.overview_width = defaults.overview_width;
        self.zoom_width = defaults.zoom_width;
        self.scroll_offset = 0;
        self.open_colormap_menu = None;
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
        self.scroll_offset = 0;
        self.scroll_to_offset = None;
        self.hovered_offset = None;
        self.selected_offset = None;
        self.selection_range = None;
        self.drag_start = None;
        self.overview_hover_offset = None;
        self.show_jump_dialog = false;
        self.jump_error = None;
        self.open_colormap_menu = None;
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

    /// Quit the application (Cmd/Ctrl+Q). Compositors that don't draw a
    /// titlebar / close button leave no other way to exit the window.
    fn on_quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        config::save(&self.current_config());
        cx.quit();
    }

    fn on_jump_to_offset(&mut self, _: &JumpToOffset, window: &mut Window, cx: &mut Context<Self>) {
        self.open_jump_dialog(window, cx);
    }

    fn on_reset_view(&mut self, _: &ResetView, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_offset = 0;
        cx.notify();
    }

    /// Reset the two drag-resizable column widths to their defaults (the
    /// debounced config save persists them a couple of seconds later).
    fn on_reset_columns(&mut self, _: &ResetColumns, _: &mut Window, cx: &mut Context<Self>) {
        let defaults = config::Config::default();
        self.overview_width = defaults.overview_width;
        self.zoom_width = defaults.zoom_width;
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
        // Only the zoom column zooms; the hex text size is fixed.
        if self.pixels_bounds.contains(&p) {
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
        // The hex column is the scroll reference, so a page is its visible rows.
        let bpr = self.hex_bpr.max(8);
        let page_bytes = panes::visible_rows(self.view_height, panes::BLOCK_H) * bpr;

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
        self.copy_selection(CopyKind::Hex, cx);
    }

    fn on_copy_ascii(&mut self, _: &CopySelectionAscii, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_selection(CopyKind::Ascii, cx);
    }

    fn on_clear_selection(&mut self, _: &ClearSelection, _: &mut Window, cx: &mut Context<Self>) {
        self.selection_range = None;
        cx.notify();
    }

    /// The byte range a copy applies to: the selection when there is one,
    /// otherwise the single byte under the pointer (or the cursor).
    fn copy_range(&self) -> Option<Range<usize>> {
        self.selection_range.clone().or_else(|| {
            let o = self.hovered_offset.or(self.selected_offset)?;
            Some(o..o + 1)
        })
    }

    fn copy_selection(&mut self, kind: CopyKind, cx: &mut Context<Self>) {
        let Some(d) = self.data() else { return };
        let Some(range) = self.copy_range() else {
            return;
        };
        if let Some(s) = selection_text(d, &range, kind) {
            cx.write_to_clipboard(ClipboardItem::new_string(s));
        }
    }

    fn on_jump_submit(&mut self, _: &JumpSubmit, _: &mut Window, cx: &mut Context<Self>) {
        if !self.show_jump_dialog {
            return;
        }
        let text = self.jump_field.read(cx).content().to_owned();
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
        self.open_colormap_menu = None;
        window.focus(&self.jump_field.read(cx).focus_handle(cx));
        cx.notify();
    }

    // ----- mouse handlers (hex column) -----

    fn hex_offset_at_pos(&self, window: &mut Window, pos: Point<Pixels>) -> Option<usize> {
        let local = self.hex_bounds.localize(&pos)?;
        let bpr = self.hex_bpr.max(8);
        let font = font(self.mono_family.clone());
        let char_w = panes::hex_char_width(window, &font, px(panes::HEX_FONT_SIZE));
        let geo = panes::RowGeo::new(char_w, bpr);
        panes::hex_offset_at(local, &geo, self.hex_view.start, self.file_size)
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
                    // This handler has no `Context`, so the text is parked in
                    // `pending_copy` for the caller to put on the clipboard.
                    if let Some(d) = self.data()
                        && let Some(range) = self.copy_range()
                    {
                        self.pending_copy = selection_text(d, &range, CopyKind::Hex);
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
        // The hex text size is fixed, so there is nothing to zoom here.
        self.scroll_by_wheel(Panel::Hex, &event.delta, panes::BLOCK_H);
        cx.notify();
    }

    // ----- mouse handlers (pixels column) -----

    fn pixels_offset_at(&self, pos: Point<Pixels>) -> Option<usize> {
        let local = self.pixels_bounds.localize(&pos)?;
        let bpr = self.zoom_bpr.max(1);
        panes::zoom_offset_at(
            local,
            bpr,
            self.zoom_view.start,
            self.zoom_row_h(),
            self.file_size,
        )
    }

    fn on_pixels_mouse_down(&mut self, event: &MouseDownEvent) {
        self.pixels_dragging = true;
        self.last_pixels_y = Some(event.position.y.to_f64() as f32);
        if event.button == MouseButton::Left
            && let Some(off) = self.pixels_offset_at(event.position)
        {
            self.selected_offset = Some(off);
            self.hovered_offset = Some(off);
        }
    }

    fn on_pixels_mouse_move(&mut self, event: &MouseMoveEvent) {
        let y = event.position.y.to_f64() as f32;
        if self.pixels_dragging
            && event.dragging()
            && let Some(last) = self.last_pixels_y
        {
            // Content follows the cursor: dragging down (dy > 0) shows
            // earlier rows, so the anchor moves back through the file.
            let rows = ((y - last) / self.zoom_row_h()).round() as i32;
            if rows != 0 {
                self.scroll_rows_by(Panel::Zoom, -rows);
                self.last_pixels_y = Some(y);
            }
        }
        if !self.pixels_dragging {
            self.last_pixels_y = Some(y);
        }
        self.hovered_offset = self.pixels_offset_at(event.position);
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
            self.scroll_by_wheel(Panel::Zoom, &event.delta, self.zoom_row_h());
        }
    }

    // ----- scroll helpers -----

    /// Convert a wheel delta into whole rows of `panel` and move the anchor.
    fn scroll_by_wheel(&mut self, panel: Panel, delta: &ScrollDelta, row_h: f32) {
        let pixels = delta.pixel_delta(px(16.0)).y.to_f64() as f32;
        // Positive wheel-up delta moves back through the file.
        let rows = -(pixels / row_h.max(1.0)).round() as i32;
        if rows != 0 {
            self.scroll_rows_by(panel, rows);
        }
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
        let w = self.strip_bounds.size.width.to_f64() as f32;
        // The bounds are zero until the strip's first prepaint; dividing by
        // that width would yield NaN.
        if self.file_size == 0 || w <= 0.0 {
            return None;
        }
        let t = ((pos.x - self.strip_bounds.left()).to_f64() as f32 / w).clamp(0.0, 1.0);
        Some(((t * self.file_size as f32) as usize).min(self.file_size.saturating_sub(1)))
    }

    fn on_overview_move(&mut self, pos: Point<Pixels>, dragging: bool) {
        if self.overview_dragging && dragging {
            if let Some(off) = self.overview_offset_at(pos) {
                self.jump_to(off);
            }
        } else {
            // Assign unconditionally: a miss inside the pane (empty file, no
            // cells yet) must clear the preview rather than leave it latched
            // on the last hit, because `byte_readout` prefers it over the
            // hex/pixels hover.
            self.overview_hover_offset = self.overview_offset_at(pos);
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
        } else {
            self.overview_hover_offset = self.strip_offset_at(pos);
        }
    }

    /// Drop the overview/strip hover preview when the pointer leaves those
    /// panes, so the status-bar readout goes back to tracking the hex and
    /// pixels columns (and the keyboard selection).
    fn on_overview_hover(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if !hovered && self.overview_hover_offset.take().is_some() {
            cx.notify();
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

    // ----- hex column scrollbar -----

    /// Move the anchor to the position a pointer at `pos` selects on the hex
    /// column's scrollbar.
    fn on_scrollbar_drag(&mut self, pos: Point<Pixels>) {
        let track = self.scrollbar_bounds;
        let track_h = track.size.height.to_f64() as f32;
        if track_h <= 0.0 {
            return;
        }
        let y = (pos.y - track.top()).to_f64() as f32;
        let visible = self.hex_view.len().max(1);
        let last = panes::max_anchor(self.file_size, self.hex_bpr.max(8));
        self.scroll_offset = panes::scrollbar_anchor_at(y, track_h, last, visible, self.file_size);
        self.clamp_anchor();
    }

    /// The hex column's scrollbar: the whole file as a track, the visible rows
    /// as a thumb. The hex column is the scroll reference, so this drives the
    /// shared anchor and the other panels follow.
    fn hex_scrollbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let anchor = self.scroll_offset;
        let visible = self.hex_view.len();
        let len = self.file_size;
        let last = panes::max_anchor(len, self.hex_bpr.max(8));
        div()
            .w(px(panes::SCROLLBAR_W))
            .h_full()
            .flex_shrink_0()
            .cursor(CursorStyle::Arrow)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    this.scrollbar_dragging = true;
                    this.on_scrollbar_drag(ev.position);
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                if this.scrollbar_dragging && ev.dragging() {
                    this.on_scrollbar_drag(ev.position);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                    this.scrollbar_dragging = false;
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                    this.scrollbar_dragging = false;
                    cx.notify();
                }),
            )
            .child(
                canvas(
                    move |bounds, _window, cx| {
                        entity.update(cx, |this, _| this.scrollbar_bounds = bounds);
                    },
                    move |bounds, (), window, _cx| {
                        panes::paint_scrollbar(window, bounds, anchor, last, visible, len);
                    },
                )
                .size_full(),
            )
    }

    // ----- column divider drag -----

    /// Begin dragging a column divider: remember the pointer x and the width
    /// being changed so the drag delta can be applied exactly.
    fn on_divider_mouse_down(&mut self, kind: DividerKind, pos: Point<Pixels>) {
        self.resizing_divider = Some(kind);
        self.divider_start_x = pos.x.to_f64() as f32;
        self.divider_start_w = match kind {
            DividerKind::OverviewZoom => self.overview_width,
            DividerKind::ZoomHex => self.zoom_width,
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
            DividerKind::OverviewZoom => (OVERVIEW_W_MIN, OVERVIEW_W_MAX),
            DividerKind::ZoomHex => (ZOOM_W_MIN, ZOOM_W_MAX),
        };
        let w = divider_width(self.divider_start_w, dx, min, max);
        match kind {
            DividerKind::OverviewZoom => {
                let changed = (w - self.overview_width).abs() > 0.5;
                self.overview_width = w;
                changed
            }
            DividerKind::ZoomHex => {
                let changed = (w - self.zoom_width).abs() > 0.5;
                self.zoom_width = w;
                changed
            }
        }
    }

    fn on_divider_mouse_up(&mut self) {
        self.resizing_divider = None;
    }

    // ----- bottom status bar -----

    /// The central area: the no-file placeholder, or the three columns with
    /// their drag dividers. It must be a dedicated child of the column root —
    /// mutating the root itself with `.when(...).flex_row()` would flatten the
    /// columns next to the top and status bars.
    fn central_area(&mut self, cx: &mut Context<Self>, no_file: bool) -> impl IntoElement {
        div()
            .flex_1()
            .min_h_0()
            .when(no_file, |d| {
                d.flex().items_center().justify_center().child(
                    div()
                        .text_color(rgb(0x565f89))
                        .child("No file loaded.\n\nOpen a binary file to explore its bytes."),
                )
            })
            .when(!no_file, |d| {
                d.flex()
                    .flex_row()
                    .min_h_0()
                    .child(self.overview_column(cx))
                    .child(Self::column_divider(cx, DividerKind::OverviewZoom))
                    .child(self.pixels_column(cx))
                    .child(Self::column_divider(cx, DividerKind::ZoomHex))
                    .child(self.hex_column(cx))
            })
    }

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
                    .child(
                        div()
                            .text_color(rgb(0x565f89))
                            .child(format!("px {}", self.pixel_zoom.round() as u32)),
                    )
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
        let bpr = self.hex_bpr.max(8);
        let total_rows = self.file_size.div_ceil(bpr);
        let first = self.hex_view.start / bpr;
        let vis = panes::visible_rows(self.view_height, panes::BLOCK_H);
        let last = (first + vis).min(total_rows);
        let pct = (self.view_frac * 100.0).round() as u32;
        Some(format!(
            "{bpr} B/row · rows {first}–{last} / {total_rows} · {pct}%"
        ))
    }

    // ----- top info bar -----

    /// The top info bar: app title, file name/size and the action controls
    /// (open, entropy window, reset/jump) plus the horizontal
    /// whole-file preview strip. Live readouts live in the bottom status bar.
    #[allow(clippy::too_many_lines)] // single-purpose element builder
    fn top_bar(&mut self, cx: &mut Context<Self>, client_side: bool) -> impl IntoElement {
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
            .child(self.slider(cx, SliderKind::EntropyWindow))
            .child(div().child("Entropy win"))
            .child(button(cx, "Reset view", |this, window, cx| {
                this.on_reset_view(&ResetView, window, cx);
            }))
            .child(button(cx, JUMP_BUTTON_LABEL, |this, window, cx| {
                this.on_jump_to_offset(&JumpToOffset, window, cx);
            }))
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
                    // The title / file-name area doubles as the titlebar drag
                    // handle. The strip and the window buttons are siblings, not
                    // children, so their own clicks never start a window move.
                    .child(
                        div()
                            .id("titlebar-drag")
                            .flex()
                            .flex_1()
                            // Let the title text be the thing that gives way in
                            // a narrow window: without min_w_0 this grow-1 area
                            // refuses to shrink below its text and squeezes the
                            // fixed-size preview strip out of the row instead.
                            .min_w_0()
                            .overflow_hidden()
                            .items_center()
                            .gap_3()
                            .when(client_side, |d| {
                                d.on_mouse_down(
                                    MouseButton::Left,
                                    |_: &MouseDownEvent, window: &mut Window, _: &mut App| {
                                        window.start_window_move();
                                    },
                                )
                                .on_click(
                                    |ev: &ClickEvent, window: &mut Window, _: &mut App| {
                                        if ev.is_right_click() {
                                            window.show_window_menu(ev.position());
                                        } else if ev.click_count() >= 2 {
                                            window.zoom_window();
                                        }
                                    },
                                )
                            })
                            .child(div().text_xl().text_color(rgb(0x7aa2f7)).child("ParallHex"))
                            .child(div().child(format!(
                                "{file_name} · {file_size} bytes ({})",
                                color::human_size(file_size)
                            ))),
                    )
                    .when(has_file, |d| d.child(self.strip(cx)))
                    .when(client_side, |d| d.child(window_buttons(cx))),
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
        let content = self.jump_field.read(cx).content().to_owned();
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
        // The strip is a second whole-file map, so it marks the same range.
        let mark = self.zoom_view.clone();
        div()
            // `on_hover` below needs a stateful element, hence the id.
            .id("preview-strip")
            .w(px(STRIP_W))
            .h(px(STRIP_H))
            // Fixed size: never let a long file name shrink the preview away.
            .flex_shrink_0()
            .rounded_md()
            .overflow_hidden()
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                this.on_strip_move(ev.position, ev.dragging());
                cx.notify();
            }))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.on_overview_hover(*hovered, cx);
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
            .child(
                canvas(
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
                                (!mark.is_empty()).then_some(&mark),
                            );
                        } else {
                            window.paint_quad(quad_dark(bounds));
                        }
                    },
                )
                // A `canvas` has no intrinsic size: without this it lays out
                // zero-height and paints nothing (`Canvas::request_layout` refines
                // `Style::default()`, and it has no children to measure).
                .size_full(),
            )
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
        // The overview marks the range the zoom column is showing.
        let mark = self.zoom_view.clone();
        let overview_colormap = self.overview_colormap;

        let overview_width = self.overview_width;
        let header = column_header(
            "Overview",
            (file_size > 0).then(|| panes::range_label(0, file_size)),
            self.colormap_picker(cx, Panel::Overview),
        );

        div()
            .w(px(overview_width))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(0x12121c))
            .border_r_1()
            .border_color(rgb(0x232740))
            .child(header)
            .child(
                div()
                    // `on_hover` below needs a stateful element, hence the id.
                    .id("overview-canvas")
                    .flex_1()
                    .min_h_0()
                    .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                        this.on_overview_move(ev.position, ev.dragging());
                        cx.notify();
                    }))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        this.on_overview_hover(*hovered, cx);
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
                    .child(
                        canvas(
                            {
                                let entity = entity.clone();
                                move |bounds, _window, cx| {
                                    // Regenerate the thumbnail when the panel resizes.
                                    let w = (bounds.size.width.to_f64() as usize).clamp(64, 512);
                                    let h = (bounds.size.height.to_f64() as usize).clamp(32, 1024);
                                    // Regenerating is O(cells) over the whole file
                                    // and runs inside prepaint, so skip it while a
                                    // divider drag is resizing the panel every
                                    // frame: the existing image scales until the
                                    // drag ends, and the size mismatch makes the
                                    // next frame regenerate once.
                                    let this = entity.read(cx);
                                    let dirty = this.overview_gen_size != Some((w, h))
                                        && this.resizing_divider.is_none();
                                    if dirty {
                                        let img = data.as_deref().map(|d| {
                                            panes::build_overview_image(
                                                d,
                                                &entropies,
                                                entropy_window,
                                                w,
                                                h,
                                                overview_colormap,
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
                                            (!mark.is_empty()).then_some(&mark),
                                        ),
                                        None => {
                                            window.paint_quad(quad_dark(bounds));
                                        }
                                    }
                                }
                            },
                        )
                        .size_full(),
                    )
                    .size_full(),
            )
    }

    /// Middle column: per-byte colormap + entropy bands.
    fn pixels_column(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let data = self.mmap.clone();
        let entropies = self.entropies.clone();
        let entropy_window = self.entropy_window;
        let bpr = self.zoom_bpr.max(1);
        let len = self.file_size;
        let first_row_start = self.zoom_view.start;
        let block = self.zoom_row_h();
        // The zoom column marks the range the hex column is showing.
        let mark = self.hex_view.clone();
        let hovered = self.hovered_offset;
        let sel = self.selection_range.clone();
        let colormap = self.zoom_colormap;

        let range = (len > 0).then(|| {
            let rows = panes::visible_rows(self.view_height, self.zoom_row_h());
            panes::range_label(first_row_start, (first_row_start + rows * bpr).min(len))
        });

        div()
            .w(px(self.zoom_width))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(0x10101a))
            .border_r_1()
            .border_color(rgb(0x232740))
            .child(self.zoom_header(cx, range))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                            this.on_pixels_mouse_down(ev);
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                        this.on_pixels_mouse_move(ev);
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
                    .child(
                        canvas(
                            move |bounds, _window, cx| {
                                entity.update(cx, |this, cx| this.measure_zoom(bounds, cx));
                            },
                            move |bounds, (), window, _cx| {
                                if let Some(d) = &data {
                                    panes::paint_zoom(
                                        window,
                                        bounds,
                                        d,
                                        bpr,
                                        first_row_start,
                                        block,
                                        hovered,
                                        sel.as_ref(),
                                        &entropies,
                                        entropy_window,
                                        colormap,
                                        (!mark.is_empty()).then_some(&mark),
                                    );
                                }
                            },
                        )
                        .size_full(),
                    )
                    .size_full(),
            )
    }

    /// The zoom column's header: title, the zoom readout / slider / reset, the
    /// visible byte range and this panel's colormap picker.
    fn zoom_header(&mut self, cx: &mut Context<Self>, range: Option<String>) -> impl IntoElement {
        let zoom_controls = div()
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
            }))
            .child(self.colormap_picker(cx, Panel::Zoom));
        column_header("Zoom", range, zoom_controls)
    }

    /// A panel's colormap control: a `Map: … ▾` toggle that expands into the
    /// four options. Only one panel's menu is open at a time, so the pills can
    /// live in the header row without stealing space when collapsed.
    fn colormap_picker(&mut self, cx: &mut Context<Self>, panel: Panel) -> impl IntoElement {
        let open = self.open_colormap_menu == Some(panel);
        let current = self.colormap(panel);
        let toggle = div()
            .id(("colormap-toggle", panel as usize))
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
            .on_click(
                cx.listener(move |this, _: &ClickEvent, _: &mut Window, cx| {
                    this.open_colormap_menu = if this.open_colormap_menu == Some(panel) {
                        None
                    } else {
                        Some(panel)
                    };
                    cx.notify();
                }),
            )
            .child(swatch(current))
            .child(div().child(format!("Map: {}", current.label())))
            .child(div().child("▾"));

        div()
            .flex()
            .items_center()
            .gap_1()
            // Any press inside the picker (toggle or option pill) counts as
            // "inside", so the root's outside-click handler leaves it alone.
            .on_any_mouse_down(
                cx.listener(move |this, _: &MouseDownEvent, _: &mut Window, _cx| {
                    this.colormap_click_inside = true;
                }),
            )
            .child(toggle)
            .when(open, |d| {
                d.children(Colormap::ALL.into_iter().enumerate().map(|(idx, cm)| {
                    let mut pill = div()
                        .id(("colormap", panel as usize * Colormap::ALL.len() + idx))
                        .px_1()
                        .py_1()
                        .rounded_md()
                        .text_size(px(11.))
                        .cursor_pointer()
                        .on_click(
                            cx.listener(move |this, _: &ClickEvent, _: &mut Window, cx| {
                                this.set_colormap(panel, cm);
                                this.open_colormap_menu = None;
                                cx.notify();
                            }),
                        );
                    pill = if cm == current {
                        pill.bg(rgb(0x7aa2f7)).text_color(rgb(0x0f1017))
                    } else {
                        pill.bg(rgb(0x24283b))
                            .text_color(rgb(0xc0caf5))
                            .hover(|s| s.bg(rgb(0x3b4261)))
                    };
                    pill.child(cm.label())
                }))
            })
    }

    /// Right column: colormap-backed hex + ASCII cells. Its row length comes
    /// from its own width and it is the scroll reference (SPECS §4.2).
    #[allow(clippy::too_many_lines)] // single-purpose element builder
    fn hex_column(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let data = self.mmap.clone();
        let bpr = self.hex_bpr.max(8);
        let len = self.file_size;
        // The anchor is the byte in the middle of the viewport, so panels align
        // on their centre line (SPECS §4.2); the prepaint recorded the row.
        let first_row_start = self.hex_view.start;
        let hovered = self.hovered_offset;
        let sel = self.selection_range.clone();
        let font = panes::mono_font(&self.mono_family);
        let hex_font = panes::mono_font(&self.mono_family);
        let hex_entropies = self.entropies.clone();
        let entropy_window = self.entropy_window;
        let hex_colormap = self.hex_colormap;

        let range = (len > 0).then(|| {
            let rows = panes::visible_rows(self.view_height, panes::BLOCK_H);
            panes::range_label(first_row_start, (first_row_start + rows * bpr).min(len))
        });

        // The hex text size is fixed, so this header carries no zoom controls.
        let header_extra = self.colormap_picker(cx, Panel::Hex);

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
                    .flex()
                    .flex_row()
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            // `on_any_mouse_down` covers left, middle and right (the
                            // handler dispatches on the button itself); adding a
                            // separate left binding would run it twice per click.
                            .on_any_mouse_down(cx.listener(
                                move |this, ev: &MouseDownEvent, window, cx| {
                                    this.on_hex_mouse_down(ev, window);
                                    if let Some(copy) = this.pending_copy.take() {
                                        cx.write_to_clipboard(ClipboardItem::new_string(copy));
                                    }
                                    cx.notify();
                                },
                            ))
                            .on_mouse_move(cx.listener(
                                move |this, ev: &MouseMoveEvent, window, cx| {
                                    this.on_hex_mouse_move(ev, window);
                                    cx.notify();
                                },
                            ))
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
                            .on_scroll_wheel(cx.listener(
                                move |this, ev: &ScrollWheelEvent, _, cx| {
                                    this.on_hex_scroll(ev, cx);
                                    cx.notify();
                                },
                            ))
                            .child(
                                canvas(
                                    move |bounds, window, cx| {
                                        let char_w = panes::hex_char_width(
                                            window,
                                            &hex_font,
                                            px(panes::HEX_FONT_SIZE),
                                        );
                                        entity.update(cx, |this, cx| {
                                            this.hex_bounds = bounds;
                                            this.view_height = bounds.size.height.to_f64() as f32;
                                            // Content fits the panel: as many whole
                                            // 8-byte groups as the width allows.
                                            let new_bpr = panes::hex_bytes_per_row(
                                                bounds.size.width.to_f64() as f32,
                                                char_w,
                                            );
                                            let bpr_changed = new_bpr != this.hex_bpr;
                                            this.hex_bpr = new_bpr;
                                            let bpr = this.hex_bpr.max(8);
                                            let before = this.scroll_offset;
                                            // The anchor *is* the centre, so a jump is just an assignment.
                                            if let Some(off) = this.scroll_to_offset.take() {
                                                this.scroll_offset = off;
                                            }
                                            this.clamp_anchor();
                                            let rows = panes::visible_rows(
                                                this.view_height,
                                                panes::BLOCK_H,
                                            );
                                            let first = panes::first_row_centred(
                                                this.scroll_offset,
                                                bpr,
                                                rows,
                                            );
                                            this.hex_view =
                                                first..(first + rows * bpr).min(this.file_size);
                                            if this.file_size > 0 {
                                                this.view_frac = (this.scroll_offset as f32
                                                    / this.file_size as f32)
                                                    .clamp(0.0, 1.0);
                                            }
                                            if this.scroll_offset != before || bpr_changed {
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
                                                bpr,
                                                first_row_start,
                                                hovered,
                                                sel.as_ref(),
                                                &hex_entropies,
                                                entropy_window,
                                                hex_colormap,
                                            );
                                        } else {
                                            window.paint_quad(quad_dark(bounds));
                                        }
                                    },
                                )
                                .size_full(),
                            )
                            .size_full(),
                    )
                    .child(self.hex_scrollbar(cx)),
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
                                let text = this.jump_field.read(cx).content().to_owned();
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
            SliderKind::PixelZoom => self.pixel_zoom,
            SliderKind::EntropyWindow => self.entropy_window as f32,
        };
        let (min, max) = match kind {
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
                    let thumb_x = slider_thumb_left(t, w);
                    let thumb = Bounds::new(
                        point(
                            bounds.left() + px(thumb_x),
                            bounds.top() + px(h * 0.5 - SLIDER_THUMB_W * 0.5),
                        ),
                        size(px(SLIDER_THUMB_W), px(SLIDER_THUMB_W)),
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
            SliderKind::PixelZoom => self.pixels_slider_bounds,
            SliderKind::EntropyWindow => self.entropy_slider_bounds,
        }
    }

    fn set_slider_bounds(&mut self, kind: SliderKind, bounds: Bounds<Pixels>) {
        match kind {
            SliderKind::PixelZoom => self.pixels_slider_bounds = bounds,
            SliderKind::EntropyWindow => self.entropy_slider_bounds = bounds,
        }
    }

    fn set_slider_value(&mut self, kind: SliderKind, v: f32, cx: &mut Context<Self>) {
        match kind {
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
        self.capture_window_geometry(window);
        // With client-side decorations nothing else draws a titlebar, so the
        // top bar has to provide move / maximize / close and the edges have to
        // provide resize. See `main.rs`'s `DECORATIONS`.
        let client_side = matches!(window.window_decorations(), Decorations::Client { .. });
        // Debounced config save.
        let current = self.current_config();
        if current != self.saved_cfg && self.last_save.elapsed() >= Duration::from_secs(2) {
            config::save(&current);
            self.saved_cfg = current;
            self.last_save = Instant::now();
        }

        self.refresh_strip();

        let show_jump = self.show_jump_dialog;
        let no_file = self.mmap.is_none();

        div()
            .id("parallhex-root")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x16161e))
            .text_color(rgb(0xc0caf5))
            .key_context("ParallHex")
            .track_focus(&self.focus_handle(cx))
            .on_action(cx.listener(Self::on_open_file))
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(Self::on_jump_to_offset))
            .on_action(cx.listener(Self::on_reset_view))
            .on_action(cx.listener(Self::on_reset_columns))
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
                    let inside = this.colormap_click_inside;
                    this.colormap_click_inside = false;
                    if this.open_colormap_menu.is_some() && !inside {
                        this.open_colormap_menu = None;
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
            .child(self.top_bar(cx, client_side))
            .child(self.central_area(cx, no_file))
            .child(self.status_bar(cx))
            .when(show_jump, |d| d.child(self.jump_dialog(cx)))
            // Last children so they sit above everything and win hit-testing
            // in the few pixels along each window edge.
            .when(client_side, |d| d.children(resize_handles(cx)))
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

/// Width of the invisible resize border along each window edge, used only with
/// client-side decorations (nothing else provides resize handles then).
const RESIZE_EDGE_W: f32 = 6.0;

/// Minimize / maximize / close, for when the app supplies its own titlebar.
/// Closing goes through the same save-then-quit path as the Quit action so
/// preferences are never lost to a click on the close button.
fn window_buttons(cx: &mut Context<ParallHexApp>) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(window_button(
            cx,
            "window-minimize",
            "–",
            false,
            |window| {
                window.minimize_window();
            },
        ))
        .child(window_button(
            cx,
            "window-maximize",
            "□",
            false,
            |window| {
                window.zoom_window();
            },
        ))
        .child(window_button(cx, "window-close", "✕", true, |_| {}))
}

/// One window-control button. `danger` tints the hover state red (close).
/// `on_window` runs against the window; the close button is special-cased by
/// `danger` so it can save preferences and quit through the view.
fn window_button(
    cx: &mut Context<ParallHexApp>,
    id: &'static str,
    glyph: &'static str,
    danger: bool,
    on_window: impl Fn(&mut Window) + 'static,
) -> impl IntoElement {
    let mut b = div()
        .id(id)
        .w(px(22.))
        .h(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .text_color(rgb(0xc0caf5))
        .cursor_pointer()
        .active(|s| s.opacity(0.7))
        .child(glyph);
    b = if danger {
        b.hover(|s| s.bg(rgb(0xf7768e)).text_color(rgb(0x16161e)))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                config::save(&this.current_config());
                cx.quit();
            }))
    } else {
        b.hover(|s| s.bg(rgb(0x3b4261)))
            .on_click(cx.listener(move |_this, _: &ClickEvent, window, _cx| on_window(window)))
    };
    b
}

/// The eight resize affordances (four edges, then four corners) as absolutely
/// positioned overlay children. Corners come last so they win where they
/// overlap an edge.
fn resize_handles(cx: &mut Context<ParallHexApp>) -> Vec<gpui::AnyElement> {
    [
        ResizeEdge::Top,
        ResizeEdge::Bottom,
        ResizeEdge::Left,
        ResizeEdge::Right,
        ResizeEdge::TopLeft,
        ResizeEdge::TopRight,
        ResizeEdge::BottomLeft,
        ResizeEdge::BottomRight,
    ]
    .into_iter()
    .map(|edge| resize_handle(cx, edge))
    .collect()
}

fn resize_handle(cx: &mut Context<ParallHexApp>, edge: ResizeEdge) -> gpui::AnyElement {
    let cursor = match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    };
    let w = px(RESIZE_EDGE_W);
    let base = div().absolute().cursor(cursor).on_mouse_down(
        MouseButton::Left,
        cx.listener(move |_this, _: &MouseDownEvent, window, _cx| {
            window.start_window_resize(edge);
        }),
    );
    match edge {
        ResizeEdge::Top => base.top(px(0.)).left(px(0.)).w_full().h(w),
        ResizeEdge::Bottom => base.bottom(px(0.)).left(px(0.)).w_full().h(w),
        ResizeEdge::Left => base.left(px(0.)).top(px(0.)).h_full().w(w),
        ResizeEdge::Right => base.right(px(0.)).top(px(0.)).h_full().w(w),
        ResizeEdge::TopLeft => base.top(px(0.)).left(px(0.)).w(w).h(w),
        ResizeEdge::TopRight => base.top(px(0.)).right(px(0.)).w(w).h(w),
        ResizeEdge::BottomLeft => base.bottom(px(0.)).left(px(0.)).w(w).h(w),
        ResizeEdge::BottomRight => base.bottom(px(0.)).right(px(0.)).w(w).h(w),
    }
    .into_any_element()
}

/// A small color swatch previewing what a colormap looks like, shown in the
/// pixels-column dropdown toggle.
fn swatch(cm: Colormap) -> impl IntoElement {
    let color = match cm {
        Colormap::None => rgb(0x3b4261),
        Colormap::Value => rgb(0x9aa5ce),
        Colormap::Class => color::class_color(0x41),
        Colormap::Entropy => color::entropy_color(4.0),
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

/// Slider thumb geometry. The thumb is a fixed-width knob whose left edge
/// travels between `SLIDER_PAD` and `w - SLIDER_PAD - SLIDER_THUMB_W`, so the
/// value corresponds to the thumb's *center*. Painting and hit-testing both go
/// through the two helpers below; computing either one inline is how the thumb
/// ends up not sitting under the pointer that set it.
const SLIDER_PAD: f32 = 2.0;
const SLIDER_THUMB_W: f32 = 12.0;

/// Horizontal distance the thumb's left edge can travel in a slider of width `w`.
fn slider_travel(w: f32) -> f32 {
    (w - 2.0 * SLIDER_PAD - SLIDER_THUMB_W).max(1.0)
}

/// The thumb's left edge for a normalized position `t`, relative to the
/// slider's left edge.
fn slider_thumb_left(t: f32, w: f32) -> f32 {
    SLIDER_PAD + t.clamp(0.0, 1.0) * slider_travel(w)
}

/// The normalized position a pointer at `x` (relative to the slider's left
/// edge) selects — the inverse of `slider_thumb_left` about the thumb center.
fn slider_t_at_x(x: f32, w: f32) -> f32 {
    ((x - SLIDER_PAD - SLIDER_THUMB_W * 0.5) / slider_travel(w)).clamp(0.0, 1.0)
}

/// Map a slider drag position (window coords) to a value, using the stored
/// bounds from the previous frame.
fn slider_value_at(kind: SliderKind, pos: Point<Pixels>, bounds: Bounds<Pixels>) -> Option<f32> {
    if bounds.size.width.to_f64() <= 0.0 {
        return None;
    }
    let w = bounds.size.width.to_f64() as f32;
    let t = slider_t_at_x((pos.x - bounds.left()).to_f64() as f32, w);
    let (min, max) = match kind {
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
    use super::{
        CopyKind, NavigationAction, OVERVIEW_W_MAX, OVERVIEW_W_MIN, ParallHexApp, SLIDER_THUMB_W,
        ZOOM_W_MAX, ZOOM_W_MIN, divider_width, selection_text, slider_t_at_x, slider_thumb_left,
    };

    /// `navigate` clamps a stale selection before delegating, so the tests
    /// exercise `nav_next` through the same clamp.
    fn next(
        action: NavigationAction,
        cur: usize,
        bpr: usize,
        page_bytes: usize,
        len: usize,
    ) -> usize {
        ParallHexApp::nav_next(action, cur.min(len - 1), bpr, page_bytes, len)
    }

    #[test]
    fn arrows_clamp_at_boundaries() {
        let len = 1000usize;
        let bpr = 32usize;
        assert_eq!(next(NavigationAction::Left, 0, bpr, 0, len), 0);
        assert_eq!(next(NavigationAction::Up, 0, bpr, 0, len), 0);
        assert_eq!(next(NavigationAction::Right, len - 1, bpr, 0, len), len - 1);
        assert_eq!(next(NavigationAction::Down, len - 1, bpr, 0, len), len - 1);
        assert_eq!(next(NavigationAction::Right, len - 2, bpr, 0, len), len - 1);
        assert_eq!(next(NavigationAction::Down, len - 32, bpr, 0, len), len - 1);
        assert_eq!(
            next(NavigationAction::Down, len - 64, bpr, 0, len),
            len - 64 + bpr
        );
    }

    #[test]
    fn page_keys_clamp_at_boundaries() {
        let len = 1000usize;
        let bpr = 32usize;
        let page_bytes = 448usize;
        assert_eq!(next(NavigationAction::PageUp, 10, bpr, page_bytes, len), 0);
        assert_eq!(
            next(NavigationAction::PageDown, len - 5, bpr, page_bytes, len),
            len - 1
        );
        assert_eq!(
            next(NavigationAction::PageDown, 100, bpr, page_bytes, len),
            100 + page_bytes
        );
        assert_eq!(
            next(NavigationAction::PageUp, 500, bpr, page_bytes, len),
            500 - page_bytes
        );
    }

    #[test]
    fn home_end_jump_to_boundaries() {
        let len = 1000usize;
        assert_eq!(next(NavigationAction::Home, 500, 32, 448, len), 0);
        assert_eq!(next(NavigationAction::End, 500, 32, 448, len), len - 1);
        assert_eq!(next(NavigationAction::End, 0, 32, 448, len), len - 1);
    }

    #[test]
    fn stale_selection_is_clamped_before_moving() {
        let len = 1000usize;
        assert_eq!(next(NavigationAction::Right, 5000, 32, 448, len), len - 1);
        assert_eq!(
            next(NavigationAction::PageDown, 5000, 32, 448, len),
            len - 1
        );
        assert_eq!(next(NavigationAction::Up, 5000, 32, 448, len), len - 1 - 32);
    }

    /// Only the zoom column zooms now, so the step is exercised over its range.
    #[test]
    fn zoom_step_multiplies_and_clamps() {
        use crate::panes::{PIXEL_ZOOM_MAX, PIXEL_ZOOM_MIN};
        assert_eq!(
            crate::panes::zoom_step(4.0, crate::panes::ZOOM_STEP, PIXEL_ZOOM_MIN, PIXEL_ZOOM_MAX),
            5.0
        );
        assert_eq!(
            crate::panes::zoom_step(
                24.0,
                crate::panes::ZOOM_STEP,
                PIXEL_ZOOM_MIN,
                PIXEL_ZOOM_MAX
            ),
            24.0
        );
        assert_eq!(
            crate::panes::zoom_step(1.0, 0.8, PIXEL_ZOOM_MIN, PIXEL_ZOOM_MAX),
            1.0
        );
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

    /// The "Reset columns" shortcut assigns the config defaults directly, so
    /// those defaults must always be widths the divider-drag clamp accepts —
    /// otherwise a reset could produce an out-of-range width.
    #[test]
    fn reset_column_defaults_are_within_drag_clamps() {
        let defaults = crate::config::Config::default();
        assert!(
            (OVERVIEW_W_MIN..=OVERVIEW_W_MAX).contains(&defaults.overview_width),
            "overview default must be within the drag clamp range"
        );
        assert!(
            (ZOOM_W_MIN..=ZOOM_W_MAX).contains(&defaults.zoom_width),
            "zoom column default must be within the drag clamp range"
        );
    }

    /// Clicking a slider must land the thumb under the pointer: mapping a
    /// position to a value and back to the thumb center has to round-trip.
    #[test]
    fn slider_thumb_sits_where_the_pointer_selected_it() {
        const W: f32 = 90.0;
        for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let center = slider_thumb_left(t, W) + SLIDER_THUMB_W * 0.5;
            let back = slider_t_at_x(center, W);
            assert!((back - t).abs() < 1e-4, "t={t} -> x={center} -> t={back}");
        }
        // The extremes saturate instead of running off the track.
        assert_eq!(slider_t_at_x(-50.0, W), 0.0);
        assert_eq!(slider_t_at_x(500.0, W), 1.0);
        // A degenerate width must not divide by zero.
        assert!(slider_t_at_x(5.0, 0.0).is_finite());
    }

    #[test]
    fn selection_text_formats_hex_and_ascii() {
        let data = b"Hi\x00\xff!";
        assert_eq!(
            selection_text(data, &(0..5), CopyKind::Hex).as_deref(),
            Some("48 69 00 FF 21")
        );
        assert_eq!(
            selection_text(data, &(0..5), CopyKind::Ascii).as_deref(),
            Some("Hi..!")
        );
        // Ranges are clamped to the file, and an empty result is `None`.
        assert_eq!(
            selection_text(data, &(3..900), CopyKind::Hex).as_deref(),
            Some("FF 21")
        );
        assert_eq!(selection_text(data, &(2..2), CopyKind::Hex), None);
        assert_eq!(selection_text(data, &(900..901), CopyKind::Hex), None);
        assert_eq!(selection_text(&[], &(0..4), CopyKind::Ascii), None);
    }
}
