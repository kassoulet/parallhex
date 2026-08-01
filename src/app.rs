//! Application state and the `eframe::App` shell.

use std::fs::File;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;
use memmap2::{Mmap, MmapOptions};

use crate::color;
use crate::entropy;
use crate::hexview;
use crate::hilbert;
use crate::texture;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Scan,
    Hilbert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Class,
    Entropy,
    Byte,
}

pub struct EntropyMapApp {
    pub file_path: Option<PathBuf>,
    pub mmap: Option<Arc<Mmap>>,
    pub file_size: usize,

    pub layout_mode: LayoutMode,
    pub color_mode: ColorMode,
    pub window_size: usize,
    pub zoom_level: f32,
    pub pan: egui::Vec2,
    pub auto_fit: bool,

    pub texture: Option<egui::TextureHandle>,
    pub texture_dirty: bool,
    pub texture_dims: (usize, usize),
    pub texture_stride: usize,

    pub hovered_offset: Option<usize>,
    pub selected_offset: Option<usize>,
    pub selection_range: Option<Range<usize>>,
    pub drag_start: Option<usize>,

    pub hex_scroll_target_row: Option<usize>,
    pub hex_last_offset: f32,

    pub message: Option<String>,
}

impl EntropyMapApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            file_path: None,
            mmap: None,
            file_size: 0,
            layout_mode: LayoutMode::Scan,
            color_mode: ColorMode::Class,
            window_size: 256,
            zoom_level: 1.0,
            pan: egui::Vec2::ZERO,
            auto_fit: true,
            texture: None,
            texture_dirty: false,
            texture_dims: (0, 0),
            texture_stride: 1,
            hovered_offset: None,
            selected_offset: None,
            selection_range: None,
            drag_start: None,
            hex_scroll_target_row: None,
            hex_last_offset: 0.0,
            message: None,
        }
    }

    pub(crate) fn data(&self) -> Option<&[u8]> {
        self.mmap.as_ref().map(|m| &m[..])
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
        self.texture_dirty = true;
        self.auto_fit = true;
        self.zoom_level = 1.0;
        self.pan = egui::Vec2::ZERO;
        self.hovered_offset = None;
        self.selected_offset = None;
        self.selection_range = None;
        self.drag_start = None;
        self.hex_scroll_target_row = None;
        self.message = None;
    }

    fn regenerate_texture(&mut self, ctx: &egui::Context) {
        self.texture_dirty = false;
        let Some(info) = texture::generate(self) else {
            return;
        };
        self.texture_dims = (info.width, info.height);
        self.texture_stride = info.stride;
        self.texture = Some(ctx.load_texture(
            "entropymap",
            info.image,
            egui::TextureOptions::NEAREST,
        ));
        ctx.request_repaint();
    }

    fn top_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("Open File…").clicked() {
                self.open_dialog();
            }
            ui.separator();

            let prev = self.layout_mode;
            egui::ComboBox::from_label("Layout")
                .selected_text(match self.layout_mode {
                    LayoutMode::Scan => "Scan",
                    LayoutMode::Hilbert => "Hilbert",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.layout_mode, LayoutMode::Scan, "Scan (256 / row)");
                    ui.selectable_value(&mut self.layout_mode, LayoutMode::Hilbert, "Hilbert curve");
                });
            if prev != self.layout_mode {
                self.texture_dirty = true;
                self.auto_fit = true;
            }

            let prev = self.color_mode;
            egui::ComboBox::from_label("Color")
                .selected_text(match self.color_mode {
                    ColorMode::Class => "Byte class",
                    ColorMode::Entropy => "Entropy",
                    ColorMode::Byte => "Byte value",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.color_mode, ColorMode::Class, "Byte class");
                    ui.selectable_value(&mut self.color_mode, ColorMode::Entropy, "Entropy");
                    ui.selectable_value(&mut self.color_mode, ColorMode::Byte, "Byte value");
                });
            if prev != self.color_mode {
                self.texture_dirty = true;
            }

            ui.separator();
            if ui
                .add(
                    egui::Slider::new(&mut self.window_size, 16..=4096)
                        .logarithmic(true)
                        .text("Entropy window"),
                )
                .changed()
            {
                self.texture_dirty = true;
            }

            ui.separator();
            if ui.button("Reset view").clicked() {
                self.auto_fit = true;
                self.zoom_level = 1.0;
                self.pan = egui::Vec2::ZERO;
            }
        });
    }

    fn bottom_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(off) = self.hovered_offset {
                if let Some(d) = self.data() {
                    if off < d.len() {
                        let b = d[off];
                        let h = entropy::window_entropy_at(d, off, self.window_size);
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
            ui.separator();
            ui.label(format!(
                "Size: {} ({})",
                self.file_size,
                color::human_size(self.file_size)
            ));
            ui.separator();
            let (w, h) = self.texture_dims;
            ui.label(format!("View: {w}×{h}  stride {}", self.texture_stride));
            ui.separator();
            ui.label(format!("Zoom: {:.2}×", self.zoom_level));
            if let Some(msg) = &self.message {
                ui.separator();
                ui.colored_label(egui::Color32::YELLOW, msg);
            }
        });
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("EntropyMap");
        ui.label("Interactive binary visualization");
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
        ui.label(format!("Size: {} bytes ({})", self.file_size, color::human_size(self.file_size)));
        ui.separator();

        ui.strong("Inspector");
        self.byte_info(ui, "Hover:", self.hovered_offset);
        self.byte_info(ui, "Selected:", self.selected_offset);
        ui.separator();

        self.selection_section(ui);
        ui.separator();

        ui.strong("Hex Viewer");
        ui.add_space(2.0);
        if ui.available_height() > 120.0 {
            hexview::show(ui, self);
        } else {
            ui.label("(side panel too short for hex view)");
        }
    }

    fn byte_info(&mut self, ui: &mut egui::Ui, label: &str, off: Option<usize>) {
        ui.horizontal(|ui| {
            ui.label(label);
            if let Some(o) = off {
                if let Some(d) = self.data() {
                    if o < d.len() {
                        let b = d[o];
                        let h = entropy::window_entropy_at(d, o, self.window_size);
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
            ui.label(format!("Range: 0x{start:08X} – 0x{:08X}", end.saturating_sub(1)));
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
        let avail = ui.available_size();
        let (tw, th) = self.texture_dims;

        let Some(texture) = self.texture.as_ref() else {
            ui.centered_and_justified(|ui| {
                ui.label("No file loaded.\n\nOpen a binary file to visualize its byte map.");
            });
            return;
        };
        let texture_id = texture.id();
        if tw == 0 || th == 0 {
            ui.centered_and_justified(|ui| {
                ui.label("Empty file.");
            });
            return;
        }

        if self.auto_fit {
            let fit = (avail.x / tw as f32).min(avail.y / th as f32);
            self.zoom_level = fit.clamp(0.02, 64.0);
            self.pan = egui::Vec2::ZERO;
            self.auto_fit = false;
        }

        let (response, painter) = ui.allocate_painter(avail, egui::Sense::click_and_drag());
        let rect = response.rect;
        painter.rect_filled(rect, 0.0, egui::Color32::from_gray(10));

        let scale = self.zoom_level;
        let img_size = egui::vec2(tw as f32 * scale, th as f32 * scale);
        let img_min = rect.center() - img_size * 0.5 + self.pan;
        let img_rect = egui::Rect::from_min_size(img_min, img_size);

        let (scroll, zoom, ctrl) = ui.input(|i| {
            (
                i.smooth_scroll_delta.y,
                i.zoom_delta(),
                i.modifiers.ctrl || i.modifiers.command,
            )
        });
        let wheel = if ctrl { zoom } else { (scroll * 0.01).exp() };
        if let Some(hover) = response.hover_pos() {
            if (wheel - 1.0).abs() > 1e-6 {
                let old_scale = scale;
                let new_scale = (old_scale * wheel).clamp(0.02, 256.0);
                let real = new_scale / old_scale;
                self.zoom_level = new_scale;
                let anchor = hover - img_rect.min;
                let new_min = hover - anchor * real;
                self.pan = new_min
                    - (rect.center() - egui::vec2(tw as f32 * new_scale, th as f32 * new_scale) * 0.5);
            }
        }

        painter.image(
            texture_id,
            img_rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        painter.rect_stroke(img_rect, 0.0, egui::Stroke::new(1.0_f32, egui::Color32::from_gray(70)));

        let shift = ui.input(|i| i.modifiers.shift);
        let pan_drag = response.dragged_by(egui::PointerButton::Middle)
            || response.dragged_by(egui::PointerButton::Secondary)
            || (shift && response.dragged_by(egui::PointerButton::Primary));
        if pan_drag {
            self.pan += response.drag_delta();
        }

        let selecting = !shift && response.dragged_by(egui::PointerButton::Primary);
        if selecting {
            if response.drag_started() {
                if let Some(o) = self.offset_at(&img_rect, response.interact_pointer_pos()) {
                    self.drag_start = Some(o);
                    self.selected_offset = Some(o);
                    self.selection_range = None;
                }
            } else if let Some(start) = self.drag_start {
                if let Some(o) = self.offset_at(&img_rect, response.interact_pointer_pos()) {
                    let (a, b) = (start.min(o), start.max(o) + 1);
                    self.selection_range = Some(a..b.min(self.file_size));
                    self.selected_offset = Some(o);
                }
            }
        } else if response.drag_stopped() {
            self.drag_start = None;
        }
        if response.clicked() && !shift {
            if let Some(o) = self.offset_at(&img_rect, response.interact_pointer_pos()) {
                self.selected_offset = Some(o);
                self.hex_scroll_target_row = Some(o / 16);
            }
        }

        let new_hover = response.hover_pos().and_then(|p| self.offset_at(&img_rect, Some(p)));
        if new_hover != self.hovered_offset {
            self.hovered_offset = new_hover;
            if let Some(o) = new_hover {
                self.hex_scroll_target_row = Some(o / 16);
            }
        }

        self.draw_selection_overlay(&painter, &img_rect);

        if let Some(o) = self.hovered_offset {
            if let Some((x, y)) = self.pixel_coords(o) {
                let r = self.pixel_rect((x, y), &img_rect);
                painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0_f32, egui::Color32::WHITE));
            }
        }
    }

    /// Map a canvas position to a file offset, or `None` if outside the image.
    fn offset_at(&self, img_rect: &egui::Rect, pos: Option<egui::Pos2>) -> Option<usize> {
        let pos = pos?;
        if !img_rect.contains(pos) {
            return None;
        }
        let (tw, th) = self.texture_dims;
        if tw == 0 || th == 0 {
            return None;
        }
        let u = ((pos.x - img_rect.min.x) / img_rect.width()).clamp(0.0, 1.0);
        let v = ((pos.y - img_rect.min.y) / img_rect.height()).clamp(0.0, 1.0);
        let x = ((u * tw as f32) as usize).min(tw - 1);
        let y = ((v * th as f32) as usize).min(th - 1);
        let i = match self.layout_mode {
            LayoutMode::Scan => y * tw + x,
            LayoutMode::Hilbert => hilbert::xy2d(th, x, y),
        };
        let off = i * self.texture_stride;
        (off < self.file_size).then_some(off)
    }

    fn pixel_coords(&self, off: usize) -> Option<(usize, usize)> {
        let (tw, th) = self.texture_dims;
        let i = off / self.texture_stride.max(1);
        if tw == 0 || th == 0 {
            return None;
        }
        match self.layout_mode {
            LayoutMode::Scan => {
                if i >= tw * th {
                    return None;
                }
                Some((i % tw, i / tw))
            }
            LayoutMode::Hilbert => {
                if i >= tw * th {
                    return None;
                }
                Some(hilbert::d2xy(th, i))
            }
        }
    }

    fn pixel_rect(&self, (x, y): (usize, usize), img_rect: &egui::Rect) -> egui::Rect {
        let scale = self.zoom_level;
        egui::Rect::from_min_size(
            img_rect.min + egui::vec2(x as f32 * scale, y as f32 * scale),
            egui::vec2(scale, scale),
        )
    }

    fn draw_selection_overlay(&self, painter: &egui::Painter, img_rect: &egui::Rect) {
        let Some(range) = &self.selection_range else {
            return;
        };
        let n = range.end.saturating_sub(range.start);
        if n == 0 {
            return;
        }
        let fill = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60);
        if n <= 20_000 {
            let mut off = range.start;
            while off < range.end {
                if let Some((x, y)) = self.pixel_coords(off) {
                    let r = self.pixel_rect((x, y), img_rect);
                    painter.rect_filled(r, 0.0, fill);
                }
                off += 1;
            }
        } else {
            if let (Some(a), Some(b)) = (
                self.pixel_coords(range.start),
                self.pixel_coords(range.end.saturating_sub(1)),
            ) {
                let ra = self.pixel_rect(a, img_rect);
                let rb = self.pixel_rect(b, img_rect);
                let rect = egui::Rect::from_min_max(ra.min, rb.max);
                painter.rect_filled(rect, 0.0, fill);
                painter.rect_stroke(
                    rect,
                    0.0,
                    egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 140)),
                );
            }
        }
    }
}

impl eframe::App for EntropyMapApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.key_pressed(egui::Key::O) && i.modifiers.command) {
            self.open_dialog();
        }
        if self.texture_dirty {
            self.regenerate_texture(ctx);
        }
        egui::TopBottomPanel::top("top").show(ctx, |ui| self.top_panel(ui));
        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| self.bottom_panel(ui));
        egui::SidePanel::right("side")
            .resizable(true)
            .default_width(420.0)
            .min_width(280.0)
            .show(ctx, |ui| self.side_panel(ui));
        egui::CentralPanel::default().show(ctx, |ui| self.central_panel(ui));
    }
}
