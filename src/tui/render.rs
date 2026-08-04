//! Drawing the three columns.
//!
//! The two graphical columns reuse the same RGBA generators the gpui frontend
//! uses, blitted as half-blocks. The hex column reuses `RowGeo` at `char_w = 1.0`
//! with no gutter, so its cell positions and 8-byte group gaps need no
//! terminal-specific arithmetic.

use std::fmt::Write as _;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::Block;

use crate::core::color::{self, Rgb};
use crate::core::geom::{self, RowGeo};
use crate::core::thumb;
use crate::tui::app::{Focus, PanelLayout, TuiApp};
use crate::tui::blit::blit_half_blocks;

/// Border of the focused column, and of the rest.
const FOCUSED: Color = Color::Rgb(0x7a, 0xa2, 0xf7);
const UNFOCUSED: Color = Color::Rgb(0x3b, 0x42, 0x61);
const MUTED: Color = Color::Rgb(0x56, 0x5f, 0x89);
/// Selection tint. The gpui frontend overlays translucent white; a terminal
/// cannot blend, so this is a fixed stand-in -- distinct from UNFOCUSED so the
/// two are never confused when reading a rendered buffer.
const SELECTED_BG: Color = Color::Rgb(0x44, 0x49, 0x6b);

/// Draw a frame and record the measured layout for the input layer.
pub(crate) fn draw(frame: &mut Frame, app: &mut TuiApp) {
    let [body, status] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.size());
    // Proportions mirror the gpui frontend's defaults; hex takes the remainder
    // because it is the widest and the scroll reference.
    let [overview, zoom, hex] = Layout::horizontal([
        Constraint::Percentage(15),
        Constraint::Percentage(28),
        Constraint::Min(20),
    ])
    .areas(body);

    let ov_inner = column(frame, app, Focus::Overview, overview);
    let zoom_inner = column(frame, app, Focus::Zoom, zoom);
    let hex_inner = column(frame, app, Focus::Hex, hex);

    // Recorded before painting, so a panel that renders nothing this frame still
    // reports its size and the input layer's row lengths stay correct.
    app.layout = PanelLayout {
        overview_cols: ov_inner.width as usize,
        zoom_cols: zoom_inner.width as usize,
        hex_cols: hex_inner.width as usize,
        text_rows: hex_inner.height as usize,
    };

    draw_overview(frame, app, ov_inner);
    draw_zoom(frame, app, zoom_inner);
    draw_hex(frame, app, hex_inner);
    draw_status(frame, app, status);
}

/// Draw a column's border and header, returning the area left for content.
fn column(frame: &mut Frame, app: &TuiApp, focus: Focus, area: Rect) -> Rect {
    let border = if app.focus == focus {
        FOCUSED
    } else {
        UNFOCUSED
    };
    let title = format!(" {} · {} ", focus.title(), app.colormap(focus).label());
    let block = Block::bordered()
        .border_style(Style::new().fg(border))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

fn draw_overview(frame: &mut Frame, app: &TuiApp, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let w = area.width as usize;
    // Two pixel rows per text row is what the half-block blit consumes.
    let rgba = thumb::build_overview_rgba(
        &app.byte_source(Focus::Overview),
        w,
        area.height as usize * 2,
    );
    blit_half_blocks(frame.buffer_mut(), area, &rgba, w);
}

fn draw_zoom(frame: &mut Frame, app: &TuiApp, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let cols = area.width as usize;
    let rows_px = area.height as usize * 2;
    let first = geom::first_row_centred(app.anchor, cols, rows_px);
    // `block = 1.0` makes the generator emit exactly one pixel per byte, which is
    // one byte per half-cell -- no separate generator needed.
    let (rgba, iw, _ih) =
        thumb::build_zoom_rgba(&app.byte_source(Focus::Zoom), cols, first, rows_px, 1.0);
    if iw > 0 {
        blit_half_blocks(frame.buffer_mut(), area, &rgba, iw);
    }
}

fn draw_hex(frame: &mut Frame, app: &TuiApp, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let data = app.data.as_slice();
    let len = data.len();
    let bpr = app.bpr_for(Focus::Hex).max(8);
    let rows = area.height as usize;
    let first = geom::first_row_centred(app.anchor, bpr, rows);
    // One cell per character and no gutter: the shared geometry, unchanged.
    let geo = RowGeo::new(0.0, 1.0, bpr);
    let src = app.byte_source(Focus::Hex);

    let mut text = String::new();
    let mut hex_offsets = Vec::new();
    let mut ascii_offsets = Vec::new();

    for r in 0..rows {
        let row_start = first + r * bpr;
        if row_start >= len {
            break;
        }
        let n = (len - row_start).min(bpr);
        let y = area.y + u16::try_from(r).unwrap_or(u16::MAX);
        geom::build_row_text_into(
            data,
            row_start,
            n,
            &mut text,
            &mut hex_offsets,
            &mut ascii_offsets,
        );
        // The address, then each byte styled individually.
        frame.buffer_mut().set_stringn(
            area.x,
            y,
            &text[..10.min(text.len())],
            area.width as usize,
            Style::new().fg(MUTED),
        );
        for i in 0..n {
            let off = row_start + i;
            let selected = app.selection.as_ref().is_some_and(|s| s.contains(&off));
            let style = cell_style(src.color_at(off), selected, off == app.cursor);
            // Two digits only: the space between bytes keeps the terminal
            // background, matching the gpui frontend's cell fills.
            put(
                frame,
                area,
                geo.cell_x(i),
                y,
                &text[hex_offsets[i]..hex_offsets[i] + 2],
                style,
            );
            let a = ascii_offsets[i];
            put(frame, area, geo.ascii_x(i), y, &text[a..=a], style);
        }
    }
}

/// Write `s` at a column offset within `area`, clipped to it.
fn put(frame: &mut Frame, area: Rect, x_off: f32, y: u16, s: &str, style: Style) {
    let x = area.x + (x_off as u16);
    if x >= area.x + area.width {
        return;
    }
    let room = (area.x + area.width - x) as usize;
    frame.buffer_mut().set_stringn(x, y, s, room, style);
}

/// Background from the colormap, foreground for contrast, with selection and the
/// cursor layered on top.
fn cell_style(bg: Option<Rgb>, selected: bool, is_cursor: bool) -> Style {
    let mut style = match bg {
        Some(c) => Style::new()
            .bg(Color::Rgb(c.r, c.g, c.b))
            .fg(to_color(color::fg_for_bg(c))),
        // Colormap::None paints nothing, so the terminal's own colours show.
        None => Style::new(),
    };
    if selected {
        style = style.bg(SELECTED_BG).fg(Color::White);
    }
    if is_cursor {
        style = style.add_modifier(ratatui::style::Modifier::REVERSED);
    }
    style
}

fn to_color(c: Rgb) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

fn draw_status(frame: &mut Frame, app: &TuiApp, area: Rect) {
    // The jump prompt owns the status line while it is open.
    if let Some(input) = &app.jump {
        let text = format!(" jump to offset: {input}_");
        frame.buffer_mut().set_stringn(
            area.x,
            area.y,
            text,
            area.width as usize,
            Style::new().fg(FOCUSED),
        );
        return;
    }
    let data = app.data.as_slice();
    let byte = data.get(app.cursor).copied().unwrap_or(0);
    let h = geom::entropy_at(&app.entropies, app.entropy_window, app.cursor);
    let mut line = format!(
        " 0x{:08X} · 0x{byte:02X} '{}' · H={h:.3} · win {} B",
        app.cursor,
        color::printable(byte),
        app.entropy_window
    );
    if let Some(sel) = &app.selection {
        let _ = write!(
            line,
            " · sel 0x{:X}–0x{:X} ({} B)",
            sel.start,
            sel.end.saturating_sub(1),
            sel.len()
        );
    }
    if app.entropy_computing {
        line.push_str(" · computing entropy…");
    }
    if let Some(m) = &app.message {
        line.push_str(" · ");
        line.push_str(m);
    }
    frame.buffer_mut().set_stringn(
        area.x,
        area.y,
        line,
        area.width as usize,
        Style::new().fg(MUTED),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(app: &mut TuiApp, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let mut term = Terminal::new(TestBackend::new(w, h)).expect("test backend");
        term.draw(|f| draw(f, app)).expect("draw");
        term.backend().buffer().clone()
    }

    fn row_text(buf: &ratatui::buffer::Buffer, y: u16, w: u16) -> String {
        (0..w).map(|x| buf.get(x, y).symbol().to_owned()).collect()
    }

    #[test]
    fn the_hex_column_renders_an_address_and_records_the_layout() {
        let mut app = TuiApp::for_test(4096);
        let buf = render(&mut app, 120, 24);
        // Row 1 is the first content row (row 0 is the border).
        let row = row_text(&buf, 1, 120);
        assert!(row.contains("00000000"), "no address in {row:?}");
        // The renderer must publish its measurements for the input layer.
        assert!(app.layout.hex_cols > 0);
        assert!(app.layout.text_rows > 0);
        assert!(app.layout.overview_cols > 0);
        assert!(app.layout.zoom_cols > 0);
    }

    #[test]
    fn all_three_headers_name_their_colormap() {
        let mut app = TuiApp::for_test(4096);
        let buf = render(&mut app, 140, 12);
        let top = row_text(&buf, 0, 140);
        assert!(top.contains("Overview"), "{top:?}");
        assert!(top.contains("Zoom"), "{top:?}");
        assert!(top.contains("Hex"), "{top:?}");
        // Each column advertises its own colormap, as the gpui headers do.
        assert!(top.contains("Entropy"), "{top:?}");
        assert!(top.contains("Value"), "{top:?}");
        assert!(top.contains("Class"), "{top:?}");
    }

    #[test]
    fn the_status_line_reads_out_the_cursor() {
        let mut app = TuiApp::for_test(4096);
        app.cursor = 0x20;
        let buf = render(&mut app, 120, 10);
        let status = row_text(&buf, 9, 120);
        assert!(status.contains("0x00000020"), "{status:?}");
    }

    #[test]
    fn the_jump_prompt_takes_over_the_status_line() {
        let mut app = TuiApp::for_test(4096);
        app.jump = Some("0xFF".to_owned());
        let buf = render(&mut app, 120, 10);
        let status = row_text(&buf, 9, 120);
        assert!(status.contains("jump to offset: 0xFF"), "{status:?}");
    }

    #[test]
    fn a_terminal_too_narrow_still_renders() {
        let mut app = TuiApp::for_test(4096);
        // Every column is squeezed below its content width; must not panic.
        render(&mut app, 20, 6);
        render(&mut app, 8, 3);
    }

    #[test]
    fn the_graphical_columns_use_half_blocks() {
        let mut app = TuiApp::for_test(65536);
        let buf = render(&mut app, 140, 20);
        // The overview's interior starts at x=1,y=1 inside its border.
        assert_eq!(buf.get(1, 1).symbol(), "▀");
    }

    /// The x range of the hex column's interior, derived from the recorded layout
    /// rather than hardcoding the percentage split. Scanning a whole row would
    /// also pick up the overview and zoom columns, which have colormaps of their
    /// own.
    fn hex_xs(app: &TuiApp, w: u16) -> std::ops::Range<u16> {
        let width = u16::try_from(app.layout.hex_cols).expect("fits in u16");
        (w - width - 1)..(w - 1)
    }

    #[test]
    fn a_selection_is_tinted_in_the_hex_column() {
        let mut app = TuiApp::for_test(4096);
        let plain = render(&mut app, 140, 12);
        let xs = hex_xs(&app, 140);
        let before = xs
            .clone()
            .filter(|&x| plain.get(x, 1).bg == SELECTED_BG)
            .count();

        app.selection = Some(0..4);
        let buf = render(&mut app, 140, 12);
        let tinted = xs.filter(|&x| buf.get(x, 1).bg == SELECTED_BG).count();

        assert_eq!(before, 0, "nothing tinted without a selection");
        // Four selected bytes: two digit cells plus one ascii cell each.
        assert_eq!(tinted, 4 * 3, "expected 12 tinted cells, got {tinted}");
    }

    #[test]
    fn none_colormap_leaves_the_hex_cells_unstyled() {
        let mut app = TuiApp::for_test(4096);
        app.colormaps[Focus::Hex as usize] = crate::core::color::Colormap::None;
        let buf = render(&mut app, 140, 12);
        // Muting a panel must leave the terminal's own colours rather than paint
        // black -- the same meaning Colormap::None carries on the gpui side.
        let styled = hex_xs(&app, 140)
            .filter(|&x| buf.get(x, 1).bg != Color::Reset)
            .count();
        assert_eq!(
            styled, 0,
            "{styled} hex cells had a background under Colormap::None"
        );
    }
}
