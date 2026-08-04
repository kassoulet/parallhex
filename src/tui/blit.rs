//! Turning RGBA buffers into terminal cells, and the clipboard escape.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::core::color::Rgb;

/// The upper-half block. `fg` paints its top half, `bg` the bottom, which is how
/// one text row carries two byte rows.
const HALF: char = '▀';

/// Paint an RGBA buffer `w` pixels wide into `area`, two pixel rows per text row.
///
/// Alpha 0 becomes `Color::Reset`, which is how `Colormap::None` leaves a panel
/// muted rather than blank — the same meaning transparent pixels carry on the
/// gpui side.
pub(crate) fn blit_half_blocks(buf: &mut Buffer, area: Rect, rgba: &[u8], w: usize) {
    for cy in 0..area.height {
        for cx in 0..area.width {
            let px = cx as usize;
            if px >= w {
                break;
            }
            let top = pixel(rgba, w, px, cy as usize * 2);
            let bottom = pixel(rgba, w, px, cy as usize * 2 + 1);
            let cell = buf.get_mut(area.x + cx, area.y + cy);
            cell.set_char(HALF);
            cell.set_fg(to_color(top));
            cell.set_bg(to_color(bottom));
        }
    }
}

/// The pixel at `(x, y)`, or `None` when it is transparent or absent.
///
/// Absence is not an error: an odd pixel height leaves the last cell without a
/// lower pixel, so the read is bounds-checked rather than assumed.
fn pixel(rgba: &[u8], w: usize, x: usize, y: usize) -> Option<Rgb> {
    let i = (y * w + x) * 4;
    if *rgba.get(i + 3)? == 0 {
        return None;
    }
    Some(Rgb::new(rgba[i], rgba[i + 1], rgba[i + 2]))
}

fn to_color(px: Option<Rgb>) -> Color {
    match px {
        Some(c) => Color::Rgb(c.r, c.g, c.b),
        None => Color::Reset,
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding. Hand-rolled because OSC 52 is the only place
/// this crate needs it, and a dependency for twenty lines is a poor trade.
pub(crate) fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[(n >> 18 & 63) as usize] as char);
        out.push(B64[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// An OSC 52 sequence asking the terminal to set the system clipboard.
///
/// Chosen over a clipboard crate because it works through ssh and tmux, which is
/// the situation a terminal frontend exists for. The trade-off is real: some
/// terminals disable OSC 52, and the write cannot be confirmed, so callers report
/// success optimistically.
pub(crate) fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b"hello"), "aGVsbG8=");
        assert_eq!(base64(b"DE AD BE EF"), "REUgQUQgQkUgRUY=");
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
    }

    #[test]
    fn osc52_wraps_the_payload() {
        assert_eq!(osc52("hello"), "\x1b]52;c;aGVsbG8=\x07");
    }

    #[test]
    fn half_blocks_take_fg_from_top_and_bg_from_bottom() {
        // One column, two pixel rows: red over blue -> one cell.
        let rgba = vec![255, 0, 0, 255, 0, 0, 255, 255];
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        blit_half_blocks(&mut buf, Rect::new(0, 0, 1, 1), &rgba, 1);
        let cell = buf.get(0, 0);
        assert_eq!(cell.symbol(), "▀");
        assert_eq!(cell.fg, Color::Rgb(255, 0, 0));
        assert_eq!(cell.bg, Color::Rgb(0, 0, 255));
    }

    #[test]
    fn transparent_pixels_become_reset_so_none_mutes_a_panel() {
        // Alpha 0 on top, opaque green below.
        let rgba = vec![9, 9, 9, 0, 0, 255, 0, 255];
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        blit_half_blocks(&mut buf, Rect::new(0, 0, 1, 1), &rgba, 1);
        assert_eq!(buf.get(0, 0).fg, Color::Reset);
        assert_eq!(buf.get(0, 0).bg, Color::Rgb(0, 255, 0));
    }

    #[test]
    fn missing_bottom_row_is_reset_not_a_panic() {
        // Odd pixel height: the last cell has no lower pixel.
        let rgba = vec![255, 0, 0, 255];
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        blit_half_blocks(&mut buf, Rect::new(0, 0, 1, 1), &rgba, 1);
        assert_eq!(buf.get(0, 0).bg, Color::Reset);
    }

    #[test]
    fn cells_beyond_the_buffer_width_are_left_alone() {
        // A 1-pixel-wide buffer into a 3-cell area must not read past its row.
        let rgba = vec![255, 0, 0, 255, 0, 0, 255, 255];
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
        blit_half_blocks(&mut buf, Rect::new(0, 0, 3, 1), &rgba, 1);
        assert_eq!(buf.get(0, 0).symbol(), "▀");
        assert_eq!(buf.get(1, 0).symbol(), " ");
        assert_eq!(buf.get(2, 0).symbol(), " ");
    }

    #[test]
    fn a_second_row_of_cells_reads_the_third_and_fourth_pixel_rows() {
        // 1 column x 4 pixel rows -> 2 cells stacked.
        let rgba = vec![
            1, 1, 1, 255, // cell 0 fg
            2, 2, 2, 255, // cell 0 bg
            3, 3, 3, 255, // cell 1 fg
            4, 4, 4, 255, // cell 1 bg
        ];
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 2));
        blit_half_blocks(&mut buf, Rect::new(0, 0, 1, 2), &rgba, 1);
        assert_eq!(buf.get(0, 0).fg, Color::Rgb(1, 1, 1));
        assert_eq!(buf.get(0, 0).bg, Color::Rgb(2, 2, 2));
        assert_eq!(buf.get(0, 1).fg, Color::Rgb(3, 3, 3));
        assert_eq!(buf.get(0, 1).bg, Color::Rgb(4, 4, 4));
    }
}
