//! Byte-to-color mappings and small display helpers.

use gpui::{Rgba, rgb};

/// Per-byte category color (binvis.io "byte class" palette).
pub fn class_color(b: u8) -> Rgba {
    match b {
        0x00 => rgb(0x000000),               // Null
        0x01..=0x1F | 0x7F => rgb(0x17becf), // Control
        0x20..=0x7E => rgb(0x1f77b4),        // Printable ASCII
        0x80..=0xFE => rgb(0xff7f0e),        // High / non-ASCII
        0xFF => rgb(0xffffff),               // Fill / padded
    }
}

/// Foreground text color with sufficient contrast against a class background.
pub fn fg_for_class(bg: Rgba) -> Rgba {
    let luma = 0.299 * bg.r + 0.587 * bg.g + 0.114 * bg.b;
    if luma > 140.0 / 255.0 {
        rgb(0x0f0f0f)
    } else {
        rgb(0xffffff)
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
pub fn entropy_color(h: f32) -> Rgba {
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
    let lerp = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    rgb(u32::from(lerp(lo.1.0, hi.1.0)) << 16
        | u32::from(lerp(lo.1.1, hi.1.1)) << 8
        | u32::from(lerp(lo.1.2, hi.1.2)))
}

/// The colormap used by the pixels column and the whole-file overview:
/// each byte is rendered with exactly one of these mappings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Colormap {
    /// Byte value mapped to greyscale brightness.
    Greyscale,
    /// Shannon entropy gradient.
    Entropy,
    /// The binvis.io byte-class palette.
    ByteClass,
}

impl Colormap {
    /// Every available colormap, in display order.
    pub const ALL: [Colormap; 3] = [Colormap::Greyscale, Colormap::Entropy, Colormap::ByteClass];

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Colormap::Greyscale => "Greyscale",
            Colormap::Entropy => "Entropy",
            Colormap::ByteClass => "Byte class",
        }
    }

    /// Config-file key.
    pub fn key(self) -> &'static str {
        match self {
            Colormap::Greyscale => "greyscale",
            Colormap::Entropy => "entropy",
            Colormap::ByteClass => "byte_class",
        }
    }

    /// Parse a config-file key back into a colormap.
    pub fn from_key(s: &str) -> Option<Colormap> {
        Colormap::ALL.iter().copied().find(|c| c.key() == s)
    }

    /// Color a single byte under this colormap.
    pub fn color_for(self, b: u8, entropy: f32) -> Rgba {
        match self {
            Colormap::Greyscale => Rgba {
                r: f32::from(b) / 255.0,
                g: f32::from(b) / 255.0,
                b: f32::from(b) / 255.0,
                a: 1.0,
            },
            Colormap::Entropy => entropy_color(entropy),
            Colormap::ByteClass => class_color(b),
        }
    }
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
