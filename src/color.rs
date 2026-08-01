//! Byte-to-color mappings and small display helpers.

use eframe::egui::Color32;

/// Per-byte category color (binvis.io "byte class" palette).
pub fn class_color(b: u8) -> Color32 {
    match b {
        0x00 => Color32::BLACK,                                   // Null
        0x01..=0x1F | 0x7F => Color32::from_rgb(0x17, 0xbe, 0xcf), // Control
        0x20..=0x7E => Color32::from_rgb(0x1f, 0x77, 0xb4),        // Printable ASCII
        0x80..=0xFE => Color32::from_rgb(0xff, 0x7f, 0x0e),        // High / non-ASCII
        0xFF => Color32::WHITE,                                    // Fill / padded
    }
}

/// Foreground text color with sufficient contrast against a class background.
pub fn fg_for_class(bg: Color32) -> Color32 {
    let luma = 0.299 * bg.r() as f32 + 0.587 * bg.g() as f32 + 0.114 * bg.b() as f32;
    if luma > 140.0 {
        Color32::from_gray(15)
    } else {
        Color32::WHITE
    }
}

/// Printable representation of a byte for ASCII dump columns.
pub fn printable(b: u8) -> char {
    if (0x20..=0x7E).contains(&b) {
        b as char
    } else {
        '.'
    }
}

/// Gradient stops for entropy `H` in `[0, 8]` bits per byte:
/// low -> deep purple/black, mid -> green/cyan, high -> red/yellow.
const STOPS: [(f32, (u8, u8, u8)); 5] = [
    (0.0, (12, 0, 40)),
    (2.0, (30, 60, 160)),
    (4.0, (0, 200, 160)),
    (6.0, (230, 210, 30)),
    (8.0, (255, 60, 40)),
];

/// Map Shannon entropy `h in [0, 8]` onto a color gradient.
pub fn entropy_color(h: f32) -> Color32 {
    let h = h.clamp(0.0, 8.0);
    let mut lo = STOPS[0];
    let mut hi = STOPS[0];
    for &stop in &STOPS {
        if stop.0 <= h {
            lo = stop;
        }
        if stop.0 >= h {
            hi = stop;
            break;
        }
    }
    let t = if hi.0 == lo.0 {
        0.0
    } else {
        (h - lo.0) / (hi.0 - lo.0)
    };
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgb(
        lerp(lo.1 .0, hi.1 .0),
        lerp(lo.1 .1, hi.1 .1),
        lerp(lo.1 .2, hi.1 .2),
    )
}

/// Human readable byte count.
pub fn human_size(bytes: usize) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_mapping() {
        assert_eq!(class_color(0x00), Color32::BLACK);
        assert_eq!(class_color(0x41), Color32::from_rgb(0x1f, 0x77, 0xb4));
        assert_eq!(class_color(0x09), Color32::from_rgb(0x17, 0xbe, 0xcf));
        assert_eq!(class_color(0xE4), Color32::from_rgb(0xff, 0x7f, 0x0e));
        assert_eq!(class_color(0xFF), Color32::WHITE);
    }

    #[test]
    fn printable_fallback() {
        assert_eq!(printable(0x41), 'A');
        assert_eq!(printable(0x00), '.');
        assert_eq!(printable(0xFF), '.');
    }

    #[test]
    fn entropy_gradient_bounds() {
        let c0 = entropy_color(0.0);
        let c8 = entropy_color(8.0);
        assert_ne!(c0, c8);
        assert_eq!(entropy_color(-1.0), entropy_color(0.0));
        assert_eq!(entropy_color(9.0), entropy_color(8.0));
    }
}
