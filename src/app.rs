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

    // Cached Shannon entropy per `entropy_window`-sized block (whole file).
    pub entropies: Vec<f32>,

    // Selection & hover state (shared by all four panes).
    pub hovered_offset: Option<usize>,
    pub selected_offset: Option<usize>,
    pub selection_range: Option<Range<usize>>,
    pub drag_start: Option<usize>,

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
            entropies: Vec::new(),
            hovered_offset: None,
            selected_offset: None,
            selection_range: None,
            drag_start: None,
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
        self.scroll_reset = true;
        self.hovered_offset = None;
        self.selected_offset = None;
        self.selection_range = None;
        self.drag_start = None;
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
            }

            ui.separator();
            if ui.button("Reset view").clicked() {
                self.scroll_reset = true;
            }
        });
    }

    fn bottom_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(off) = self.hovered_offset {
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

        ui.strong("Inspector");
        self.byte_info(ui, "Hover:", self.hovered_offset);
        self.byte_info(ui, "Selected:", self.selected_offset);
        ui.separator();

        self.selection_section(ui);
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
        egui::TopBottomPanel::top("top").show(ctx, |ui| self.top_panel(ui));
        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| self.bottom_panel(ui));
        egui::SidePanel::right("side")
            .resizable(true)
            .default_width(360.0)
            .min_width(280.0)
            .show(ctx, |ui| self.side_panel(ui));
        egui::CentralPanel::default().show(ctx, |ui| self.central_panel(ui));
    }
}
