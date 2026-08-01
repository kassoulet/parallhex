//! Virtualized hex dump viewer synced with the visual canvas.

use eframe::egui;

use crate::app::EntropyMapApp;
use crate::color;

pub fn show(ui: &mut egui::Ui, app: &mut EntropyMapApp) {
    let data: &[u8] = match &app.mmap {
        Some(m) => &m[..],
        None => return,
    };
    let len = data.len();
    if len == 0 {
        return;
    }
    let total_rows = len.div_ceil(16);

    let font_id = egui::TextStyle::Monospace.resolve(ui.style());
    let char_w = ui.fonts(|f| f.glyph_width(&font_id, '0'));
    let row_h = 18.0;

    let addr_x = 8.0;
    let addr_w = 8.0 * char_w;
    let hex_start = addr_x + addr_w + 12.0;
    let byte_w = 3.0 * char_w;
    let group_gap = 2.0 * char_w;
    let hex_w = 16.0 * byte_w + group_gap;
    let ascii_start = hex_start + hex_w + 12.0;

    let view_h = ui.available_height();

    let mut target_offset = app.hex_last_offset;
    let mut clear_target = false;
    if let Some(row) = app.hex_scroll_target_row {
        let desired = (row as f32) * row_h - view_h * 0.5;
        target_offset = desired.max(0.0);
        if (app.hex_last_offset - desired).abs() < 2.0 {
            clear_target = true;
        }
    }

    let visuals = ui.visuals().clone();
    let mut hovered: Option<usize> = None;
    let mut clicked: Option<usize> = None;

    let out = egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .vertical_scroll_offset(target_offset)
        .show_viewport(ui, |ui, viewport| {
            let content_h = total_rows as f32 * row_h;
            ui.allocate_space(egui::vec2(ui.available_width(), content_h));

            let first = (viewport.min.y / row_h).floor().max(0.0) as usize;
            let last = ((viewport.max.y / row_h).ceil() as usize + 1).min(total_rows);

            for idx in first..last {
                let y = idx as f32 * row_h;
                let row_rect =
                    egui::Rect::from_min_size(egui::pos2(addr_x, y), egui::vec2(viewport.width(), row_h));
                let resp = ui.interact(row_rect, ui.id().with(("hexrow", idx)), egui::Sense::click());
                let row_start = idx * 16;
                let bytes_in_row = (len - row_start).min(16);

                let mut hover_byte: Option<usize> = None;
                if let Some(p) = resp.hover_pos() {
                    let lx = p.x;
                    for i in 0..bytes_in_row {
                        let x0 = hex_start + i as f32 * byte_w + if i >= 8 { group_gap } else { 0.0 };
                        if lx >= x0 && lx < x0 + 2.0 * char_w {
                            hover_byte = Some(i);
                            break;
                        }
                    }
                    if hover_byte.is_none() {
                        for i in 0..bytes_in_row {
                            let x0 = ascii_start + i as f32 * char_w;
                            if lx >= x0 && lx < x0 + char_w {
                                hover_byte = Some(i);
                                break;
                            }
                        }
                    }
                }
                if let Some(bi) = hover_byte {
                    hovered = Some(row_start + bi);
                }
                if resp.clicked() {
                    if let Some(bi) = hover_byte {
                        clicked = Some(row_start + bi);
                    }
                }

                let sel_row = app.selected_offset.map(|o| o / 16) == Some(idx);
                let bg = if sel_row {
                    visuals.selection.bg_fill
                } else if resp.hovered() {
                    visuals.widgets.hovered.bg_fill
                } else {
                    egui::Color32::TRANSPARENT
                };
                if bg != egui::Color32::TRANSPARENT {
                    ui.painter().rect_filled(row_rect, 0.0, bg);
                }

                if let Some(sel) = &app.selection_range {
                    let lo = sel.start.max(row_start);
                    let hi = sel.end.min(row_start + 16);
                    if lo < hi {
                        for i in (lo - row_start)..(hi - row_start) {
                            let x0 = hex_start + i as f32 * byte_w + if i >= 8 { group_gap } else { 0.0 };
                            let cell = egui::Rect::from_min_size(
                                egui::pos2(x0, y),
                                egui::vec2(2.0 * char_w, row_h),
                            );
                            ui.painter().rect_filled(cell, 0.0, visuals.selection.bg_fill);
                        }
                    }
                }

                ui.painter().text(
                    egui::pos2(addr_x, y + row_h * 0.5),
                    egui::Align2::LEFT_CENTER,
                    format!("{row_start:08X}"),
                    font_id.clone(),
                    egui::Color32::from_gray(120),
                );

                for i in 0..bytes_in_row {
                    let b = data[row_start + i];
                    let c = color::class_color(b);
                    let hex_x = hex_start + i as f32 * byte_w + if i >= 8 { group_gap } else { 0.0 };
                    ui.painter().text(
                        egui::pos2(hex_x, y + row_h * 0.5),
                        egui::Align2::LEFT_CENTER,
                        format!("{b:02X}"),
                        font_id.clone(),
                        c,
                    );
                    let ax = ascii_start + i as f32 * char_w;
                    ui.painter().text(
                        egui::pos2(ax, y + row_h * 0.5),
                        egui::Align2::LEFT_CENTER,
                        color::printable(b).to_string(),
                        font_id.clone(),
                        c,
                    );
                }
            }
        });

    app.hex_last_offset = out.state.offset.y;
    if clear_target {
        app.hex_scroll_target_row = None;
    }
    if let Some(o) = hovered {
        app.hovered_offset = Some(o);
    }
    if let Some(o) = clicked {
        app.selected_offset = Some(o);
    }
}
