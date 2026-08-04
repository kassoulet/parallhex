//! Byte-to-color mappings and small display helpers.

/// A plain 8-bit colour, deliberately toolkit-neutral so that every frontend can
/// consume it: the gpui side converts to `gpui::Rgba` at its painting boundary,
/// a terminal frontend to its own colour type. 8-bit rather than float because
/// both destinations ultimately want bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Split a packed `0xRRGGBB` literal, which is how this palette is written.
    pub const fn from_hex(v: u32) -> Self {
        Self {
            r: (v >> 16) as u8,
            g: (v >> 8) as u8,
            b: v as u8,
        }
    }
}

/// Per-byte category color (binvis.io "byte class" palette).
pub fn class_color(b: u8) -> Rgb {
    match b {
        0x00 => Rgb::new(0, 0, 0),                     // Null
        0x01..=0x1F | 0x7F => Rgb::from_hex(0x17becf), // Control
        0x20..=0x7E => Rgb::from_hex(0x1f77b4),        // Printable ASCII
        0x80..=0xFE => Rgb::from_hex(0xff7f0e),        // High / non-ASCII
        0xFF => Rgb::new(255, 255, 255),               // Fill / padded
    }
}

/// Foreground text color with sufficient contrast against any cell background.
/// The threshold is on the 0–255 scale, matching `Rgb`'s channels.
pub fn fg_for_bg(bg: Rgb) -> Rgb {
    let luma = 0.299 * f32::from(bg.r) + 0.587 * f32::from(bg.g) + 0.114 * f32::from(bg.b);
    if luma > 140.0 {
        Rgb::from_hex(0x0f0f0f)
    } else {
        Rgb::new(255, 255, 255)
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
pub fn entropy_color(h: f32) -> Rgb {
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
    Rgb::new(
        lerp(lo.1.0, hi.1.0),
        lerp(lo.1.1, hi.1.1),
        lerp(lo.1.2, hi.1.2),
    )
}

/// The colormap a panel uses to color each byte. Every panel picks its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Colormap {
    /// No colormap: nothing is painted for the byte.
    None,
    /// Byte value mapped to greyscale brightness.
    Value,
    /// The binvis.io byte-class palette.
    Class,
    /// Shannon entropy gradient.
    Entropy,
}

impl Colormap {
    /// Every available colormap, in display order.
    pub const ALL: [Colormap; 4] = [
        Colormap::None,
        Colormap::Value,
        Colormap::Class,
        Colormap::Entropy,
    ];

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Colormap::None => "None",
            Colormap::Value => "Value",
            Colormap::Class => "Class",
            Colormap::Entropy => "Entropy",
        }
    }

    /// Config-file key.
    pub fn key(self) -> &'static str {
        match self {
            Colormap::None => "none",
            Colormap::Value => "value",
            Colormap::Class => "class",
            Colormap::Entropy => "entropy",
        }
    }

    /// Parse a config-file key back into a colormap.
    pub fn from_key(s: &str) -> Option<Colormap> {
        Colormap::ALL.iter().copied().find(|c| c.key() == s)
    }

    /// Whether this colormap's output depends on the per-byte entropy value.
    /// Only `Entropy` does; callers can skip the (per-byte, interpolating)
    /// `entropy_at` lookup entirely for the others. Route per-byte lookups
    /// through `panes::entropy_for` so they honor this gate.
    pub fn uses_entropy(self) -> bool {
        matches!(self, Colormap::Entropy)
    }

    /// Color a single byte under this colormap, or `None` when this colormap
    /// paints nothing — callers skip drawing entirely rather than filling.
    pub fn color_for(self, b: u8, entropy: f32) -> Option<Rgb> {
        match self {
            Colormap::None => Option::None,
            Colormap::Value => Some(Rgb::new(b, b, b)),
            Colormap::Class => Some(class_color(b)),
            Colormap::Entropy => Some(entropy_color(entropy)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_from_hex_splits_channels() {
        let c = Rgb::from_hex(0x17becf);
        assert_eq!((c.r, c.g, c.b), (0x17, 0xbe, 0xcf));
    }

    #[test]
    fn class_colors_are_plain_rgb() {
        assert_eq!(class_color(0x00), Rgb::new(0, 0, 0));
        assert_eq!(class_color(0xFF), Rgb::new(255, 255, 255));
        assert_eq!(class_color(0x41), Rgb::from_hex(0x1f77b4));
    }

    #[test]
    fn colormap_keys_round_trip() {
        for cm in Colormap::ALL {
            assert_eq!(Colormap::from_key(cm.key()), Some(cm), "{cm:?}");
        }
        assert_eq!(Colormap::ALL.len(), 4);
        assert_eq!(Colormap::from_key("greyscale"), None); // retired key
        assert_eq!(Colormap::from_key("byte_class"), None); // retired key
        assert_eq!(Colormap::from_key(""), None);
    }

    #[test]
    fn none_colormap_paints_nothing() {
        assert_eq!(Colormap::None.color_for(0x41, 4.0), None);
        assert!(Colormap::Value.color_for(0x41, 4.0).is_some());
        assert!(Colormap::Class.color_for(0x41, 4.0).is_some());
        assert!(Colormap::Entropy.color_for(0x41, 4.0).is_some());
    }

    #[test]
    fn value_colormap_is_byte_brightness() {
        let c = Colormap::Value.color_for(0x80, 0.0).expect("value paints");
        assert_eq!(c, Rgb::new(0x80, 0x80, 0x80));
    }

    #[test]
    fn fg_contrast_flips_on_light_backgrounds() {
        // Dark glyphs on a light cell, light glyphs on a dark one. Guards the
        // luma threshold's move from the 0..1 scale to 0..255.
        assert!(fg_for_bg(Rgb::new(255, 255, 255)).r < 128);
        assert!(fg_for_bg(Rgb::new(0, 0, 0)).r > 128);
        // Mid-grey sits just below the threshold, so glyphs stay light.
        assert!(fg_for_bg(Rgb::new(128, 128, 128)).r > 128);
        // A bright colour crosses it, so glyphs go dark.
        assert!(fg_for_bg(Rgb::from_hex(0xffffff)).r < 128);
    }
}
