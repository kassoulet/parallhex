//! Application state and the `eframe::App` shell.

use std::fs::File;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use memmap2::{Mmap, MmapOptions};

use crate::color;
use crate::config;
use crate::entropy;
use crate::panes;

/// Size of the horizontal whole-file preview strip in the top bar.
const STRIP_W: f32 = 360.0;
const STRIP_H: f32 = 40.0;

/// The navigation key pressed this frame (one-hot).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NavigationAction {
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

pub struct EntropyMapApp {
    pub file_path: Option<PathBuf>,
    pub mmap: Option<Arc<Mmap>>,
    pub file_size: usize,

    // Hex-viewer parameters
    pub bytes_per_row: usize,
    pub entropy_window: usize,

    // One-shot: jump the central scroll area back to the top.
    pub scroll_reset: bool,

    // One-shot: scroll the central view to a specific file offset.
    pub scroll_to_offset: Option<usize>,

    // Cached Shannon entropy per `entropy_window`-sized block (whole file).
    pub entropies: Vec<f32>,

    // Whole-file 2D overview (left panel): the file is downsampled into a
    // `w × h` grid of cells, each drawn as a greyscale band over an entropy
    // band (the pixels column's look). Regenerated at the panel size;
    // `overview_cells` are the cell dimensions of the current texture.
    pub overview_image: Option<egui::ColorImage>,
    pub overview_tex: Option<egui::TextureHandle>,
    pub overview_dirty: bool,
    pub overview_gen_size: Option<(f32, f32)>,
    pub overview_cells: Option<(usize, usize)>,
    // Horizontal whole-file preview strip (top bar, right): a fixed 256×2
    // greyscale / entropy thumbnail with x mapping to file offset.
    pub strip_image: Option<egui::ColorImage>,
    pub strip_tex: Option<egui::TextureHandle>,
    pub strip_dirty: bool,
    pub strip_rect: egui::Rect,

    // Visible fraction of the file in the central view, for the overview's
    // viewport indicator.
    pub view_frac: f32,
    pub view_frac_h: f32,
    pub view_height: f32,

    // Three-column layout: per-column zoom, the shared scroll position (in
    // rows; the hex column is the master) and each column's content rect.
    pub hex_zoom: f32,
    pub pixel_zoom: f32,
    pub scroll_rows: f32,
    pub hex_rect: egui::Rect,
    pub pixels_rect: egui::Rect,
    pub overview_rect: egui::Rect,

    // Selection & hover state (shared by all four panes).
    pub hovered_offset: Option<usize>,
    pub selected_offset: Option<usize>,
    pub selection_range: Option<Range<usize>>,
    pub drag_start: Option<usize>,

    // Offset under the pointer while hovering the overview previews (top-bar
    // strip / left overview; previewed in the top-bar readout; does not
    // touch the panes' hover/selection).
    pub overview_hover_offset: Option<usize>,

    // Jump-to-offset dialog (Ctrl+G).
    pub show_jump_dialog: bool,
    pub jump_input: String,
    // One-shot: request keyboard focus on the dialog's text field the first
    // frame it opens, so typing works immediately (Ctrl+G or toolbar button).
    pub jump_focus_requested: bool,

    // Persisted layout prefs: the resizable side-panel widths plus the
    // zooms / bytes-per-row, written to a config file. `saved_cfg` and
    // `last_save` debounce the write so an unchanged layout never rewrites
    // the file.
    pub overview_width: f32,
    pub pixels_width: f32,
    pub saved_cfg: config::Config,
    pub last_save: Instant,

    pub message: Option<String>,
}

impl EntropyMapApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, initial_file: Option<PathBuf>) -> Self {
        let prefs = config::load();
        let mut app = Self {
            file_path: None,
            mmap: None,
            file_size: 0,
            bytes_per_row: match prefs.bytes_per_row {
                16 | 32 | 64 => prefs.bytes_per_row,
                _ => 32,
            },
            entropy_window: 256,
            scroll_reset: false,
            scroll_to_offset: None,
            entropies: Vec::new(),
            overview_image: None,
            overview_tex: None,
            overview_dirty: false,
            overview_gen_size: None,
            overview_cells: None,
            strip_image: None,
            strip_tex: None,
            strip_dirty: false,
            strip_rect: egui::Rect::NOTHING,
            view_frac: 0.0,
            view_frac_h: 1.0,
            view_height: 600.0,
            // Values from the config file are clamped to the same ranges
            // the wheel / keyboard zoom handlers use.
            hex_zoom: prefs
                .hex_zoom
                .clamp(panes::HEX_ZOOM_MIN, panes::HEX_ZOOM_MAX),
            pixel_zoom: prefs
                .pixel_zoom
                .clamp(panes::PIXEL_ZOOM_MIN, panes::PIXEL_ZOOM_MAX),
            scroll_rows: 0.0,
            hex_rect: egui::Rect::NOTHING,
            pixels_rect: egui::Rect::NOTHING,
            overview_rect: egui::Rect::NOTHING,
            hovered_offset: None,
            selected_offset: None,
            selection_range: None,
            drag_start: None,
            overview_hover_offset: None,
            show_jump_dialog: false,
            jump_input: String::new(),
            jump_focus_requested: false,
            // Panel widths, guarded against absurd hand-edited values.
            overview_width: prefs.overview_width.clamp(140.0, 2000.0),
            pixels_width: prefs.pixels_width.clamp(200.0, 3000.0),
            saved_cfg: prefs,
            last_save: Instant::now(),
            message: None,
        };
        if let Some(path) = initial_file {
            app.load_file(path);
        }
        app
    }

    pub(crate) fn data(&self) -> Option<&[u8]> {
        self.mmap.as_ref().map(|m| &m[..])
    }

    /// Entropy (bits/byte) at `offset`: linearly interpolates between the two
    /// `entropy_window`-sized blocks overlapping the offset.
    pub fn entropy_at(&self, offset: usize) -> f32 {
        let w = self.entropy_window.max(1);
        if self.entropies.is_empty() {
            return 0.0;
        }
        let block = (offset / w).min(self.entropies.len() - 1);
        let h0 = self.entropies[block];
        let Some(&h1) = self.entropies.get(block + 1) else {
            return h0;
        };
        let t = (offset % w) as f32 / w as f32;
        h0 + (h1 - h0) * t
    }

    fn recompute_entropies(&mut self) {
        self.entropies = match self.data() {
            Some(d) => entropy::block_entropies(d, self.entropy_window),
            None => Vec::new(),
        };
    }

    /// Build the 2D whole-file overview (left panel): the file is
    /// downsampled into a `w × h` cell grid sized to `size`, each cell
    /// drawn as a greyscale band over an entropy band (the same two-band
    /// look as the pixels column).
    fn generate_overview(&mut self, size: (f32, f32)) {
        let Some(data) = self.data() else {
            self.overview_image = None;
            self.overview_cells = None;
            self.overview_dirty = true;
            return;
        };
        let len = data.len();
        if len == 0 {
            self.overview_image = None;
            self.overview_cells = None;
            self.overview_dirty = true;
            return;
        }
        // Cap the texture resolution so very large windows stay cheap.
        let w = (size.0.max(1.0) as usize).clamp(64, 512);
        let h = (size.1.max(1.0) as usize).clamp(32, 1024);
        let cells = w * h;
        // Two image rows per cell: greyscale on top, entropy below.
        let mut pixels = vec![egui::Color32::from_gray(8); w * 2 * h];
        for k in 0..cells {
            let start = k * len / cells;
            let end = ((k + 1) * len / cells).max(start + 1);
            let idx = 2 * (k % w + (k / w) * w);
            pixels[idx] = egui::Color32::from_gray(Self::sample_average(data, start, end));
            let mid = (start + (end - start) / 2).min(len - 1);
            pixels[idx + 1] = color::entropy_color(self.entropy_at(mid));
        }
        self.overview_image = Some(egui::ColorImage {
            size: [w, 2 * h],
            pixels,
        });
        self.overview_cells = Some((w, h));
        self.overview_dirty = true;
    }

    /// Build the horizontal whole-file preview strip (top bar, right): a
    /// 2-row greyscale / entropy thumbnail, x mapping to file offset.
    fn generate_strip(&mut self) {
        let Some(data) = self.data() else {
            self.strip_image = None;
            self.strip_dirty = true;
            return;
        };
        let len = data.len();
        if len == 0 {
            self.strip_image = None;
            self.strip_dirty = true;
            return;
        }
        const W: usize = 256;
        let mut pixels = vec![egui::Color32::from_gray(8); W * 2];
        for x in 0..W {
            let start = x * len / W;
            let end = ((x + 1) * len / W).max(start + 1);
            pixels[x] = egui::Color32::from_gray(Self::sample_average(data, start, end));
            let mid = (start + (end - start) / 2).min(len - 1);
            pixels[W + x] = color::entropy_color(self.entropy_at(mid));
        }
        self.strip_image = Some(egui::ColorImage {
            size: [W, 2],
            pixels,
        });
        self.strip_dirty = true;
    }

    /// Average byte value over `[start, end)`, sampled at a few points (a
    /// thumbnail cell can cover many bytes). Shared by the 2D overview and
    /// the horizontal strip generators.
    fn sample_average(data: &[u8], start: usize, end: usize) -> u8 {
        const SAMPLES: usize = 8;
        let mut sum = 0u32;
        for k in 0..SAMPLES {
            let off = (start + (end - start) * k / SAMPLES).min(data.len() - 1);
            sum += data[off] as u32;
        }
        (sum / SAMPLES as u32) as u8
    }

    /// Move the selection with the keyboard (arrows / PageUp / PageDown /
    /// Home / End) and scroll the hex viewer to keep it centered.
    fn keyboard_navigate(&mut self, ctx: &egui::Context) {
        // Don't steal keys from a focused widget (e.g. the entropy slider).
        if ctx.wants_keyboard_input() {
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
        // Page size: (view height / zoomed hex row height) rows, times bpr.
        let page_rows = (self.view_height / panes::hex_row_h(self.hex_zoom)).max(1.0) as usize;
        let page_bytes = page_rows * bpr;

        // `key_down` repeats while held, so holding an arrow key keeps moving.
        let input = ctx.input(|i| i.clone());
        let left = input.key_down(egui::Key::ArrowLeft);
        let right = input.key_down(egui::Key::ArrowRight);
        let up = input.key_down(egui::Key::ArrowUp);
        let down = input.key_down(egui::Key::ArrowDown);
        let pg_up = input.key_down(egui::Key::PageUp);
        let pg_down = input.key_down(egui::Key::PageDown);
        let home = input.key_down(egui::Key::Home);
        let end = input.key_down(egui::Key::End);
        if !(left || right || up || down || pg_up || pg_down || home || end) {
            return;
        }

        // First navigation press with no selection yet: honor Home/End,
        // otherwise place the cursor at offset 0.
        let Some(cur) = self.selected_offset else {
            let start = if end { len - 1 } else { 0 };
            self.selected_offset = Some(start);
            self.hovered_offset = Some(start);
            self.scroll_to_offset = Some(start);
            return;
        };

        let next = Self::nav_next(
            NavigationAction::new(left, right, up, down, pg_up, pg_down, home),
            cur.min(len - 1),
            bpr,
            page_bytes,
            len,
        );
        self.selected_offset = Some(next);
        self.hovered_offset = Some(next);
        self.scroll_to_offset = Some(next);
    }

    /// `+` / `=` and `-` zoom the column under the pointer (hex row height
    /// or pixel size), clamped to its range. The overview has no zoom, so
    /// nothing happens while the pointer isn't over a zoomable column; the
    /// column headers' Reset zoom buttons restore the defaults.
    fn keyboard_zoom(&mut self, ctx: &egui::Context) {
        // Don't steal keys from a focused widget (e.g. the jump dialog's
        // text field) or zoom behind an open dialog.
        if ctx.wants_keyboard_input() || self.show_jump_dialog {
            return;
        }
        if self.mmap.is_none() {
            return;
        }
        let (plus, minus) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals),
                i.key_pressed(egui::Key::Minus),
            )
        });
        if !plus && !minus {
            return;
        }
        let factor = if plus { panes::ZOOM_STEP } else { 1.0 / panes::ZOOM_STEP };
        // The focused column is the one under the pointer. The stored rects
        // come from the previous frame, which is fine for hit-testing.
        let Some(p) = ctx.input(|i| i.pointer.interact_pos()) else {
            return;
        };
        if self.hex_rect.contains(p) {
            self.hex_zoom = panes::zoom_step(
                self.hex_zoom,
                factor,
                panes::HEX_ZOOM_MIN,
                panes::HEX_ZOOM_MAX,
            );
        } else if self.pixels_rect.contains(p) {
            self.pixel_zoom = panes::zoom_step(
                self.pixel_zoom,
                factor,
                panes::PIXEL_ZOOM_MIN,
                panes::PIXEL_ZOOM_MAX,
            );
        }
    }

    fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_file() {
            self.load_file(path);
        }
    }

    /// Pure navigation math used by the keyboard handler: compute the offset
    /// reached by `action` from `cur`, clamped to `[0, len)`.
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
        // The 2D overview regenerates lazily at the panel's current size;
        // the horizontal top-bar strip is fixed-resolution.
        self.overview_gen_size = None;
        self.overview_image = None;
        self.overview_tex = None;
        self.overview_dirty = true;
        self.generate_strip();
        self.scroll_reset = true;
        self.scroll_rows = 0.0;
        self.scroll_to_offset = None;
        self.hovered_offset = None;
        self.selected_offset = None;
        self.selection_range = None;
        self.drag_start = None;
        self.overview_hover_offset = None;
        self.show_jump_dialog = false;
        self.jump_input.clear();
        self.jump_focus_requested = false;
        self.message = None;
    }

    fn top_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.heading("EntropyMap");
            ui.separator();

            // File info (moved from the old side panel).
            let fname = self
                .file_path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "<no file>".to_owned());
            ui.label(format!(
                "{fname} · {} bytes ({})",
                self.file_size,
                color::human_size(self.file_size)
            ));
            ui.separator();

            // Hovered / selected byte info (moved from the old side panel).
            let off = self
                .overview_hover_offset
                .or(self.hovered_offset)
                .or(self.selected_offset);
            if let Some(off) = off {
                if let Some(d) = self.data() {
                    if off < d.len() {
                        let b = d[off];
                        let h = self.entropy_at(off);
                        ui.label(format!(
                            "0x{off:08X} · 0x{b:02X} '{}' · H={h:.3}",
                            color::printable(b)
                        ));
                    }
                }
            }

            // Jump-dialog live preview while typing.
            if self.show_jump_dialog {
                ui.separator();
                match Self::parse_offset(&self.jump_input) {
                    Some(o) if o < self.file_size => {
                        if let Some(d) = self.data() {
                            let b = d[o];
                            let h = self.entropy_at(o);
                            ui.colored_label(
                                egui::Color32::from_gray(180),
                                format!(
                                    "Jump: 0x{o:08X}  Byte: 0x{b:02X} '{}'  H={h:.3}",
                                    color::printable(b)
                                ),
                            );
                        }
                    }
                    Some(o) => {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            format!(
                                "Out of range: 0x{o:X} (file is 0x{:X} bytes).",
                                self.file_size
                            ),
                        );
                    }
                    None => {
                        ui.colored_label(egui::Color32::YELLOW, "Jump: invalid offset");
                    }
                }
            }
            ui.separator();
            if let Some(msg) = self.message.clone() {
                ui.colored_label(egui::Color32::YELLOW, msg);
            }
        });

        ui.horizontal_wrapped(|ui| {
            if ui.button("Open File…").clicked() {
                self.open_dialog();
            }
            ui.separator();

            let prev = self.bytes_per_row;
            egui::ComboBox::from_label("Bytes/Row")
                .selected_text(format!("{}", self.bytes_per_row))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.bytes_per_row, 16, "16");
                    ui.selectable_value(&mut self.bytes_per_row, 32, "32");
                    ui.selectable_value(&mut self.bytes_per_row, 64, "64");
                });
            if prev != self.bytes_per_row {
                self.scroll_reset = true;
            }

            ui.separator();
            if ui
                .add(
                    egui::Slider::new(&mut self.entropy_window, 16..=4096)
                        .logarithmic(true)
                        .text("Entropy window"),
                )
                .changed()
            {
                self.recompute_entropies();
                // The 2D overview regenerates on the next frame with the
                // new entropy values; the strip regenerates now.
                self.overview_gen_size = None;
                self.generate_strip();
            }

            ui.separator();
            if ui.button("Reset view").clicked() {
                self.scroll_reset = true;
            }
            ui.separator();
            if ui
                .add_enabled(self.mmap.is_some(), egui::Button::new("Jump to offset… (Ctrl+G)"))
                .clicked()
            {
                self.open_jump_dialog();
            }
            ui.separator();
            ui.label("Ctrl+wheel or +/- zoom (pointer over column) · drag pan/select (hex: middle or Ctrl/Alt+drag) · right-click copy");
        });

        // Right side of the top panel: the horizontal whole-file preview.
        if self.mmap.is_some() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.overview_strip(ui);
            });
        }
    }

    /// Left column: a vertical whole-file overview in the same style as the
    /// pixels column — each downsampled cell is a greyscale band over an
    /// entropy band, with the entire file fitted into the panel. A
    /// translucent region marks the currently visible range; hover previews
    /// the offset in the top bar; click / drag jumps to the offset.
    fn overview_column(&mut self, ui: &mut egui::Ui) {
        let range = (self.file_size > 0).then(|| panes::range_label(0, self.file_size));
        panes::column_header(ui, "Overview", range, |_| {});
        ui.label("Whole file · greyscale / entropy");
        if self.mmap.is_none() {
            ui.label("(no data)");
            return;
        }
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), ui.available_height()),
            egui::Sense::click_and_drag(),
        );
        self.overview_rect = rect;

        // Regenerate the overview when the panel resizes so the aspect
        // stays correct. Compare the quantized cell counts (what the texture
        // is actually built at), so sub-pixel width/height changes during a
        // window resize don't rebuild it every frame.
        let target = (
            (rect.width().max(1.0) as usize).clamp(64, 512) as f32,
            (rect.height().max(1.0) as usize).clamp(32, 1024) as f32,
        );
        if self.overview_gen_size != Some(target) {
            self.generate_overview(target);
            self.overview_gen_size = Some(target);
        }
        let Some(tex) = self.overview_tex.clone() else {
            return;
        };
        let painter = ui.painter();
        painter.rect_filled(rect, 2.0, egui::Color32::from_gray(10));
        painter.image(
            tex.id(),
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );

        // Viewport region: translucent overlay over the visible byte range.
        let Some((w, h)) = self.overview_cells else {
            return;
        };
        let cells = w * h;
        if cells > 0 && self.file_size > 0 {
            let len = self.file_size;
            let start_off = (self.view_frac.clamp(0.0, 1.0) * len as f32) as usize;
            let end_off =
                ((self.view_frac + self.view_frac_h).clamp(0.0, 1.0) * len as f32) as usize;
            let k0 = (start_off.saturating_mul(cells) / len).min(cells - 1);
            let k1 = (end_off.saturating_mul(cells) / len).min(cells - 1);
            let (i0, j0) = (k0 % w, k0 / w);
            let (i1, j1) = (k1 % w, k1 / w);
            let x0 = rect.min.x + i0 as f32 / w as f32 * rect.width();
            let y0 = rect.min.y + j0 as f32 / h as f32 * rect.height();
            let x1 = rect.min.x + (i1 as f32 + 1.0) / w as f32 * rect.width();
            let y1 = rect.min.y + (j1 as f32 + 1.0) / h as f32 * rect.height();
            let band = egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1));
            painter.rect_filled(
                band,
                0.0,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 40),
            );
            painter.rect_stroke(
                band,
                0.0,
                egui::Stroke::new(1.0_f32, egui::Color32::from_gray(140)),
            );
        }

        // Hover: preview the file offset under the cursor in the top bar.
        if let Some(p) = resp.hover_pos() {
            if let Some(off) = Self::overview_offset_at(p, rect, w, h, self.file_size) {
                self.overview_hover_offset = Some(off);
            }
        }

        // Click / drag navigation: jump to the offset and select it so the
        // top bar and hex view update immediately.
        if resp.clicked() || resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                if let Some(off) = Self::overview_offset_at(p, rect, w, h, self.file_size) {
                    self.scroll_to_offset = Some(off);
                    self.selected_offset = Some(off);
                    self.hovered_offset = Some(off);
                }
            }
        }
        ui.label("Click / drag to navigate");
    }

    /// Map a pointer position in the 2D overview to a file offset. The
    /// texture is `w` cells wide and `2h` image rows tall (two rows per
    /// cell: greyscale + entropy), so cell row `j = image_row / 2`.
    fn overview_offset_at(
        p: egui::Pos2,
        rect: egui::Rect,
        w: usize,
        h: usize,
        len: usize,
    ) -> Option<usize> {
        if len == 0 || w == 0 || h == 0 {
            return None;
        }
        let i = (((p.x - rect.min.x) / rect.width().max(1.0)) * w as f32)
            .clamp(0.0, w as f32 - 1.0) as usize;
        let r = (((p.y - rect.min.y) / rect.height().max(1.0)) * (2 * h) as f32)
            .clamp(0.0, (2 * h) as f32 - 1.0) as usize;
        let j = r / 2;
        let k = (j * w + i).min(w * h - 1);
        Some((k * len / (w * h)).min(len - 1))
    }

    /// Horizontal whole-file preview strip (greyscale / entropy) shown at
    /// the right of the top panel; the translucent band marks the visible
    /// range, hovering previews the offset, click / drag navigates.
    fn overview_strip(&mut self, ui: &mut egui::Ui) {
        let Some(tex) = self.strip_tex.clone() else {
            return;
        };
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(STRIP_W, STRIP_H),
            egui::Sense::click_and_drag(),
        );
        self.strip_rect = rect;
        let painter = ui.painter();
        painter.rect_filled(rect, 2.0, egui::Color32::from_gray(10));
        painter.image(
            tex.id(),
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        // Viewport band (x maps to file offset).
        let x0 = rect.min.x + self.view_frac.clamp(0.0, 1.0) * rect.width();
        let x1 = rect.min.x
            + (self.view_frac + self.view_frac_h).clamp(0.0, 1.0) * rect.width();
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x0, rect.min.y), egui::pos2(x1, rect.max.y)),
            0.0,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 40),
        );
        painter.rect_stroke(rect, 2.0, egui::Stroke::new(1.0_f32, egui::Color32::from_gray(80)));

        // Hover: preview the file offset under the cursor in the top bar.
        if let Some(p) = resp.hover_pos() {
            let t = ((p.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
            self.overview_hover_offset = Some(
                ((t * self.file_size as f32) as usize).min(self.file_size.saturating_sub(1)),
            );
        }

        // Click / drag navigation: jump to the offset and select it.
        if resp.clicked() || resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                let t = ((p.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
                let off =
                    ((t * self.file_size as f32) as usize).min(self.file_size.saturating_sub(1));
                self.scroll_to_offset = Some(off);
                self.selected_offset = Some(off);
                self.hovered_offset = Some(off);
            }
        }
    }

    /// Parse a user-supplied offset as hex: `0x` prefix optional, underscores
    /// and whitespace allowed (e.g. `"0x1_000"`, `"1F"`).
    fn parse_offset(input: &str) -> Option<usize> {
        let s = input.trim().replace('_', "");
        if s.is_empty() {
            return None;
        }
        let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(&s);
        usize::from_str_radix(hex, 16).ok()
    }

    /// Open the jump-to-offset dialog, prefilled with the current selection.
    fn open_jump_dialog(&mut self) {
        if self.show_jump_dialog {
            return; // already open: don't wipe the user's typing
        }
        let cur = self.selected_offset.unwrap_or(0);
        self.jump_input = format!("0x{cur:X}");
        self.show_jump_dialog = true;
        self.jump_focus_requested = true;
    }

    /// Ctrl+G jump dialog: type a hex offset and press Enter.
    fn jump_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_jump_dialog {
            return;
        }
        let file_size = self.file_size;
        let mut jumped: Option<usize> = None;
        let mut err: Option<String> = None;

        let resp = egui::Window::new("Jump to Offset")
            .id(egui::Id::new("jump_dialog"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!(
                    "Offset (hex, 0x… up to 0x{:X}):",
                    file_size.saturating_sub(1)
                ));
                ui.horizontal(|ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.jump_input)
                            .hint_text("0x1000")
                            .desired_width(180.0)
                            .font(egui::TextStyle::Monospace),
                    );
                    // Focus the field the first frame the dialog is open.
                    if self.jump_focus_requested {
                        resp.request_focus();
                        self.jump_focus_requested = false;
                    }
                    let submit = resp.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Jump").clicked() || submit {
                        err = Self::parse_offset(&self.jump_input).map_or_else(
                            || Some("Invalid offset.".to_owned()),
                            |o| {
                                if o >= file_size {
                                    Some(format!(
                                        "Offset 0x{o:X} is out of range (file is 0x{:X} bytes).",
                                        file_size
                                    ))
                                } else {
                                    jumped = Some(o);
                                    None
                                }
                            },
                        );
                    }
                });
                if let Some(e) = &err {
                    ui.colored_label(egui::Color32::YELLOW, e);
                }
            });

        // User closed the window (X button / Escape): dismiss the dialog.
        if resp.is_none() {
            self.show_jump_dialog = false;
            return;
        }
        // Apply the jump outside the closure (avoids borrow issues).
        if let Some(o) = jumped {
            self.scroll_to_offset = Some(o);
            self.selected_offset = Some(o);
            self.hovered_offset = Some(o);
            self.show_jump_dialog = false;
        }
    }

    fn central_panel(&mut self, ui: &mut egui::Ui) {
        if self.mmap.is_none() {
            ui.centered_and_justified(|ui| {
                ui.label("No file loaded.\n\nOpen a binary file to explore its bytes.");
            });
            return;
        }
        panes::show_hex(ui, self);
    }

    /// Snapshot the current layout prefs for saving (widths rounded to
    /// whole pixels so an unchanged layout doesn't rewrite the file).
    fn current_config(&self) -> config::Config {
        config::Config {
            bytes_per_row: self.bytes_per_row,
            hex_zoom: self.hex_zoom,
            pixel_zoom: self.pixel_zoom,
            overview_width: self.overview_width.round(),
            pixels_width: self.pixels_width.round(),
        }
    }
}

impl eframe::App for EntropyMapApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.key_pressed(egui::Key::O) && i.modifiers.command) {
            self.open_dialog();
        }
        if self.mmap.is_some()
            && ctx.input(|i| i.key_pressed(egui::Key::G) && i.modifiers.command)
        {
            self.open_jump_dialog();
        }
        self.jump_dialog(ctx);
        // While the jump dialog is open, navigation keys belong to it; don't
        // move the file selection behind it (e.g. when the field isn't focused).
        if !self.show_jump_dialog {
            self.keyboard_navigate(ctx);
        }
        self.keyboard_zoom(ctx);
        if self.overview_dirty {
            self.overview_dirty = false;
            self.overview_tex = self.overview_image.clone().map(|img| {
                ctx.load_texture("overview", img, egui::TextureOptions::NEAREST)
            });
        }
        if self.strip_dirty {
            self.strip_dirty = false;
            self.strip_tex = self.strip_image.clone().map(|img| {
                ctx.load_texture("strip", img, egui::TextureOptions::NEAREST)
            });
        }
        egui::TopBottomPanel::top("top").show(ctx, |ui| self.top_panel(ui));
        // Capture the resizable panel widths each frame so they can be
        // persisted. `default_width` restores the saved width on launch.
        let overview_resp = egui::SidePanel::left("overview")
            .resizable(true)
            .default_width(self.overview_width)
            .min_width(140.0)
            .show(ctx, |ui| self.overview_column(ui));
        self.overview_width = overview_resp.response.rect.width();
        let pixels_resp = egui::SidePanel::left("pixels")
            .resizable(true)
            .default_width(self.pixels_width)
            .min_width(200.0)
            .show(ctx, |ui| panes::show_pixels(ui, self));
        self.pixels_width = pixels_resp.response.rect.width();
        egui::CentralPanel::default().show(ctx, |ui| self.central_panel(ui));

        // Persist the layout a couple of seconds after the last change.
        let current = self.current_config();
        if current != self.saved_cfg && self.last_save.elapsed() >= Duration::from_secs(2) {
            config::save(&current);
            self.saved_cfg = current;
            self.last_save = Instant::now();
        }

        // Clear the shared hover once the pointer leaves every column, and
        // the overview preview once it leaves the strip / 2D overview.
        let hover_pos = ctx.pointer_hover_pos();
        let in_columns = hover_pos.is_some_and(|p| {
            self.strip_rect.contains(p)
                || self.overview_rect.contains(p)
                || self.pixels_rect.contains(p)
                || self.hex_rect.contains(p)
        });
        if !in_columns {
            self.hovered_offset = None;
        }
        let over_overview = hover_pos.is_some_and(|p| {
            self.strip_rect.contains(p) || self.overview_rect.contains(p)
        });
        if !over_overview {
            self.overview_hover_offset = None;
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Flush any layout change made within the debounce window.
        config::save(&self.current_config());
    }
}

#[cfg(test)]
mod tests {
    use super::{EntropyMapApp, NavigationAction};

    /// The navigation key pressed this frame (one-hot).
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

    /// Mirror keyboard_navigate's call path: the current offset is clamped to
    /// the file before the move is applied.
    fn next(key: NavKey, cur: usize, bpr: usize, page_bytes: usize, len: usize) -> usize {
        EntropyMapApp::nav_next(action(key), cur.min(len - 1), bpr, page_bytes, len)
    }

    #[test]
    fn page_size_matches_view_geometry() {
        // The page size used by keyboard_navigate: (view_height / hex_row_h)
        // rows, rounded up, times bytes_per_row.
        let bpr = 32usize;
        let page_bytes = ((600.0 / crate::panes::hex_row_h(1.0)).max(1.0) as usize) * bpr;
        // Sanity: PageDown from 0 lands exactly one page in.
        assert_eq!(next(NavKey::PageDown, 0, bpr, page_bytes, 1 << 20), page_bytes);
        // And PageUp from there lands back on 0.
        assert_eq!(next(NavKey::PageUp, page_bytes, bpr, page_bytes, 1 << 20), 0);
    }

    #[test]
    fn arrows_clamp_at_boundaries() {
        let len = 1000usize;
        let bpr = 32usize;
        // At the start: Left and Up clamp to 0.
        assert_eq!(next(NavKey::Left, 0, bpr, 0, len), 0);
        assert_eq!(next(NavKey::Up, 0, bpr, 0, len), 0);
        // At the end: Right and Down clamp to len - 1.
        assert_eq!(next(NavKey::Right, len - 1, bpr, 0, len), len - 1);
        assert_eq!(next(NavKey::Down, len - 1, bpr, 0, len), len - 1);
        // Right clamps when the next byte would overshoot the end.
        assert_eq!(next(NavKey::Right, len - 2, bpr, 0, len), len - 1);
        // Down from the last row clamps to the last byte (960 + 32 = 992,
        // which is still in range, so use the true last row 968 + 32 = 1000).
        assert_eq!(next(NavKey::Down, len - 32, bpr, 0, len), len - 1);
        // Down from one row above the last row lands inside the file.
        assert_eq!(next(NavKey::Down, len - 64, bpr, 0, len), len - 64 + bpr);
    }

    #[test]
    fn page_keys_clamp_at_boundaries() {
        let len = 1000usize;
        let bpr = 32usize;
        let page_bytes = 448usize; // 14 visible rows of 32 bytes
        // PageUp at the top clamps to 0.
        assert_eq!(next(NavKey::PageUp, 10, bpr, page_bytes, len), 0);
        // PageDown at the bottom clamps to len - 1.
        assert_eq!(next(NavKey::PageDown, len - 5, bpr, page_bytes, len), len - 1);
        // PageDown from mid-file moves exactly one page.
        assert_eq!(next(NavKey::PageDown, 100, bpr, page_bytes, len), 100 + page_bytes);
        // PageUp from mid-file moves exactly one page back.
        assert_eq!(next(NavKey::PageUp, 500, bpr, page_bytes, len), 500 - page_bytes);
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
        // keyboard_navigate clamps `cur` to len - 1 before applying the move.
        let len = 1000usize;
        // A stale cursor beyond EOF: Right can't go anywhere, stays at end.
        assert_eq!(next(NavKey::Right, 5000, 32, 448, len), len - 1);
        // PageDown from a stale position also clamps to the end.
        assert_eq!(next(NavKey::PageDown, 5000, 32, 448, len), len - 1);
        // Up from a stale position moves back exactly one row from the end.
        assert_eq!(next(NavKey::Up, 5000, 32, 448, len), len - 1 - 32);
    }

    #[test]
    fn zoom_step_multiplies_and_clamps() {
        // The +/- keyboard zoom applies a multiplicative step, clamped to
        // the column's range (shared helper for the hex and pixels columns).
        assert_eq!(crate::panes::zoom_step(1.0, crate::panes::ZOOM_STEP, 0.5, 4.0), 1.25);
        assert_eq!(crate::panes::zoom_step(4.0, crate::panes::ZOOM_STEP, 0.5, 4.0), 4.0);
        assert_eq!(crate::panes::zoom_step(0.5, 0.8, 0.5, 4.0), 0.5);
        // Pixels range is 1..=24: 20 × 1.25 clamps to 24.
        assert_eq!(
            crate::panes::zoom_step(20.0, crate::panes::ZOOM_STEP, 1.0, 24.0),
            24.0
        );
    }

    #[test]
    fn parse_hex_with_prefix() {
        assert_eq!(EntropyMapApp::parse_offset("0x1F"), Some(31));
        assert_eq!(EntropyMapApp::parse_offset("0X1000"), Some(4096));
    }

    #[test]
    fn parse_hex_without_prefix() {
        assert_eq!(EntropyMapApp::parse_offset("1F"), Some(31));
        assert_eq!(EntropyMapApp::parse_offset("DEADBEEF"), Some(0xDEAD_BEEF));
    }

    #[test]
    fn parse_allows_underscores_and_whitespace() {
        assert_eq!(EntropyMapApp::parse_offset(" 0x1_000 "), Some(4096));
    }

    #[test]
    fn parse_invalid_returns_none() {
        assert_eq!(EntropyMapApp::parse_offset(""), None);
        assert_eq!(EntropyMapApp::parse_offset("xyz"), None);
        assert_eq!(EntropyMapApp::parse_offset("0x"), None);
        assert_eq!(EntropyMapApp::parse_offset("-5"), None);
    }
}
