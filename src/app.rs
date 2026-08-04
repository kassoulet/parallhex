//! Application state and the gpui view shell.

use std::fs::File;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, ClickEvent, ClipboardItem, Context, CursorStyle, Decorations, Entity, FocusHandle,
    Focusable, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, Render, RenderImage, ResizeEdge, ScrollDelta, ScrollWheelEvent, SharedString,
    WeakEntity, Window, div, prelude::*, px, rgb,
};

use gpui::AsyncApp;

use memmap2::{Mmap, MmapOptions};

use crate::core::color::{self, Colormap};
use crate::core::config;
use crate::core::entropy;
use crate::core::geom;
use crate::gui::paint;
use crate::gui::{
    ClearSelection, CopySelectionAscii, CopySelectionHex, JumpCancel, JumpSubmit, JumpToOffset,
    NavigateDown, NavigateEnd, NavigateHome, NavigateLeft, NavigatePageDown, NavigatePageUp,
    NavigateRight, NavigateUp, OpenFile, Quit, ResetColumns, ResetSettings, ResetView, ZoomIn,
    ZoomOut,
};
use crate::jump::{JumpField, JumpFieldEvent};

// The view-construction methods (columns, bars, dialog, sliders) and their
// shared chrome helpers live in `ui`; this module keeps the state, the
// handlers, the async work and the `Render` shell.
pub(crate) mod ui;

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

impl SliderKind {
    /// The `(min, max)` this slider spans on its log scale. Both the thumb
    /// position (`ui::slider`) and the pointer→value mapping (`slider_value_at`)
    /// read it here, so they cannot disagree about where a value sits.
    fn range(self) -> (f32, f32) {
        match self {
            SliderKind::PixelZoom => (geom::PIXEL_ZOOM_MIN, geom::PIXEL_ZOOM_MAX),
            SliderKind::EntropyWindow => (
                geom::ENTROPY_WINDOW_MIN as f32,
                geom::ENTROPY_WINDOW_MAX as f32,
            ),
        }
    }
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

/// The inputs the whole-file overview thumbnail is a function of; the cached
/// image is rebuilt only when this changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OverviewKey {
    w: usize,
    h: usize,
    colormap: Colormap,
    entropy_window: usize,
}

/// The inputs the zoom column's visible-region texture is a function of; the
/// cached image is rebuilt only when this changes, so scrolling and zooming
/// re-upload one texture instead of repainting a quad per visible byte.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ZoomImageKey {
    bpr: usize,
    first_row_start: usize,
    iw: usize,
    ih: usize,
    colormap: Colormap,
    entropy_window: usize,
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

    // Each panel colors its bytes independently.
    pub overview_colormap: Colormap,
    pub zoom_colormap: Colormap,
    pub hex_colormap: Colormap,

    // One-shot: scroll the central view to a specific file offset.
    pub scroll_to_offset: Option<usize>,

    // Cached Shannon entropy per `entropy_window`-sized block (whole file).
    // Arc so canvas paint closures can grab a cheap snapshot.
    pub entropies: Arc<Vec<f32>>,

    // Monospace glyph width for the hex column, measured in `render` only when
    // the window scale changes, and reused by the canvas prepaint, the paint
    // closure and the hit-testing — previously reshaped 64 glyphs on every frame
    // *and* on every mouse move.
    pub hex_char_w: f32,
    pub hex_char_w_scale: f32,

    // The zoom column renders its visible bytes as one texture: `zoom_image` is
    // rebuilt in the canvas prepaint only when its key changes, so
    // scrolling/zooming re-uploads a texture instead of emitting a quad per byte.
    pub zoom_image: Option<Arc<RenderImage>>,
    pub zoom_image_key: Option<ZoomImageKey>,
    // A zoom-texture build in flight (coalesces rebuilds; the landing commits
    // the key it built, and the next prepaint's key mismatch re-requests).
    pub zoom_computing: bool,

    // Entropy computation generation: a background recompute applies only if no
    // newer one was started meanwhile.
    pub entropy_gen: u64,
    // Coalescing for the async entropy pass: while `entropy_computing` is set,
    // new requests just mark `entropy_pending` and the in-flight task re-runs
    // with the latest window when it lands. Without this, dragging the
    // entropy-window slider would queue a whole-file compute per tick.
    pub entropy_computing: bool,
    pub entropy_pending: bool,

    // Whole-file 2D overview (left panel) and horizontal preview strip,
    // generated as gpui RenderImages from downsampled byte data.
    pub overview_image: Option<Arc<RenderImage>>,
    pub overview_cells: Option<(usize, usize)>,
    // The inputs the overview thumbnail was built for; a mismatch (or `None`)
    // means a rebuild is owed. The build runs on the background executor
    // (`overview_computing` coalesces requests), so load/resize/colormap
    // changes never stall the UI thread.
    pub overview_key: Option<OverviewKey>,
    pub overview_computing: bool,
    pub strip_image: Option<Arc<RenderImage>>,
    pub strip_dirty: bool,

    // Three-column layout. The zoom column is the only one that zooms; the
    // shared scroll position is a *byte anchor* because each panel derives its
    // own row length from its own width, so rows no longer line up.
    // `hex_bpr` / `zoom_bpr` are recomputed in each canvas's prepaint
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
    // column is.
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

    // Wall-clock time spent building the previous frame's element tree,
    // shown in the status bar as a rough per-frame speed readout.
    pub last_render_ms: f32,

    // Wall-clock time from the end of the previous `render` until gpui's next
    // frame callback fires — i.e. the full frame cycle: the rest of the CPU
    // paint, the GPU submit/present and the wait for the next refresh. Kept in
    // sync via `Window::on_next_frame` (gpui 0.2 exposes no post-present hook).
    pub last_frame_ms: f32,

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
    // Window-space bounds of each header's colormap toggle, recorded by a
    // transparent prepaint canvas inside the picker (gpui 0.2 has no div
    // bounds callback). The floating option menu anchors to these so it can
    // overflow the narrow fixed-width columns.
    pub colormap_anchors: [Bounds<Pixels>; 3],

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
            entropy_window: prefs
                .entropy_window
                .clamp(geom::ENTROPY_WINDOW_MIN, geom::ENTROPY_WINDOW_MAX),
            overview_colormap: prefs.overview_colormap,
            zoom_colormap: prefs.zoom_colormap,
            hex_colormap: prefs.hex_colormap,
            scroll_to_offset: None,
            entropies: Arc::new(Vec::new()),
            hex_char_w: 8.0,
            hex_char_w_scale: 0.0,
            zoom_image: None,
            zoom_image_key: None,
            zoom_computing: false,
            entropy_gen: 0,
            entropy_computing: false,
            entropy_pending: false,
            overview_image: None,
            overview_cells: None,
            overview_key: None,
            overview_computing: false,
            strip_image: None,
            strip_dirty: false,
            pixel_zoom: prefs
                .pixel_zoom
                .clamp(geom::PIXEL_ZOOM_MIN, geom::PIXEL_ZOOM_MAX),
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
            last_render_ms: 0.0,
            last_frame_ms: 0.0,
            resizing_divider: None,
            divider_start_x: 0.0,
            divider_start_w: 0.0,
            pixels_slider_bounds: Bounds::default(),
            entropy_slider_bounds: Bounds::default(),
            dragging_slider: None,
            open_colormap_menu: None,
            colormap_click_inside: false,
            colormap_anchors: [Bounds::default(); 3],
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
            app.load_file(path, cx);
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
                self.overview_key = None;
                self.strip_dirty = true;
            }
            Panel::Zoom => self.zoom_colormap = cm,
            Panel::Hex => self.hex_colormap = cm,
        }
    }

    /// Window-space bounds of a panel's colormap toggle, recorded by the
    /// picker's prepaint canvas each frame (indexed in declaration order).
    fn colormap_anchor(&self, panel: Panel) -> Bounds<Pixels> {
        self.colormap_anchors[panel as usize]
    }

    fn set_colormap_anchor(&mut self, panel: Panel, bounds: Bounds<Pixels>) {
        self.colormap_anchors[panel as usize] = bounds;
    }

    /// Recompute the zoom column's layout from its measured canvas: how many
    /// bytes fit per row at the target block size, and which byte range that
    /// makes visible (the overview draws this as its band). Also rebuilds the
    /// visible-region texture when its inputs changed.
    fn measure_zoom(&mut self, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        self.pixels_bounds = bounds;
        // Redistribute the bytes so a row spans the panel exactly: as many
        // target-sized blocks as fit, then widened to fill the width.
        let w = bounds.size.width.to_f64() as f32;
        let bpr = geom::zoom_bytes_per_row(w, self.zoom_target());
        let changed = bpr != self.zoom_bpr;
        self.zoom_bpr = bpr;
        let block = geom::zoom_block_w(w, bpr);
        let rows = geom::visible_rows(bounds.size.height.to_f64() as f32, block);
        let first = geom::first_row_centred(self.scroll_offset, bpr, rows);
        self.zoom_view = first..(first + rows * bpr).min(self.file_size);
        // Rebuild the visible-region texture only when its inputs changed, on
        // the background executor so scrolling at low zoom never stalls the
        // frame. While a build is in flight the key mismatch is left alone; if
        // the view scrolled during the build, the landing's stale key triggers
        // another build. Divider drags skip the rebuild (the old texture
        // scales until the drag ends).
        let iw = ((bpr as f32 * block).ceil() as usize).max(1);
        let ih = ((rows as f32 * block).ceil() as usize).max(1);
        let key = ZoomImageKey {
            bpr,
            first_row_start: first,
            iw,
            ih,
            colormap: self.zoom_colormap,
            entropy_window: self.entropy_window,
        };
        if self.zoom_image_key != Some(key)
            && !self.zoom_computing
            && self.resizing_divider.is_none()
        {
            self.zoom_computing = true;
            let data = self.mmap.clone();
            let entropies = self.entropies.clone();
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let img = cx
                    .background_executor()
                    .spawn(async move {
                        data.as_deref().and_then(|d| {
                            let src = geom::ByteSource {
                                data: d,
                                entropies: &entropies,
                                entropy_window: key.entropy_window,
                                colormap: key.colormap,
                            };
                            paint::build_zoom_image(&src, key.bpr, key.first_row_start, rows, block)
                        })
                    })
                    .await;
                this.update(cx, |this, cx| {
                    this.zoom_image = img;
                    this.zoom_image_key = Some(key);
                    this.zoom_computing = false;
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        if changed {
            // The paint closure captured the old row length; ask for a frame
            // with the new one.
            cx.notify();
        }
    }

    /// The zoom column's *target* block size, as set by the slider.
    fn zoom_target(&self) -> f32 {
        self.pixel_zoom
            .clamp(geom::PIXEL_ZOOM_MIN, geom::PIXEL_ZOOM_MAX)
    }

    /// Actual size of one byte's block. The bytes are redistributed across the
    /// panel so a row spans its full width, so this is the target widened to
    /// divide the width exactly. Blocks are square, so it is the row height too.
    fn zoom_row_h(&self) -> f32 {
        geom::zoom_block_w(
            self.pixels_bounds.size.width.to_f64() as f32,
            self.zoom_bpr.max(1),
        )
    }

    /// Clamp the shared anchor to the hex column's last row — the hex column is
    /// the scroll reference for all three panels.
    fn clamp_anchor(&mut self) {
        self.scroll_offset = self
            .scroll_offset
            .min(geom::max_anchor(self.file_size, self.hex_bpr.max(8)));
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

    /// Recompute the whole-file entropy cache off the UI thread:
    /// `block_entropies` can take a second on multi-gigabyte files, which
    /// previously froze the window. The result is applied only if no newer
    /// recompute was started meanwhile (the `entropy_gen` guard), so changing
    /// the window mid-compute can't race an older result in. `show_message`
    /// flashes a "computing entropy…" status (used on load, not on slider
    /// drags).
    fn recompute_entropies_async(&mut self, cx: &mut Context<Self>, show_message: bool) {
        let Some(d) = self.mmap.clone() else {
            self.entropies = Arc::new(Vec::new());
            self.entropy_computing = false;
            return;
        };
        if show_message {
            self.message = Some("Computing entropy…".to_owned());
        }
        if self.entropy_computing {
            // A whole-file pass is already in flight; coalesce so slider drags
            // don't queue one pass per tick. The in-flight task re-runs with
            // the latest window when it lands.
            self.entropy_pending = true;
            return;
        }
        self.entropy_computing = true;
        self.entropy_gen += 1;
        let generation = self.entropy_gen;
        let window_size = self.entropy_window;
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let ents = cx
                .background_executor()
                .spawn(async move { entropy::block_entropies(&d, window_size) })
                .await;
            this.update(cx, |this, cx| {
                if this.entropy_gen == generation {
                    this.entropies = Arc::new(ents);
                    this.entropy_computing = false;
                    // The thumbnails and the zoom texture bake entropy in, so
                    // invalidate all three; their keys change only via these
                    // resets, not via the data itself.
                    this.overview_key = None;
                    this.strip_dirty = true;
                    this.zoom_image_key = None;
                    // Clear the load status. `show_message` covers the normal
                    // case; the value check also clears it when a newer
                    // request (e.g. an entropy-window slider drag during a
                    // load) coalesced onto this task.
                    if show_message || this.message.as_deref() == Some("Computing entropy…") {
                        this.message = None;
                    }
                    // Re-run with the latest window if requests arrived while
                    // the pass was in flight.
                    if this.entropy_pending {
                        this.entropy_pending = false;
                        this.recompute_entropies_async(cx, false);
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Rebuild the fixed-resolution top-bar strip when the file, entropy window
    /// or its colormap changed.
    fn refresh_strip(&mut self) {
        if !self.strip_dirty {
            return;
        }
        self.strip_dirty = false;
        let entropies = self.entropies.clone();
        let entropy_window = self.entropy_window;
        let colormap = self.overview_colormap;
        self.strip_image = self.data().map(|d| {
            paint::build_strip_image(&geom::ByteSource {
                data: d,
                entropies: &entropies,
                entropy_window,
                colormap,
            })
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
        self.recompute_entropies_async(cx, false);
        self.overview_key = None;
        self.strip_dirty = true;
        config::save(&defaults);
        self.saved_cfg = defaults;
        self.last_save = Instant::now();
        self.message = Some("Settings reset to defaults.".to_owned());
        cx.notify();
    }

    fn load_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
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
        // Clear the previous file's status *before* the async pass sets its
        // "computing entropy…" message, or the two lines cancel out.
        self.message = None;
        // Drop any in-flight pass from the previous file: bump the generation
        // so its landing is ignored, and let the new file's pass start now.
        self.entropy_gen += 1;
        self.entropy_computing = false;
        self.entropy_pending = false;
        // Entropy is computed off the UI thread; the hex/zoom panes render
        // with the empty cache (flat colors) and refresh when it lands.
        self.recompute_entropies_async(cx, true);
        self.overview_key = None;
        self.overview_image = None;
        self.overview_cells = None;
        self.strip_image = None;
        self.strip_dirty = true;
        // The zoom texture's cached key never changes across files, so force a
        // rebuild of the new file's bytes.
        self.zoom_image = None;
        self.zoom_image_key = None;
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
    }

    fn open_dialog(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = rfd::FileDialog::new().pick_file() {
            self.load_file(path, cx);
        }
    }

    // ----- keyboard actions -----

    fn on_open_file(&mut self, _: &OpenFile, _: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog(cx);
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
        self.zoom_under_pointer(window, paint::ZOOM_STEP);
        cx.notify();
    }

    fn on_zoom_out(&mut self, _: &ZoomOut, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_jump_dialog {
            return;
        }
        self.zoom_under_pointer(window, 1.0 / paint::ZOOM_STEP);
        cx.notify();
    }

    /// `+`/`-` zoom the column under the pointer (hex row height or pixel
    /// size), clamped to its range.
    fn zoom_under_pointer(&mut self, window: &Window, factor: f32) {
        let p = window.mouse_position();
        // Only the zoom column zooms; the hex text size is fixed.
        if self.pixels_bounds.contains(&p) {
            self.pixel_zoom = paint::zoom_step(
                self.pixel_zoom,
                factor,
                geom::PIXEL_ZOOM_MIN,
                geom::PIXEL_ZOOM_MAX,
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
        let page_bytes = geom::visible_rows(self.view_height, paint::BLOCK_H) * bpr;

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

    fn hex_offset_at_pos(&self, pos: Point<Pixels>) -> Option<usize> {
        let local = self.hex_bounds.localize(&pos)?;
        let bpr = self.hex_bpr.max(8);
        let geo = geom::RowGeo::new(paint::ADDR_X, self.hex_char_w, bpr);
        geom::hex_offset_at(
            local.x.to_f64() as f32,
            local.y.to_f64() as f32,
            &geo,
            paint::BLOCK_H,
            self.hex_view.start,
            self.file_size,
        )
    }

    fn on_hex_mouse_down(&mut self, event: &MouseDownEvent) {
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
                    .hex_offset_at_pos(event.position)
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
        if let Some(off) = self.hex_offset_at_pos(event.position) {
            self.drag_start = Some(off);
            self.selection_range = None;
            self.selected_offset = Some(off);
        }
    }

    fn on_hex_mouse_move(&mut self, event: &MouseMoveEvent) {
        let off = self.hex_offset_at_pos(event.position);
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
        self.scroll_by_wheel(Panel::Hex, &event.delta, paint::BLOCK_H);
        cx.notify();
    }

    // ----- mouse handlers (pixels column) -----

    fn pixels_offset_at(&self, pos: Point<Pixels>) -> Option<usize> {
        let local = self.pixels_bounds.localize(&pos)?;
        let bpr = self.zoom_bpr.max(1);
        geom::zoom_offset_at(
            local.x.to_f64() as f32,
            local.y.to_f64() as f32,
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
                self.pixel_zoom = paint::zoom_step(
                    self.pixel_zoom,
                    factor,
                    geom::PIXEL_ZOOM_MIN,
                    geom::PIXEL_ZOOM_MAX,
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
        let last = geom::max_anchor(self.file_size, self.hex_bpr.max(8));
        self.scroll_offset = geom::scrollbar_anchor_at(y, track_h, last, visible, self.file_size);
        self.clamp_anchor();
    }

    /// Begin a column-divider drag: remember which divider and the width it
    /// started at, so `on_divider_mouse_move` can apply the pointer delta.
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
}

impl Focusable for ParallHexApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ParallHexApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame_start = Instant::now();
        self.capture_window_geometry(window);
        // The hex glyph width is a function of the font and the window scale,
        // so measure it only when the scale changes and share it with the hex
        // column's prepaint, paint and hit-testing.
        // Previously it was reshaped on every canvas paint *and* on every
        // mouse move.
        let scale = window.scale_factor();
        if self.hex_char_w_scale != scale {
            let mono = paint::mono_font(&self.mono_family);
            self.hex_char_w = paint::hex_char_width(window, &mono, px(paint::HEX_FONT_SIZE));
            self.hex_char_w_scale = scale;
        }
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
        let open_colormap = self.open_colormap_menu;
        let no_file = self.mmap.is_none();

        let el = div()
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
            .when(open_colormap.is_some(), |d| d.child(self.colormap_menu(cx)))
            // Last children so they sit above everything and win hit-testing
            // in the few pixels along each window edge.
            .when(client_side, |d| d.children(resize_handles(cx)));

        // Measure the tree build itself (element construction, not the GPU
        // paint) so the status bar can report a rough per-frame cost.
        let live_render_ms = frame_start.elapsed().as_secs_f32() * 1000.0;
        let live_frame_ms = move || frame_start.elapsed().as_secs_f32() * 1000.0;

        // Timing the GPU paint itself isn't possible from outside gpui 0.2 (no
        // post-present hook), so capture the full frame cycle instead: gpui
        // runs this closure at the next frame's outset, i.e. after the current
        // frame has been painted and presented. The readout therefore includes
        // the rest of the CPU paint, the GPU submit and the vsync wait.
        let entity = cx.entity();
        window.on_next_frame(move |_, cx| {
            entity.update(cx, |this, _| this.last_frame_ms = live_frame_ms());
        });

        self.last_render_ms = live_render_ms;
        el
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

/// Width of the invisible resize strip along each window edge and corner.
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

/// Inset of a slider's track and the width of its thumb; the thumb travels
/// between the pads, so both the paint and the hit-testing derive from these.
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
    let (min, max) = kind.range();
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

/// The monospace family to render hex cells in: the first preferred face the
/// system actually has, then any family whose name contains "mono", then the
/// default UI font as a last resort (whose glyphs are proportional, so
/// `RowGeo`'s measured `char_w` keeps the cells aligned regardless).
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
        use crate::core::geom::{PIXEL_ZOOM_MAX, PIXEL_ZOOM_MIN};
        assert_eq!(
            crate::gui::paint::zoom_step(
                4.0,
                crate::gui::paint::ZOOM_STEP,
                PIXEL_ZOOM_MIN,
                PIXEL_ZOOM_MAX
            ),
            5.0
        );
        assert_eq!(
            crate::gui::paint::zoom_step(
                24.0,
                crate::gui::paint::ZOOM_STEP,
                PIXEL_ZOOM_MIN,
                PIXEL_ZOOM_MAX
            ),
            24.0
        );
        assert_eq!(
            crate::gui::paint::zoom_step(1.0, 0.8, PIXEL_ZOOM_MIN, PIXEL_ZOOM_MAX),
            1.0
        );
        assert_eq!(
            crate::gui::paint::zoom_step(20.0, crate::gui::paint::ZOOM_STEP, 1.0, 24.0),
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
        let defaults = crate::core::config::Config::default();
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
