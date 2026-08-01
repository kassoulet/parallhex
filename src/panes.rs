//! Central panes: three synchronized, zoomable views of the same file.
//!
//! Layout (left to right): the **overview** column (whole-file thumbnail,
//! drawn in `app.rs`), the **pixels** column (per-byte greyscale + entropy
//! bands) and the **hex** column (class-colored hex + ASCII cells). All
//! three share the same scroll position (`app.scroll_rows`, in rows), so
//! they stay in sync: scrolling or dragging any column pans the others.

use eframe::egui;

use crate::app::EntropyMapApp;
use crate::color;

const ADDR_X: f32 = 8.0;
const ROW_H: f32 = 18.0;
const ROW_GAP: f32 = 3.0;

/// Zoom limits and defaults, shared between the wheel-zoom handlers, the
/// column headers' readouts and their reset buttons.
pub(crate) const HEX_ZOOM_DEFAULT: f32 = 1.0;
pub(crate) const HEX_ZOOM_MIN: f32 = 0.5;
pub(crate) const HEX_ZOOM_MAX: f32 = 4.0;
pub(crate) const PIXEL_ZOOM_DEFAULT: f32 = 4.0;
pub(crate) const PIXEL_ZOOM_MIN: f32 = 1.0;
pub(crate) const PIXEL_ZOOM_MAX: f32 = 24.0;

/// Keyboard zoom step factor (`+` / `-`), applied multiplicatively per press.
pub(crate) const ZOOM_STEP: f32 = 1.25;

/// Apply a multiplicative zoom step, clamped to `[min, max]`. Shared by the
/// Ctrl+wheel handlers and the `+`/`-` keyboard shortcuts.
pub(crate) fn zoom_step(zoom: f32, factor: f32, min: f32, max: f32) -> f32 {
    (zoom * factor).clamp(min, max)
}

/// Height of one hex row at zoom `zoom` (1.0 = default).
pub(crate) fn hex_row_h(zoom: f32) -> f32 {
    ROW_H * zoom
}

/// Format a half-open byte range as a hex header label, e.g.
/// `0x00000000 – 0x000000FF`.
pub(crate) fn range_label(start: usize, end_exclusive: usize) -> String {
    if end_exclusive > start {
        format!("0x{start:08X} – 0x{:08X}", end_exclusive - 1)
    } else {
        format!("0x{start:08X}")
    }
}

/// Draw a column header: bold title, an optional muted byte-range label,
/// and a right-aligned row of trailing widgets (e.g. a zoom readout and
/// reset button). A separator line follows the header.
pub(crate) fn column_header(
    ui: &mut egui::Ui,
    title: &str,
    range: Option<String>,
    trailing: impl FnOnce(&mut egui::Ui),
) {
    // Title row: the trailing widgets (zoom controls) sit on the right and
    // get the full row width to themselves.
    ui.horizontal(|ui| {
        ui.strong(title);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            trailing(ui);
        });
    });
    // Second row: the visible byte range, on its own line so it never
    // crowds the zoom controls.
    if let Some(r) = range {
        ui.monospace(egui::RichText::new(r).color(egui::Color32::from_gray(150)));
    }
    ui.separator();
}

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
    /// Build the per-row geometry for the given (possibly zoom-scaled)
    /// monospace font: every metric is derived from its glyph width.
    fn new(ui: &egui::Ui, bpr: usize, font_id: &egui::FontId) -> Self {
        let char_w = ui.fonts(|f| f.glyph_width(font_id, '0'));
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

/// Hex column (right): class-colored hex + ASCII cells, one row per file
/// row. This column owns the master vertical scrollbar; its position is
/// written back to `app.scroll_rows` so the other columns follow. Primary
/// drag selects a range; **middle-mouse drag or Ctrl/Alt + primary drag
/// pans** (the same gesture the other columns use); right-click copies /
/// clears the selection; Ctrl+wheel / `+`/`-` / the header slider zoom the
/// text and row size.
pub fn show_hex(ui: &mut egui::Ui, app: &mut EntropyMapApp) {
    let bpr = app.bytes_per_row.max(1);
    let len = app.file_size;

    // ---- column header: title, visible byte range, zoom + reset ----
    let range = if len > 0 {
        let block_h = hex_row_h(app.hex_zoom) + ROW_GAP;
        let first = app.scroll_rows.floor().max(0.0) as usize;
        let total_rows = len.div_ceil(bpr);
        let vis_rows = ((ui.available_height() / block_h).ceil() as usize + 1)
            .min(total_rows.saturating_sub(first));
        Some(range_label(first * bpr, ((first + vis_rows) * bpr).min(len)))
    } else {
        None
    };
    column_header(ui, "Hex", range, |ui| {
        if ui
            .add(egui::Button::new("Reset zoom").small())
            .on_hover_text("Reset hex zoom to ×1.0")
            .clicked()
        {
            app.hex_zoom = HEX_ZOOM_DEFAULT;
        }
        ui.spacing_mut().slider_width = 90.0;
        ui.add(
            egui::Slider::new(&mut app.hex_zoom, HEX_ZOOM_MIN..=HEX_ZOOM_MAX)
                .logarithmic(true)
                .show_value(false),
        );
        ui.monospace(
            egui::RichText::new(format!("×{:.2}", app.hex_zoom)).color(egui::Color32::from_gray(150)),
        );
    });

    // Ctrl+wheel / pinch zooms the hex cells.
    if ui.rect_contains_pointer(ui.max_rect()) {
        let z = ui.input(|i| i.zoom_delta());
        if z != 1.0 {
            app.hex_zoom = zoom_step(app.hex_zoom, z, HEX_ZOOM_MIN, HEX_ZOOM_MAX);
        }
    }
    let row_h = hex_row_h(app.hex_zoom);
    let block_h = row_h + ROW_GAP;

    // One-shot scroll requests are applied before borrowing the data.
    let mut scroll_offset = app.scroll_rows * block_h;
    if app.scroll_reset {
        scroll_offset = 0.0;
        app.scroll_reset = false;
    }
    if let Some(off) = app.scroll_to_offset {
        // Center the target row in the viewport (content height unknown yet,
        // so clamp after the area is shown).
        let row = (off / bpr) as f32;
        scroll_offset = (row * block_h - ui.available_height().max(0.0) * 0.5).max(0.0);
        app.scroll_to_offset = None;
    }

    // ---- drag-to-pan: middle mouse, or Ctrl/Alt + primary drag ----
    // Plain primary drag still selects; this gesture matches the pixels
    // column so all three views drag to pan. `app.hex_rect` (from last
    // frame) gates the gesture to this column's content area.
    let hex_rect = app.hex_rect; // Rect is Copy
    let pan_active = ui.input(|i| {
        let p = &i.pointer;
        let middle = p.button_down(egui::PointerButton::Middle);
        // `command` covers Ctrl on non-macOS and Cmd on macOS; the explicit
        // `ctrl` covers the literal Ctrl key on macOS (Cmd is `command` there).
        let modded = p.primary_down() && (i.modifiers.command || i.modifiers.ctrl || i.modifiers.alt);
        let over = p.interact_pos().is_some_and(|pos| hex_rect.contains(pos));
        (middle || modded) && over
    });
    if pan_active {
        // Content follows the cursor: dragging down (dy > 0) shows earlier
        // rows, so the scroll offset decreases (mirrors show_pixels).
        let dy = ui.input(|i| i.pointer.delta().y);
        scroll_offset = (scroll_offset - dy).max(0.0);
    }

    let Some(data) = app.data() else { return };
    let len = data.len();
    if len == 0 {
        return;
    }
    let total_rows = len.div_ceil(bpr);
    // The hex zoom scales the text size: cells, spacing, the address gutter
    // and the row height all follow the scaled monospace font below.
    let base_font = egui::TextStyle::Monospace.resolve(ui.style());
    let font_id = egui::FontId::new(base_font.size * app.hex_zoom, base_font.family);
    let geo = RowGeo::new(ui, bpr, &font_id);
    let content_w = geo.content_w();
    let content_h = total_rows as f32 * block_h;
    let sel = app.selection_range.clone();

    let scroll_area = egui::ScrollArea::both()
        .auto_shrink([false, false])
        .drag_to_scroll(false)
        .vertical_scroll_offset(scroll_offset);

    let mut hovered: Option<usize> = None;
    let mut clicked: Option<usize> = None;
    let mut drag_started: Option<usize> = None;
    let mut drag_pos: Option<usize> = None;
    let mut drag_stopped = false;
    let mut menu_action: Option<&'static str> = None;
    let mut origin = egui::Pos2::ZERO;

    let out = scroll_area.show_viewport(ui, |ui, vp| {
        ui.allocate_space(egui::vec2(content_w, content_h));
        origin = ui.min_rect().min;
        let painter = ui.painter().clone();

        let first = (vp.min.y / block_h).floor().max(0.0) as usize;
        let last = ((vp.max.y / block_h).ceil() as usize + 1).min(total_rows);

        for row in first..last {
            let y0 = row as f32 * block_h;
            let block_rect = egui::Rect::from_min_size(
                egui::pos2(ADDR_X, y0),
                egui::vec2(vp.width(), block_h),
            );
            let resp = ui.interact(
                block_rect,
                ui.id().with(("row", row)),
                egui::Sense::click_and_drag(),
            );

            // While panning, the pointer belongs to the pan gesture: don't
            // start or extend a selection. Hover is still reported so the
            // readout stays live.
            if !pan_active {
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
                if resp.clicked() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        clicked = offset_from(p, origin, &geo, total_rows, len, block_h);
                    }
                }
            }
            if let Some(p) = resp.hover_pos() {
                hovered = offset_from(p, origin, &geo, total_rows, len, block_h);
            }
            resp.context_menu(|ui| {
                if ui.button("Copy Hex").clicked() {
                    menu_action = Some("hex");
                }
                if ui.button("Copy ASCII").clicked() {
                    menu_action = Some("ascii");
                }
                ui.separator();
                if ui.button("Clear selection").clicked() {
                    menu_action = Some("clear");
                }
            });

            // ---- rendering ----
            let row_start = row * bpr;
            let n = (len - row_start).min(bpr);

            painter.text(
                egui::pos2(ADDR_X, y0 + row_h * 0.5),
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
                let hex_cell = egui::Rect::from_min_size(
                    egui::pos2(cx, y0),
                    egui::vec2(geo.cell_w, row_h),
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
                    egui::pos2(cx, y0 + row_h * 0.5),
                    egui::Align2::LEFT_CENTER,
                    format!("{b:02X}"),
                    font_id.clone(),
                    fg,
                );

                let ax = geo.ascii_x(i);
                let ascii_cell = egui::Rect::from_min_size(
                    egui::pos2(ax, y0),
                    egui::vec2(geo.char_w, row_h),
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
                    egui::pos2(ax, y0 + row_h * 0.5),
                    egui::Align2::LEFT_CENTER,
                    color::printable(b).to_string(),
                    font_id.clone(),
                    fg,
                );
            }

            // Hover outline across hex + ascii cells.
            if let Some(o) = app.hovered_offset {
                if (row_start..row_start + n).contains(&o) {
                    let i = o - row_start;
                    let cx = geo.cell_x(i);
                    painter.rect_stroke(
                        egui::Rect::from_min_size(
                            egui::pos2(cx, y0),
                            egui::vec2(geo.cell_w, block_h - ROW_GAP),
                        ),
                        0.0,
                        egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
                    );
                }
            }

            // Row separator.
            painter.line_segment(
                [
                    egui::pos2(ADDR_X, y0 + block_h - ROW_GAP * 0.5),
                    egui::pos2(vp.width(), y0 + block_h - ROW_GAP * 0.5),
                ],
                egui::Stroke::new(1.0_f32, egui::Color32::from_gray(25)),
            );
        }
    });

    // Report the visible range to the overview marker.
    app.hex_rect = out.inner_rect;
    app.view_height = out.inner_rect.height();
    app.scroll_rows = (out.state.offset.y / block_h).clamp(0.0, total_rows as f32);
    if content_h > 0.0 {
        app.view_frac = (out.state.offset.y / content_h).clamp(0.0, 1.0);
        app.view_frac_h = (out.inner_rect.height() / content_h).clamp(0.0, 1.0);
    }

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
    }

    // ---- context-menu actions ----
    match menu_action {
        Some("clear") => app.selection_range = None,
        Some(kind) => {
            if let Some(d) = app.data() {
                if let Some(r) = app.selection_range.clone() {
                    let start = r.start;
                    let end = r.end.min(d.len());
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
        }
        _ => {}
    }
}

/// Pixels column (middle): per-byte greyscale + entropy bands at an
/// adjustable zoom. Drag to pan (syncs `app.scroll_rows` with the hex
/// column), wheel to scroll, Ctrl+wheel to zoom, click to select a byte.
pub fn show_pixels(ui: &mut egui::Ui, app: &mut EntropyMapApp) {
    let len = app.file_size;
    let bpr = app.bytes_per_row.max(1);
    let total_rows = len.div_ceil(bpr);

    // ---- column header: title, visible byte range, zoom + reset ----
    let range = if len > 0 {
        let px = app.pixel_zoom.clamp(PIXEL_ZOOM_MIN, PIXEL_ZOOM_MAX);
        let row_h = 2.0 * px + 1.0;
        let first = app.scroll_rows.floor().max(0.0) as usize;
        let vis_rows = ((ui.available_height() / row_h).ceil() as usize + 1)
            .min(total_rows.saturating_sub(first));
        Some(range_label(first * bpr, ((first + vis_rows) * bpr).min(len)))
    } else {
        None
    };
    column_header(ui, "Pixels", range, |ui| {
        if ui
            .add(egui::Button::new("Reset zoom").small())
            .on_hover_text("Reset pixel zoom to 4 px")
            .clicked()
        {
            app.pixel_zoom = PIXEL_ZOOM_DEFAULT;
        }
        ui.spacing_mut().slider_width = 90.0;
        ui.add(
            egui::Slider::new(&mut app.pixel_zoom, PIXEL_ZOOM_MIN..=PIXEL_ZOOM_MAX)
                .logarithmic(true)
                .show_value(false),
        );
        ui.monospace(
            egui::RichText::new(format!("{} px", app.pixel_zoom.round() as u32))
                .color(egui::Color32::from_gray(150)),
        );
    });
    if len == 0 {
        return;
    }

    let (rect, resp) = ui.allocate_exact_size(
        ui.available_size(),
        egui::Sense::click_and_drag(),
    );
    app.pixels_rect = rect;

    // Ctrl+wheel / pinch zooms the pixel size.
    let z = ui.input(|i| i.zoom_delta());
    if resp.hovered() && z != 1.0 {
        app.pixel_zoom = zoom_step(app.pixel_zoom, z, PIXEL_ZOOM_MIN, PIXEL_ZOOM_MAX);
    }
    let px = app.pixel_zoom.clamp(PIXEL_ZOOM_MIN, PIXEL_ZOOM_MAX);
    let band_h = px; // greyscale band height; entropy band sits below it
    let row_h = 2.0 * band_h + 1.0;

    // Drag to pan; wheel scrolls. Both sync `app.scroll_rows`, which the hex
    // column consumes as its master scroll position.
    if resp.dragged() {
        let dy = ui.input(|i| i.pointer.delta().y);
        app.scroll_rows -= dy / row_h;
    }
    if resp.hovered() {
        // egui: positive `smooth_scroll_delta.y` = scrolling down (content
        // moves up, offset increases) — so wheel down advances the file.
        let wheel = ui.input(|i| i.smooth_scroll_delta.y);
        if wheel != 0.0 {
            app.scroll_rows += wheel / row_h;
        }
    }
    // Clamp only to the file bounds: the hex column owns the real viewport
    // clamp, so restricting to the pixels viewport would fight the master
    // scroll and make EOF unreachable in the hex view.
    app.scroll_rows = app.scroll_rows.clamp(0.0, total_rows as f32);

    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(10));

    // Hover → preview offset; click → select a byte.
    if let Some(p) = resp.hover_pos() {
        let row = app.scroll_rows + (p.y - rect.min.y) / row_h;
        let col = ((p.x - rect.min.x) / px).floor();
        if row >= 0.0 && col >= 0.0 {
            let off = (row as usize * bpr + col as usize).min(len.saturating_sub(1));
            app.hovered_offset = Some(off);
        }
    }
    if resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let row = app.scroll_rows + (p.y - rect.min.y) / row_h;
            let col = ((p.x - rect.min.x) / px).floor();
            if row >= 0.0 && col >= 0.0 {
                let off = (row as usize * bpr + col as usize).min(len.saturating_sub(1));
                app.selected_offset = Some(off);
                app.hovered_offset = Some(off);
            }
        }
    }

    let Some(data) = app.data() else { return };
    let sel = app.selection_range.clone();
    let hovered = app.hovered_offset;

    // Draw only the visible rows.
    let first = app.scroll_rows.floor().max(0.0) as usize;
    let visible_rows = (rect.height() / row_h).ceil() as usize + 1;
    let last = (first + visible_rows).min(total_rows);
    for row in first..last {
        let y = rect.min.y + (row as f32 - app.scroll_rows) * row_h;
        let row_start = row * bpr;
        let n = (len - row_start).min(bpr);
        for i in 0..n {
            let off = row_start + i;
            let b = data[off];
            let x = rect.min.x + i as f32 * px;
            let grey = egui::Rect::from_min_size(
                egui::pos2(x, y),
                egui::vec2(px, band_h),
            );
            painter.rect_filled(grey, 0.0, egui::Color32::from_gray(b));
            let entr = egui::Rect::from_min_size(
                egui::pos2(x, y + band_h),
                egui::vec2(px, band_h),
            );
            painter.rect_filled(entr, 0.0, color::entropy_color(app.entropy_at(off)));

            let in_sel = sel.as_ref().is_some_and(|r| r.contains(&off));
            if in_sel {
                painter.rect_filled(
                    grey,
                    0.0,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 40),
                );
                painter.rect_filled(
                    entr,
                    0.0,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 40),
                );
            }
            if hovered == Some(off) {
                painter.rect_stroke(
                    egui::Rect::from_min_max(
                        egui::pos2(x, y),
                        egui::pos2(x + px, y + row_h),
                    ),
                    0.0,
                    egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
                );
            }
        }
    }
}
