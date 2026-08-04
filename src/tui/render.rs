//! Drawing the three columns.
//!
//! The two graphical columns reuse the same RGBA generators the gpui frontend
//! uses, blitted as half-blocks. The hex column reuses `RowGeo` at `char_w = 1.0`
//! with no gutter, so its cell positions and 8-byte group gaps need no
//! terminal-specific arithmetic.

use std::fmt::Write as _;
use std::ops::Range;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::Block;

use crate::core::color::{self, Colormap, Rgb};
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
    let [body, status, hints] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.size());
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

    // Each column marks where the *next* one is looking, so the overview tracks
    // your position even though it cannot scroll -- it always shows the whole
    // file. Same relationship the gpui frontend draws as a translucent band.
    let len = app.file_size;
    let zoom_view = visible_range(app, Focus::Zoom, zoom_inner);
    let hex_view = visible_range(app, Focus::Hex, hex_inner);
    draw_band(frame, overview, ov_inner, &(0..len), &zoom_view);
    draw_band(frame, zoom, zoom_inner, &zoom_view, &hex_view);

    draw_status(frame, app, status);
    draw_hints(frame, app, hints);
}

/// The byte range a column is currently showing.
fn visible_range(app: &TuiApp, focus: Focus, inner: Rect) -> Range<usize> {
    let len = app.file_size;
    match focus {
        Focus::Overview => 0..len,
        Focus::Zoom => {
            let cols = (inner.width as usize).max(1);
            // Two byte rows per text row, because of the half-block packing.
            let rows = inner.height as usize * 2;
            let first = geom::first_row_centred(app.anchor, cols, rows);
            first..(first + rows * cols).min(len)
        }
        Focus::Hex => {
            let bpr = app.bpr_for(Focus::Hex).max(8);
            let rows = inner.height as usize;
            let first = geom::first_row_centred(app.anchor, bpr, rows);
            first..(first + rows * bpr).min(len)
        }
    }
}

/// Mark `mark`'s share of `panel`'s range on the block's right-hand border.
///
/// The border rather than the interior: a terminal cannot overlay a translucent
/// band the way gpui does, and tinting data cells would destroy the very colours
/// the column exists to show.
fn draw_band(
    frame: &mut Frame,
    block: Rect,
    inner: Rect,
    panel: &Range<usize>,
    mark: &Range<usize>,
) {
    let Some(rows) = band_rows(inner.height, panel, mark) else {
        return;
    };
    // The right border column of the block.
    let x = block.x + block.width.saturating_sub(1);
    for y in rows {
        let cell = frame.buffer_mut().get_mut(x, inner.y + y);
        cell.set_char('┃');
        cell.set_fg(FOCUSED);
    }
}

/// Which of `height` text rows `mark` covers, as a fraction of `panel`'s range.
///
/// Returns `None` when there is nothing to draw. Always at least one row when the
/// mark is non-empty, so a small range still shows.
fn band_rows(height: u16, panel: &Range<usize>, mark: &Range<usize>) -> Option<Range<u16>> {
    if height == 0 || mark.is_empty() || panel.is_empty() {
        return None;
    }
    // A mark entirely outside the panel's range has nothing to mark.
    if mark.end <= panel.start || mark.start >= panel.end {
        return None;
    }
    let span = (panel.end - panel.start) as f32;
    let frac = |off: usize| {
        (off.saturating_sub(panel.start) as f32 / span).clamp(0.0, 1.0) * f32::from(height)
    };
    let top = frac(mark.start) as u16;
    let bottom = (frac(mark.end).ceil() as u16).min(height);
    Some(top..bottom.max(top + 1).min(height))
}

/// The key-hint row. Its main job is the colormap keys: `1`–`4` are otherwise
/// undiscoverable, and showing them numbered with the focused panel's current
/// choice highlighted makes the row double as state rather than just a legend.
fn draw_hints(frame: &mut Frame, app: &TuiApp, area: Rect) {
    if area.height == 0 {
        return;
    }
    let mut x = area.x;
    let end = area.x + area.width;
    let mut put = |s: &str, style: Style| {
        if x >= end {
            return;
        }
        let room = (end - x) as usize;
        frame.buffer_mut().set_stringn(x, area.y, s, room, style);
        // Width in cells, not bytes: the labels are ASCII, but `·` is not.
        x = x.saturating_add(u16::try_from(s.chars().count()).unwrap_or(u16::MAX));
    };

    put(
        &format!(" {} colormap:", app.focus.title()),
        Style::new().fg(MUTED),
    );
    let current = app.colormap(app.focus);
    for (i, cm) in Colormap::ALL.iter().enumerate() {
        let style = if *cm == current {
            // Highlighted rather than merely listed, so the row shows which
            // colormap is active as well as how to change it.
            Style::new().fg(Color::Black).bg(FOCUSED)
        } else {
            Style::new().fg(MUTED)
        };
        put(&format!(" {} {} ", i + 1, cm.label()), style);
    }
    put(
        "  Tab panel · g jump · y copy · -/+ window · q quit",
        Style::new().fg(MUTED),
    );
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
        let status = row_text(&buf, 8, 120);
        assert!(status.contains("0x00000020"), "{status:?}");
    }

    #[test]
    fn the_jump_prompt_takes_over_the_status_line() {
        let mut app = TuiApp::for_test(4096);
        app.jump = Some("0xFF".to_owned());
        let buf = render(&mut app, 120, 10);
        let status = row_text(&buf, 8, 120);
        assert!(status.contains("jump to offset: 0xFF"), "{status:?}");
    }

    #[test]
    fn the_hint_row_teaches_the_colormap_keys_and_marks_the_active_one() {
        let mut app = TuiApp::for_test(4096);
        app.focus = Focus::Hex; // default Class
        let buf = render(&mut app, 140, 12);
        let hints = row_text(&buf, 11, 140);
        // Every colormap must be listed with its number, or the binding stays
        // undiscoverable.
        for (i, cm) in Colormap::ALL.iter().enumerate() {
            let want = format!("{} {}", i + 1, cm.label());
            assert!(hints.contains(&want), "hint row lacks {want:?}: {hints:?}");
        }
        assert!(hints.contains("Hex colormap:"), "{hints:?}");

        // The active one is highlighted, so the row shows state too. Class is 3rd.
        let x = hints.find("3 Class").expect("3 Class present");
        let cell = buf.get(u16::try_from(x).unwrap(), 11);
        assert_eq!(
            cell.bg, FOCUSED,
            "the active colormap should be highlighted"
        );
        let other = hints.find("2 Value").expect("2 Value present");
        assert_ne!(buf.get(u16::try_from(other).unwrap(), 11).bg, FOCUSED);
    }

    #[test]
    fn the_hint_row_follows_the_focused_panel() {
        let mut app = TuiApp::for_test(4096);
        app.focus = Focus::Overview; // default Entropy
        let buf = render(&mut app, 140, 12);
        let hints = row_text(&buf, 11, 140);
        assert!(hints.contains("Overview colormap:"), "{hints:?}");
        let x = hints.find("4 Entropy").expect("4 Entropy present");
        assert_eq!(buf.get(u16::try_from(x).unwrap(), 11).bg, FOCUSED);
    }

    #[test]
    fn band_rows_maps_a_range_onto_text_rows() {
        // The whole panel covers every row.
        assert_eq!(band_rows(10, &(0..1000), &(0..1000)), Some(0..10));
        // The first tenth covers the first row.
        assert_eq!(band_rows(10, &(0..1000), &(0..100)), Some(0..1));
        // The middle fifth lands in the middle.
        assert_eq!(band_rows(10, &(0..1000), &(400..600)), Some(4..6));
        // A tiny mark still shows as one row rather than vanishing.
        assert_eq!(band_rows(10, &(0..1000), &(500..501)), Some(5..6));
        // Nothing to draw.
        assert_eq!(band_rows(0, &(0..1000), &(0..10)), None);
        assert_eq!(band_rows(10, &(0..1000), &(5..5)), None);
        assert_eq!(band_rows(10, &(0..0), &(0..10)), None);
        // A mark outside the panel's range is not clamped into view.
        assert_eq!(band_rows(10, &(500..600), &(0..100)), None);
        assert_eq!(band_rows(10, &(0..100), &(200..300)), None);
    }

    #[test]
    fn the_overview_marks_where_the_zoom_column_is_looking() {
        let mut app = TuiApp::for_test(1 << 20);
        let buf = render(&mut app, 140, 14);
        // The band lives on the overview block's right border.
        let x = u16::try_from(app.layout.overview_cols + 1).expect("fits");
        let marked: Vec<u16> = (1..12).filter(|&y| buf.get(x, y).symbol() == "┃").collect();
        assert!(!marked.is_empty(), "no band drawn on the overview border");

        // Moving the anchor to the end moves the band down.
        app.apply(crate::tui::app::Action::Move(crate::core::geom::Nav::End));
        let buf = render(&mut app, 140, 14);
        let moved: Vec<u16> = (1..12).filter(|&y| buf.get(x, y).symbol() == "┃").collect();
        assert!(!moved.is_empty(), "band vanished after seeking to the end");
        assert!(
            moved[0] > marked[0],
            "band should move down: {marked:?} -> {moved:?}"
        );
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
