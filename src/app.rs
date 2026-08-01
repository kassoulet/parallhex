//! Application state and the `eframe::App` shell.

use std::fs::File;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;
use memmap2::{Mmap, MmapOptions};

use crate::color;
use crate::entropy;
use crate::panes;

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

    // Whole-file overview thumbnail (greyscale + entropy) for the side panel.
    pub overview_image: Option<egui::ColorImage>,
    pub overview_tex: Option<egui::TextureHandle>,
    pub overview_dirty: bool,

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

    // Offset under the pointer while hovering the overview map (previewed
    // in the status bar; does not touch the panes' hover/selection).
    pub overview_hover_offset: Option<usize>,

    // Jump-to-offset dialog (Ctrl+G).
    pub show_jump_dialog: bool,
    pub jump_input: String,
    // One-shot: request keyboard focus on the dialog's text field the first
    // frame it opens, so typing works immediately (Ctrl+G or toolbar button).
    pub jump_focus_requested: bool,

    pub message: Option<String>,
}

impl EntropyMapApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, initial_file: Option<PathBuf>) -> Self {
        let mut app = Self {
            file_path: None,
            mmap: None,
            file_size: 0,
            bytes_per_row: 32,
            entropy_window: 256,
            scroll_reset: false,
            scroll_to_offset: None,
            entropies: Vec::new(),
            overview_image: None,
            overview_tex: None,
            overview_dirty: false,
            view_frac: 0.0,
            view_frac_h: 1.0,
            view_height: 600.0,
            hex_zoom: 1.0,
            pixel_zoom: 4.0,
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

    /// Build a 2-row whole-file thumbnail: greyscale (top) and entropy
    /// (bottom). Each column covers `len / width` bytes.
    fn generate_overview(&mut self) {
        let Some(data) = self.data() else {
            self.overview_image = None;
            self.overview_dirty = true;
            return;
        };
        let len = data.len();
        if len == 0 {
            self.overview_image = None;
            self.overview_dirty = true;
            return;
        }
        const W: usize = 256;
        const SAMPLES: usize = 8;
        let mut pixels = vec![egui::Color32::from_gray(8); W * 2];
        for x in 0..W {
            let start = x * len / W;
            let end = ((x + 1) * len / W).max(start + 1);
            let mut sum = 0u32;
            for k in 0..SAMPLES {
                let off = (start + (end - start) * k / SAMPLES).min(len - 1);
                sum += data[off] as u32;
            }
            pixels[x] = egui::Color32::from_gray((sum / SAMPLES as u32) as u8);
            let mid = (start + (end - start) / 2).min(len - 1);
            pixels[W + x] = color::entropy_color(self.entropy_at(mid));
        }
        self.overview_image = Some(egui::ColorImage {
            size: [W, 2],
            pixels,
        });
        self.overview_dirty = true;
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
        self.generate_overview();
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
                self.generate_overview();
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
            ui.label(format!(
                "Zoom: hex ×{:.2} · px {} · drag pan, Ctrl+wheel zoom",
                self.hex_zoom, self.pixel_zoom
            ));
        });
    }

    /// Left column: whole-file thumbnail (greyscale / entropy) with a
    /// viewport band; click or drag to navigate, hover previews the offset
    /// in the top bar.
    fn overview_column(&mut self, ui: &mut egui::Ui) {
        ui.strong("Overview");
        ui.label("Whole file · greyscale / entropy");
        let Some(tex) = self.overview_tex.clone() else {
            ui.label("(no data)");
            return;
        };
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 56.0),
            egui::Sense::click_and_drag(),
        );
        self.overview_rect = rect;
        let painter = ui.painter();
        painter.rect_filled(rect, 2.0, egui::Color32::from_gray(10));
        painter.image(
            tex.id(),
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        // Viewport indicator (x maps to file offset).
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
        self.overview_hover_offset = match resp.hover_pos() {
            Some(p) => {
                let t = ((p.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
                let off = (t * self.file_size as f32) as usize;
                Some(off.min(self.file_size.saturating_sub(1)))
            }
            None => None,
        };

        // Click / drag navigation: jump to the offset and select it so the
        // top bar and hex view update immediately.
        if resp.clicked() || resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                let t = ((p.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
                let off = (t * self.file_size as f32) as usize;
                let off = off.min(self.file_size.saturating_sub(1));
                self.scroll_to_offset = Some(off);
                self.selected_offset = Some(off);
                self.hovered_offset = Some(off);
            }
        }
        ui.label("Click / drag to navigate");
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
        if self.overview_dirty {
            self.overview_dirty = false;
            self.overview_tex = self.overview_image.clone().map(|img| {
                ctx.load_texture("overview", img, egui::TextureOptions::NEAREST)
            });
        }
        egui::TopBottomPanel::top("top").show(ctx, |ui| self.top_panel(ui));
        egui::SidePanel::left("overview")
            .resizable(true)
            .default_width(200.0)
            .min_width(140.0)
            .show(ctx, |ui| self.overview_column(ui));
        egui::SidePanel::left("pixels")
            .resizable(true)
            .default_width(320.0)
            .min_width(200.0)
            .show(ctx, |ui| panes::show_pixels(ui, self));
        egui::CentralPanel::default().show(ctx, |ui| self.central_panel(ui));

        // Clear the shared hover once the pointer leaves every column.
        let hover_pos = ctx.pointer_hover_pos();
        let in_columns = hover_pos.is_some_and(|p| {
            self.overview_rect.contains(p)
                || self.pixels_rect.contains(p)
                || self.hex_rect.contains(p)
        });
        if !in_columns {
            self.hovered_offset = None;
        }
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
