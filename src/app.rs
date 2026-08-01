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

    // Selection & hover state (shared by all four panes).
    pub hovered_offset: Option<usize>,
    pub selected_offset: Option<usize>,
    pub selection_range: Option<Range<usize>>,
    pub drag_start: Option<usize>,

    // Offset under the pointer while hovering the overview map (previewed
    // in the status bar; does not touch the panes' hover/selection).
    pub overview_hover_offset: Option<usize>,

    pub message: Option<String>,
}

impl EntropyMapApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
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
            hovered_offset: None,
            selected_offset: None,
            selection_range: None,
            drag_start: None,
            overview_hover_offset: None,
            message: None,
        }
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
        let page_rows = (self.view_height / panes::BLOCK_H).max(1.0) as usize;
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
        let cur = cur.min(len - 1);
        let next = if left {
            cur.saturating_sub(1)
        } else if right {
            (cur + 1).min(len - 1)
        } else if up {
            cur.saturating_sub(bpr)
        } else if down {
            (cur + bpr).min(len - 1)
        } else if pg_up {
            cur.saturating_sub(page_bytes)
        } else if pg_down {
            (cur + page_bytes).min(len - 1)
        } else if home {
            0
        } else {
            len - 1
        };

        self.selected_offset = Some(next);
        self.hovered_offset = Some(next);
        self.scroll_to_offset = Some(next);
    }

    fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_file() {
            self.load_file(path);
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
        self.scroll_to_offset = None;
        self.hovered_offset = None;
        self.selected_offset = None;
        self.selection_range = None;
        self.drag_start = None;
        self.overview_hover_offset = None;
        self.message = None;
    }

    fn top_panel(&mut self, ui: &mut egui::Ui) {
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
        });
    }

    fn bottom_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // Preview the offset under the cursor in the overview map first.
            if let Some(off) = self.overview_hover_offset {
                if let Some(d) = self.data() {
                    if off < d.len() {
                        let b = d[off];
                        let h = self.entropy_at(off);
                        ui.colored_label(
                            egui::Color32::from_gray(180),
                            format!(
                                "Preview: 0x{off:08X}  Byte: 0x{b:02X} '{}'  H={h:.3}",
                                color::printable(b)
                            ),
                        );
                    } else {
                        ui.label("Offset: —");
                    }
                }
            } else {
                // Show the hovered byte, or the selected byte when not
                // hovering the content.
                let off = self.hovered_offset.or(self.selected_offset);
                if let Some(off) = off {
                    if let Some(d) = self.data() {
                        if off < d.len() {
                            let b = d[off];
                            let h = self.entropy_at(off);
                            ui.label(format!(
                                "Offset: 0x{off:08X}  Byte: 0x{b:02X} '{}'  H={h:.3}",
                                color::printable(b)
                            ));
                        } else {
                            ui.label("Offset: —");
                        }
                    }
                } else {
                    ui.label("Offset: —");
                }
            }
            ui.separator();
            ui.label(format!(
                "Size: {} ({})",
                self.file_size,
                color::human_size(self.file_size)
            ));
            ui.separator();
            let rows = self.file_size.div_ceil(self.bytes_per_row.max(1));
            ui.label(format!("Rows: {rows}  Bytes/Row: {}", self.bytes_per_row));
            if let Some(msg) = &self.message {
                ui.separator();
                ui.colored_label(egui::Color32::YELLOW, msg);
            }
        });
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("EntropyMap");
        ui.label("Wide hex-viewer binary explorer");
        ui.separator();

        if self.mmap.is_none() {
            ui.label("No file loaded.\n\nClick “Open File…” or press Ctrl/Cmd+O to open a binary file.");
            return;
        }

        let fname = self
            .file_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "<unknown>".to_owned());
        ui.label(format!("File: {fname}"));
        ui.label(format!(
            "Size: {} bytes ({})",
            self.file_size,
            color::human_size(self.file_size)
        ));
        ui.separator();

        self.overview_section(ui);
        ui.separator();

        ui.strong("Inspector");
        self.byte_info(ui, "Hover:", self.hovered_offset);
        self.byte_info(ui, "Selected:", self.selected_offset);
        ui.separator();

        self.selection_section(ui);
    }

    /// Whole-file thumbnail (greyscale / entropy). Click or drag to jump the
    /// central view to that offset; a translucent band marks the visible range.
    fn overview_section(&mut self, ui: &mut egui::Ui) {
        ui.strong("Overview");
        ui.label("Greyscale · Entropy — click to navigate");
        let Some(tex) = self.overview_tex.clone() else {
            ui.label("(no data)");
            return;
        };
        let h = 56.0;
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), h),
            egui::Sense::click_and_drag(),
        );
        let painter = ui.painter();
        painter.rect_filled(rect, 2.0, egui::Color32::from_gray(10));
        painter.image(
            tex.id(),
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        // Viewport indicator.
        let y0 = rect.min.y + self.view_frac.clamp(0.0, 1.0) * rect.height();
        let y1 = rect.min.y
            + (self.view_frac + self.view_frac_h).clamp(0.0, 1.0) * rect.height();
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(rect.min.x, y0), egui::pos2(rect.max.x, y1)),
            0.0,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 40),
        );
        painter.rect_stroke(rect, 2.0, egui::Stroke::new(1.0_f32, egui::Color32::from_gray(80)));

        // Hover: preview the file offset under the cursor in the status bar.
        self.overview_hover_offset = match resp.hover_pos() {
            Some(p) => {
                let t = ((p.y - rect.min.y) / rect.height()).clamp(0.0, 1.0);
                let off = (t * self.file_size as f32) as usize;
                Some(off.min(self.file_size.saturating_sub(1)))
            }
            None => None,
        };

        // Click / drag navigation: jump to the offset and select it so the
        // status bar and inspector update immediately.
        if resp.clicked() || resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                let t = ((p.y - rect.min.y) / rect.height()).clamp(0.0, 1.0);
                let off = (t * self.file_size as f32) as usize;
                let off = off.min(self.file_size.saturating_sub(1));
                self.scroll_to_offset = Some(off);
                self.selected_offset = Some(off);
                self.hovered_offset = Some(off);
            }
        }
    }

    fn byte_info(&mut self, ui: &mut egui::Ui, label: &str, off: Option<usize>) {
        ui.horizontal(|ui| {
            ui.label(label);
            if let Some(o) = off {
                if let Some(d) = self.data() {
                    if o < d.len() {
                        let b = d[o];
                        let h = self.entropy_at(o);
                        ui.label(format!(
                            "0x{o:08X} · 0x{b:02X} '{}' · H={h:.3}",
                            color::printable(b)
                        ));
                        return;
                    }
                }
            }
            ui.label("—");
        });
    }

    fn selection_section(&mut self, ui: &mut egui::Ui) {
        ui.strong("Selection");
        if let Some(r) = self.selection_range.clone() {
            let start = r.start;
            let end = r.end.min(self.file_size);
            let len = end.saturating_sub(start);
            ui.label(format!(
                "Range: 0x{start:08X} – 0x{:08X}",
                end.saturating_sub(1)
            ));
            ui.label(format!("Length: {len} bytes ({})", color::human_size(len)));
            if start < end && start < self.file_size {
                let mut action: Option<&'static str> = None;
                ui.horizontal(|ui| {
                    if ui.button("Copy Hex").clicked() {
                        action = Some("hex");
                    }
                    if ui.button("Copy ASCII").clicked() {
                        action = Some("ascii");
                    }
                    if ui.button("Clear").clicked() {
                        action = Some("clear");
                    }
                });
                match action {
                    Some("clear") => self.selection_range = None,
                    Some(kind) => {
                        if let Some(d) = self.data() {
                            let end = end.min(d.len());
                            if start < end {
                                if kind == "hex" {
                                    let s: String = d[start..end]
                                        .iter()
                                        .map(|b| format!("{b:02X}"))
                                        .collect::<Vec<_>>()
                                        .join(" ");
                                    ui.ctx().copy_text(s);
                                } else {
                                    let s: String = d[start..end]
                                        .iter()
                                        .map(|&b| color::printable(b))
                                        .collect();
                                    ui.ctx().copy_text(s);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        } else {
            ui.label("Drag on the map to select a range.");
            ui.label("Click to pick a single byte.");
        }
    }

    fn central_panel(&mut self, ui: &mut egui::Ui) {
        if self.mmap.is_none() {
            ui.centered_and_justified(|ui| {
                ui.label("No file loaded.\n\nOpen a binary file to explore its bytes.");
            });
            return;
        }
        panes::show_central(ui, self);
    }
}

impl eframe::App for EntropyMapApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.key_pressed(egui::Key::O) && i.modifiers.command) {
            self.open_dialog();
        }
        self.keyboard_navigate(ctx);
        if self.overview_dirty {
            self.overview_dirty = false;
            self.overview_tex = self.overview_image.clone().map(|img| {
                ctx.load_texture("overview", img, egui::TextureOptions::NEAREST)
            });
        }
        egui::TopBottomPanel::top("top").show(ctx, |ui| self.top_panel(ui));
        // Side panel before bottom panel: the status bar reads the overview
        // hover offset computed this frame, so the preview has no lag.
        egui::SidePanel::right("side")
            .resizable(true)
            .default_width(360.0)
            .min_width(280.0)
            .show(ctx, |ui| self.side_panel(ui));
        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| self.bottom_panel(ui));
        egui::CentralPanel::default().show(ctx, |ui| self.central_panel(ui));
    }
}
