//! Central panel: four synchronized hex-viewer panes.
//!
//! All panes (hex+ASCII, direct greyscale, entropy) are drawn from the same
//! byte rows inside a single virtualized scroll area, so they are always
//! aligned and scrolled together. Rows are only painted when visible.

use eframe::egui;

use crate::app::EntropyMapApp;
use crate::color;

const ADDR_X: f32 = 8.0;
const ROW_H: f32 = 18.0;
const MAP_H: f32 = 8.0;
const BLOCK_GAP: f32 = 3.0;

/// Per-row horizontal geometry for one bytes-per-row layout.
struct RowGeo {
    bpr: usize,
    hex_start: f32,
    cell_w: f32,
    group_gap: f32,
    ascii_start: f32,
    char_w: f32,
}

impl RowGeo {
    fn new(ui: &egui::Ui, bpr: usize) -> Self {
        let font_id = egui::TextStyle::Monospace.resolve(ui.style());
        let char_w = ui.fonts(|f| f.glyph_width(&font_id, '0'));
        let addr_w = 8.0 * char_w;
        let hex_start = ADDR_X + addr_w + 12.0;
        let cell_w = 3.0 * char_w;
        let group_gap = 2.0 * char_w;
        let hex_w = bpr as f32 * cell_w + (bpr / 8) as f32 * group_gap;
        let ascii_start = hex_start + hex_w + 12.0;
        Self {
            bpr,
            hex_start,
            cell_w,
            group_gap,
            ascii_start,
            char_w,
        }
    }

    fn cell_x(&self, i: usize) -> f32 {
        self.hex_start + i as f32 * self.cell_w + (i / 8) as f32 * self.group_gap
    }

    fn ascii_x(&self, i: usize) -> f32 {
        self.ascii_start + i as f32 * self.char_w
    }

    /// Total content width (address + hex + ascii + margin).
    fn content_w(&self) -> f32 {
        self.ascii_start + self.bpr as f32 * self.char_w + ADDR_X
    }

    /// Column index under an x position within the row, or `None` when the
    /// pointer is over a gap or the address gutter.
    fn byte_at_x(&self, x: f32) -> Option<usize> {
        if x >= self.hex_start {
            for i in 0..self.bpr {
                let x0 = self.cell_x(i);
                if x >= x0 && x < x0 + self.cell_w {
                    return Some(i);
                }
            }
        }
        if x >= self.ascii_start {
            for i in 0..self.bpr {
                let x0 = self.ascii_x(i);
                if x >= x0 && x < x0 + self.char_w {
                    return Some(i);
                }
            }
        }
        None
    }
}

/// Map a screen-space pointer to a file offset, or `None` when outside the
/// content or over a gap.
fn offset_from(
    p: egui::Pos2,
    origin: egui::Pos2,
    geo: &RowGeo,
    total_rows: usize,
    len: usize,
    block_h: f32,
) -> Option<usize> {
    let c = egui::pos2(p.x - origin.x, p.y - origin.y);
    if c.y < 0.0 {
        return None;
    }
    let row = (c.y / block_h) as usize;
    if row >= total_rows {
        return None;
    }
    let row_start = row * geo.bpr;
    if row_start >= len {
        return None;
    }
    let i = geo.byte_at_x(c.x)?;
    let off = row_start + i;
    (off < len).then_some(off)
}

pub fn show_central(ui: &mut egui::Ui, app: &mut EntropyMapApp) {
    let bpr = app.bytes_per_row.max(1);
    let block_h = ROW_H + 2.0 * MAP_H + BLOCK_GAP;

    let mut scroll_area = egui::ScrollArea::both()
        .auto_shrink([false, false])
        .drag_to_scroll(false);
    if app.scroll_reset {
        scroll_area = scroll_area
            .vertical_scroll_offset(0.0)
            .horizontal_scroll_offset(0.0);
        app.scroll_reset = false;
    }

    let Some(data) = app.data() else { return };
    let len = data.len();
    if len == 0 {
        return;
    }
    let total_rows = len.div_ceil(bpr);

    let font_id = egui::TextStyle::Monospace.resolve(ui.style());
    let geo = RowGeo::new(ui, bpr);
    let content_w = geo.content_w();
    let sel = app.selection_range.clone();

    let mut hovered: Option<usize> = None;
    let mut clicked: Option<usize> = None;
    let mut drag_started: Option<usize> = None;
    let mut drag_pos: Option<usize> = None;
    let mut drag_stopped = false;
    let mut viewport = egui::Rect::NOTHING;
    let mut origin = egui::Pos2::ZERO;

    scroll_area.show_viewport(ui, |ui, vp| {
        viewport = vp;
        ui.allocate_space(egui::vec2(content_w, total_rows as f32 * block_h));
        origin = ui.min_rect().min;
        let painter = ui.painter().clone();

        let first = (viewport.min.y / block_h).floor().max(0.0) as usize;
        let last = ((viewport.max.y / block_h).ceil() as usize + 1).min(total_rows);

        for row in first..last {
            let y0 = row as f32 * block_h;
            let block_rect = egui::Rect::from_min_size(
                egui::pos2(ADDR_X, y0),
                egui::vec2(viewport.width(), block_h),
            );
            let resp = ui.interact(
                block_rect,
                ui.id().with(("row", row)),
                egui::Sense::click_and_drag(),
            );

            if resp.drag_started() {
                drag_started = resp
                    .interact_pointer_pos()
                    .and_then(|p| offset_from(p, origin, &geo, total_rows, len, block_h));
            }
            if resp.dragged() {
                drag_pos = resp
                    .interact_pointer_pos()
                    .and_then(|p| offset_from(p, origin, &geo, total_rows, len, block_h));
            }
            if resp.drag_stopped() {
                drag_stopped = true;
            }
            if let Some(p) = resp.hover_pos() {
                hovered = offset_from(p, origin, &geo, total_rows, len, block_h);
            }
            if resp.clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    clicked = offset_from(p, origin, &geo, total_rows, len, block_h);
                }
            }

            // ---- rendering ----
            let row_start = row * bpr;
            let n = (len - row_start).min(bpr);

            painter.text(
                egui::pos2(ADDR_X, y0 + ROW_H * 0.5),
                egui::Align2::LEFT_CENTER,
                format!("{row_start:08X}"),
                font_id.clone(),
                egui::Color32::from_gray(120),
            );

            for i in 0..n {
                let off = row_start + i;
                let b = data[off];
                let class = color::class_color(b);
                let fg = color::fg_for_class(class);
                let in_sel = sel.as_ref().is_some_and(|r| r.contains(&off));

                let cx = geo.cell_x(i);
                // Hex cell: class color as background.
                let hex_cell = egui::Rect::from_min_size(
                    egui::pos2(cx, y0),
                    egui::vec2(geo.cell_w, ROW_H),
                );
                painter.rect_filled(hex_cell, 0.0, class);
                if in_sel {
                    painter.rect_filled(
                        hex_cell,
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60),
                    );
                }
                painter.text(
                    egui::pos2(cx, y0 + ROW_H * 0.5),
                    egui::Align2::LEFT_CENTER,
                    format!("{b:02X}"),
                    font_id.clone(),
                    fg,
                );

                // ASCII cell: same class background.
                let ax = geo.ascii_x(i);
                let ascii_cell = egui::Rect::from_min_size(
                    egui::pos2(ax, y0),
                    egui::vec2(geo.char_w, ROW_H),
                );
                painter.rect_filled(ascii_cell, 0.0, class);
                if in_sel {
                    painter.rect_filled(
                        ascii_cell,
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60),
                    );
                }
                painter.text(
                    egui::pos2(ax, y0 + ROW_H * 0.5),
                    egui::Align2::LEFT_CENTER,
                    color::printable(b).to_string(),
                    font_id.clone(),
                    fg,
                );

                // Direct greyscale pixel.
                let grey_rect = egui::Rect::from_min_size(
                    egui::pos2(cx, y0 + ROW_H),
                    egui::vec2(geo.cell_w, MAP_H),
                );
                painter.rect_filled(grey_rect, 0.0, egui::Color32::from_gray(b));
                if in_sel {
                    painter.rect_filled(
                        grey_rect,
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60),
                    );
                }

                // Entropy pixel.
                let h = app.entropy_at(off);
                let entr_rect = egui::Rect::from_min_size(
                    egui::pos2(cx, y0 + ROW_H + MAP_H),
                    egui::vec2(geo.cell_w, MAP_H),
                );
                painter.rect_filled(entr_rect, 0.0, color::entropy_color(h));
                if in_sel {
                    painter.rect_filled(
                        entr_rect,
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60),
                    );
                }
            }

            // Hover outline across all three representations of the byte.
            if let Some(o) = app.hovered_offset {
                if (row_start..row_start + n).contains(&o) {
                    let i = o - row_start;
                    let cx = geo.cell_x(i);
                    painter.rect_stroke(
                        egui::Rect::from_min_size(
                            egui::pos2(cx, y0),
                            egui::vec2(geo.cell_w, block_h - BLOCK_GAP),
                        ),
                        0.0,
                        egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
                    );
                }
            }

            // Row separator.
            painter.line_segment(
                [
                    egui::pos2(ADDR_X, y0 + block_h - BLOCK_GAP * 0.5),
                    egui::pos2(viewport.width(), y0 + block_h - BLOCK_GAP * 0.5),
                ],
                egui::Stroke::new(1.0_f32, egui::Color32::from_gray(25)),
            );
        }
    });

    // ---- apply interaction results to shared state ----
    if let Some(o) = drag_started {
        app.drag_start = Some(o);
        app.selection_range = None;
        app.selected_offset = Some(o);
    }
    if let Some(o) = drag_pos {
        if let Some(start) = app.drag_start {
            let (a, b) = (start.min(o), start.max(o) + 1);
            app.selection_range = Some(a..b.min(len));
            app.selected_offset = Some(o);
        }
    }
    if drag_stopped {
        app.drag_start = None;
    }
    if let Some(o) = clicked {
        app.selected_offset = Some(o);
    }
    if let Some(o) = hovered {
        app.hovered_offset = Some(o);
    } else if !(viewport.is_positive()
        && ui.ctx().pointer_hover_pos().is_some_and(|p| {
            viewport.contains(egui::pos2(p.x - origin.x, p.y - origin.y))
        }))
    {
        // Pointer left the content: clear the stale hover.
        app.hovered_offset = None;
    }
}
