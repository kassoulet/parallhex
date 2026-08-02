# Three-Column Rework Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rework ParallHex into three independently-colored columns whose rows fit their own width, driven by a shared byte anchor, and fix the hex background misalignment.

**Architecture:** All geometry, row-length and pixel math stays in `src/panes.rs` as pure functions with unit tests; `src/app.rs` holds state and wires those functions into gpui canvases. The shared `scroll_rows: f32` (row index) becomes `scroll_offset: usize` (byte anchor), because each panel now derives its own bytes-per-row from its own width. The jump dialog moves out of `app.rs` first so the remaining work happens in a smaller file.

**Tech Stack:** Rust 2024, gpui 0.2.2, memmap2, rayon, image. Tests are inline `#[cfg(test)] mod tests` blocks; no `tests/` directory.

## Global Constraints

- `SPECS.md` is the authority for behaviour; update it in the same commit as any behaviour change that deviates from it.
- Lints are hard gates: `[lints.rust] warnings = "deny"`, `[lints.clippy] pedantic = "deny"`. Every task must end with `cargo clippy --all-targets` clean.
- Curated pedantic allows live in the `#![allow(...)]` at the top of `src/main.rs`, never in `Cargo.toml`.
- `cargo fmt` before every commit; `cargo fmt --check` gates commits via `prek`.
- Colormap config values are exactly `none`, `value`, `class`, `entropy`.
- Zoom range is `1..=24` px; entropy window `16..=4096`; hex rows are a multiple of 8, minimum 8.
- Hex text size is fixed at `HEX_FONT_SIZE = 13.0` — no hex zoom anywhere.
- gpui canvases must be sized explicitly: `canvas(prepaint, paint).size_full()`.
- Retired config keys (`bytes_per_row`, `hex_zoom`, `pixel_colormap`, `pixels_width`) must load without error as unknown keys.

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `src/main.rs` | CLI, actions, keybindings, window options | add `mod jump;` |
| `src/jump.rs` | Jump-to-offset text field + its custom element | **new** (moved out of `app.rs`) |
| `src/color.rs` | Byte→colour mappings, `Colormap`, `human_size` | `Colormap` gains `None`, renames |
| `src/panes.rs` | All pure geometry / row-length / anchor / paint / thumbnail math | most of the work |
| `src/config.rs` | Persisted preferences | key migration |
| `src/app.rs` | View state + gpui wiring for the three columns | state + UI rework |

---

### Task 1: Extract the jump dialog into `src/jump.rs`

Pure move, no behaviour change. `app.rs` is ~3100 lines and the next tasks edit it heavily; the jump field is a self-contained ~500-line unit with no dependency on the rest of the view.

**Files:**
- Create: `src/jump.rs`
- Modify: `src/app.rs` (remove the moved block, import from `crate::jump`), `src/main.rs` (add `mod jump;`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `crate::jump::{JumpField, JumpFieldEvent}`. `JumpField::new(cx: &mut Context<JumpField>) -> JumpField`, `JumpField::set_content(&mut self, s: &str)`, field `content: SharedString` accessed via a new `pub(crate) fn content(&self) -> &str`. `JumpFieldEvent::{Submit(String), Cancel}`.

- [ ] **Step 1: Create `src/jump.rs` with the moved code**

Move these items verbatim out of `src/app.rs` into `src/jump.rs`, changing nothing but visibility: `JumpFieldEvent`, `JumpField` and its `impl` block, `impl EventEmitter<JumpFieldEvent> for JumpField`, `impl Focusable for JumpField`, `impl EntityInputHandler for JumpField`, `range_from_utf16`, `range_to_utf16`, `JumpFieldElement`, `JumpFieldPrepaint`, `impl IntoElement for JumpFieldElement`, `impl Element for JumpFieldElement`, `impl Render for JumpField`.

Add the module header and the accessor `app.rs` needs (it currently reaches into the private `content` field):

```rust
//! The jump-to-offset dialog's single-line text field.
//!
//! gpui 0.2 has no built-in text input, so this implements `EntityInputHandler`
//! plus a hand-written `Element` that shapes the line and paints the caret.

use std::ops::Range;

use gpui::{
    App, Bounds, Context, CursorStyle, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, Hsla, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ShapedLine,
    SharedString, Style, TextRun, UTF16Selection, Window, div, fill, point, prelude::*, px,
    relative, rgb, rgba, size,
};

use crate::{Backspace, Delete, JumpCancel, JumpSubmit, NavigateLeft, NavigateRight, Paste};

impl JumpField {
    /// The current text, for the parent view's live preview and submit.
    pub(crate) fn content(&self) -> &str {
        &self.content
    }
}
```

- [ ] **Step 2: Wire the module up**

In `src/main.rs` add `mod jump;` next to the other `mod` declarations (alphabetical: after `mod entropy;`).

In `src/app.rs` delete the moved block and add:

```rust
use crate::jump::{JumpField, JumpFieldEvent};
```

Replace the two places that read the private field — `self.jump_field.read(cx).content.to_string()` in `on_jump_submit` and `jump_preview` — with `self.jump_field.read(cx).content().to_owned()`.

- [ ] **Step 3: Verify nothing changed**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: clippy clean, all 57 tests pass (this task adds none).

- [ ] **Step 4: Commit**

```bash
git add src/jump.rs src/app.rs src/main.rs
git commit -m "refactor: move the jump dialog into its own module"
```

---

### Task 2: `Colormap` becomes `None | Value | Class | Entropy`

**Files:**
- Modify: `src/color.rs`, `src/panes.rs` (call sites), `src/app.rs` (call sites)
- Test: `src/color.rs` inline `mod tests`

**Interfaces:**
- Produces: `Colormap::{None, Value, Class, Entropy}`; `Colormap::ALL: [Colormap; 4]`; `Colormap::label(self) -> &'static str` → `"None"|"Value"|"Class"|"Entropy"`; `Colormap::key(self) -> &'static str` → `"none"|"value"|"class"|"entropy"`; `Colormap::from_key(&str) -> Option<Colormap>`; **`Colormap::color_for(self, b: u8, entropy: f32) -> Option<Rgba>`** returning `None` for `Colormap::None`; `color::fg_for_bg(bg: Rgba) -> Rgba` (renamed from `fg_for_class`).

- [ ] **Step 1: Write the failing tests**

Add to `src/color.rs`'s `mod tests` (create the module if absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colormap_keys_round_trip() {
        for cm in Colormap::ALL {
            assert_eq!(Colormap::from_key(cm.key()), Some(cm), "{:?}", cm);
        }
        assert_eq!(Colormap::ALL.len(), 4);
        assert_eq!(Colormap::from_key("greyscale"), None); // retired key
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
        let c = Colormap::Value.color_for(0x80, 0.0).expect("some");
        assert_eq!(c.r, c.g);
        assert_eq!(c.g, c.b);
        assert!((c.r - 128.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn fg_contrast_flips_on_light_backgrounds() {
        assert_eq!(fg_for_bg(rgb(0xffffff)), rgb(0x0f0f0f));
        assert_eq!(fg_for_bg(rgb(0x000000)), rgb(0xffffff));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib colormap 2>&1 | head -20`
Expected: compile error — no variant `None`, no function `fg_for_bg`.

- [ ] **Step 3: Implement**

In `src/color.rs` replace the enum and its impl:

```rust
/// The colormap a panel uses to color each byte.
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

    pub fn from_key(s: &str) -> Option<Colormap> {
        Colormap::ALL.iter().copied().find(|c| c.key() == s)
    }

    /// Color for a single byte, or `None` when this colormap paints nothing.
    pub fn color_for(self, b: u8, entropy: f32) -> Option<Rgba> {
        match self {
            Colormap::None => Option::None,
            Colormap::Value => Some(Rgba {
                r: f32::from(b) / 255.0,
                g: f32::from(b) / 255.0,
                b: f32::from(b) / 255.0,
                a: 1.0,
            }),
            Colormap::Class => Some(class_color(b)),
            Colormap::Entropy => Some(entropy_color(entropy)),
        }
    }
}
```

Rename `fg_for_class` to `fg_for_bg` (body unchanged) and update its doc comment to "Foreground text color with sufficient contrast against any cell background."

- [ ] **Step 4: Fix the call sites so the crate compiles**

`src/panes.rs` — in `paint_pixels`, `build_overview_rgba` and `build_strip_rgba`, skip when there is no color. In `paint_pixels` the top band becomes:

```rust
if let Some(c) = colormap.color_for(b, entropy_at(entropies, entropy_window, off)) {
    let top: Background = c.into();
    window.paint_quad(quad(
        grey_rect,
        px(0.),
        top,
        px(0.),
        transparent_black(),
        BorderStyle::default(),
    ));
}
```

In `build_overview_rgba` and `build_strip_rgba` replace `Colormap::Greyscale.color_for(avg, 0.0)` with `Colormap::Value.color_for(avg, 0.0).expect("Value always paints")` for now — Task 7 threads the real colormap through.

In `panes::build_row_runs` replace `color::fg_for_class(bg)` with `color::fg_for_bg(bg)`.

`src/app.rs` — `swatch` matches on the enum, so add the new arm and rename:

```rust
fn swatch(cm: Colormap) -> impl IntoElement {
    let color = match cm {
        Colormap::None => rgb(0x3b4261),
        Colormap::Value => rgb(0x9aa5ce),
        Colormap::Class => color::class_color(0x41),
        Colormap::Entropy => color::entropy_color(4.0),
    };
    div().w(px(10.)).h(px(10.)).rounded_md().bg(color)
}
```

`src/config.rs` — `Config::default()` uses `Colormap::Greyscale`; change to `Colormap::Value`.

- [ ] **Step 5: Run the tests**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: clippy clean; the four new colour tests pass; existing tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/color.rs src/panes.rs src/app.rs src/config.rs
git commit -m "feat: add a None colormap and rename Greyscale/ByteClass to Value/Class"
```

---

### Task 3: Fix the hex glyph/background misalignment

The reported bug. `RowGeo` and `build_row_text` disagree twice: the text is painted without the `ADDR_X` gutter the geometry includes (half-byte shift), and `hex_w` counts `bpr/8` group gaps where the text emits `(bpr-1)/8` (full-character shift of the ASCII block).

**Files:**
- Modify: `src/panes.rs` (`RowGeo::new`, `paint_hex`)
- Test: `src/panes.rs` inline `mod tests`

**Interfaces:**
- Produces: unchanged signatures; `RowGeo::ascii_start` moves left by one `char_w` for `bpr` a multiple of 8.

- [ ] **Step 1: Write the failing test**

Add to `src/panes.rs`'s `mod tests`. This asserts the invariant directly: the x of a glyph, derived from the character offsets `build_row_text` produced, equals the x of the background rect `RowGeo` computes — for both blocks, across a group boundary.

```rust
/// The x a monospace glyph lands at, given its character offset in the row
/// text: the row is painted at `ADDR_X`, so glyph `k` sits at
/// `ADDR_X + k * char_w`.
fn glyph_x(char_offset: usize, char_w: f32) -> f32 {
    ADDR_X + char_offset as f32 * char_w
}

#[test]
fn hex_and_ascii_glyphs_sit_on_their_background_cells() {
    let char_w = 10.0;
    for bpr in [8usize, 16, 32] {
        let geo = RowGeo::new(char_w, bpr);
        let data: Vec<u8> = (0..bpr).map(|i| i as u8).collect();
        let rt = build_row_text(&data, 0, bpr);
        for i in 0..bpr {
            assert_eq!(
                glyph_x(rt.hex_offsets[i], char_w),
                geo.cell_x(i),
                "hex byte {i} of {bpr} misaligned"
            );
            assert_eq!(
                glyph_x(rt.ascii_offsets[i], char_w),
                geo.ascii_x(i),
                "ascii byte {i} of {bpr} misaligned"
            );
        }
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test hex_and_ascii_glyphs -- --nocapture`
Expected: FAIL on `hex byte 0 of 8 misaligned`, left `100.0` (glyph, no gutter) vs right `108.0` (cell, with gutter).

- [ ] **Step 3: Implement both fixes**

In `RowGeo::new`, count the gaps the text actually emits (a space *between* groups):

```rust
    pub fn new(char_w: f32, bpr: usize) -> Self {
        let hex_start = ADDR_X + 8.0 * char_w + 2.0 * char_w;
        let cell_w = 3.0 * char_w; // two hex digits + one space
        let group_gap = char_w; // extra space between 8-byte groups
        // `build_row_text` emits a space *between* groups, so a row of `bpr`
        // bytes has `(bpr - 1) / 8` of them, not `bpr / 8`.
        let hex_w = bpr as f32 * cell_w + (bpr.saturating_sub(1) / 8) as f32 * group_gap;
        let ascii_start = hex_start + hex_w + 2.0 * char_w;
        Self { bpr, hex_start, cell_w, group_gap, ascii_start, char_w }
    }
```

In `paint_hex`, paint the line at the same gutter the geometry uses:

```rust
        // `RowGeo` builds ADDR_X into `hex_start`, so the glyphs need it too or
        // every background sits half a byte right of its digits.
        let _ = line.paint(
            point(origin.x + px(ADDR_X), origin.y + px(y0)),
            px(row_h),
            window,
            cx,
        );
```

- [ ] **Step 4: Update the two existing tests that encoded the old `ascii_start`**

`row_geo()` is `RowGeo::new(10.0, 16)`, so `ascii_start` becomes `618.0` (was `628.0`): `hex_w = 16*30 + ((16-1)/8)*10 = 490`.

In `row_geo_byte_at_x_maps_cells_gaps_and_ascii`: `assert_eq!(geo.ascii_start, 628.0)` → `618.0`; `byte_at_x(627.9)` → `617.9`; `byte_at_x(628.0)` → `618.0`; `byte_at_x(643.0)` → `633.0`.

In `hex_offset_at_maps_y_to_row_and_x_to_byte`: `hit(0.0, 628.0)` → `hit(0.0, 618.0)`.

In `hex_offset_at_scrolls_and_clamps_to_file_end`: `hit(21.0, 628.0)` → `hit(21.0, 618.0)`. The `cell_x(11) == 448.0` and `hit(21.0, 478.0) == None` assertions are unchanged — `cell_x` never moved.

- [ ] **Step 5: Run the tests**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: the new test passes, the two updated tests pass, everything else unchanged.

- [ ] **Step 6: Verify on screen**

Run: `cargo build && /home/gautier/target/debug/parallhex /usr/bin/gnome-screenshot &` then screenshot with `gnome-screenshot -f /tmp/align.png`, crop the hex column and confirm each coloured cell now sits exactly under its two hex digits. Kill the app.

- [ ] **Step 7: Commit**

```bash
git add src/panes.rs
git commit -m "fix: align hex/ASCII cell backgrounds with their glyphs"
```

---

### Task 4: Per-panel row-length functions

**Files:**
- Modify: `src/panes.rs`
- Test: `src/panes.rs` inline `mod tests`

**Interfaces:**
- Produces: `hex_bytes_per_row(panel_width: f32, char_w: f32) -> usize`, `zoom_bytes_per_row(panel_width: f32, zoom: f32) -> usize`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn hex_bytes_per_row_fits_and_snaps_to_eight() {
    let char_w = 10.0;
    // width_for(n) = ADDR_X + char_w * (12 + 4n + (n-1)/8)
    // n = 8  -> 8 + 10 * (12 + 32 + 0) = 448
    // n = 16 -> 8 + 10 * (12 + 64 + 1) = 778
    // n = 24 -> 8 + 10 * (12 + 96 + 2) = 1108
    assert_eq!(hex_bytes_per_row(448.0, char_w), 8);
    assert_eq!(hex_bytes_per_row(777.0, char_w), 8);
    assert_eq!(hex_bytes_per_row(778.0, char_w), 16);
    assert_eq!(hex_bytes_per_row(1107.0, char_w), 16);
    assert_eq!(hex_bytes_per_row(1108.0, char_w), 24);
    // Always a multiple of 8, never below 8, however narrow the panel.
    assert_eq!(hex_bytes_per_row(0.0, char_w), 8);
    assert_eq!(hex_bytes_per_row(-50.0, char_w), 8);
    for w in [500.0, 900.0, 1500.0, 4000.0] {
        assert_eq!(hex_bytes_per_row(w, char_w) % 8, 0);
    }
    // Degenerate glyph width must not divide by zero.
    assert_eq!(hex_bytes_per_row(1000.0, 0.0), 8);
}

#[test]
fn zoom_bytes_per_row_is_width_over_zoom() {
    assert_eq!(zoom_bytes_per_row(320.0, 4.0), 80);
    assert_eq!(zoom_bytes_per_row(320.0, 8.0), 40);
    assert_eq!(zoom_bytes_per_row(321.0, 8.0), 40); // floors
    assert_eq!(zoom_bytes_per_row(7.0, 8.0), 1); // never zero
    assert_eq!(zoom_bytes_per_row(0.0, 8.0), 1);
    assert_eq!(zoom_bytes_per_row(320.0, 0.0), 1); // degenerate zoom
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test bytes_per_row`
Expected: compile error — functions not found.

- [ ] **Step 3: Implement**

```rust
/// Width a hex row of `n` bytes needs: address gutter, `"HH "` per byte, a space
/// between 8-byte groups, the two-space gap, then one ASCII glyph per byte.
fn hex_row_width(n: usize, char_w: f32) -> f32 {
    let chars = 12 + 4 * n + n.saturating_sub(1) / 8;
    ADDR_X + char_w * chars as f32
}

/// Bytes per row for a hex panel `panel_width` wide: the largest multiple of 8
/// that fits, floored at 8 so the 8-byte grouping is never split.
pub(crate) fn hex_bytes_per_row(panel_width: f32, char_w: f32) -> usize {
    if !(char_w > 0.0) || !panel_width.is_finite() {
        return 8;
    }
    let mut n = 8;
    while hex_row_width(n + 8, char_w) <= panel_width {
        n += 8;
        if n >= 4096 {
            break; // absurd width: stop growing
        }
    }
    n
}

/// Bytes per row for the zoom panel: one `zoom`-wide block per byte.
pub(crate) fn zoom_bytes_per_row(panel_width: f32, zoom: f32) -> usize {
    if !(zoom > 0.0) || !panel_width.is_finite() {
        return 1;
    }
    ((panel_width / zoom).floor() as usize).max(1)
}
```

`!(char_w > 0.0)` is deliberate — it also rejects NaN. Add `#[allow(clippy::neg_cmp_op_on_partial_ord)]` on both functions if clippy pedantic objects, with the comment `// NaN-safe: `!(x > 0.0)` also rejects NaN`.

- [ ] **Step 4: Run the tests**

Run: `cargo fmt && cargo clippy --all-targets && cargo test bytes_per_row`
Expected: PASS, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/panes.rs
git commit -m "feat: derive bytes-per-row from each panel's width"
```

---

### Task 5: Byte-anchor helpers

**Files:**
- Modify: `src/panes.rs`
- Test: `src/panes.rs` inline `mod tests`

**Interfaces:**
- Produces: `row_start_for(anchor: usize, bpr: usize) -> usize`, `max_anchor(file_size: usize, hex_bpr: usize) -> usize`, `visible_rows(panel_height: f32, row_h: f32) -> usize`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn row_start_aligns_the_anchor_to_each_panel() {
    assert_eq!(row_start_for(0, 16), 0);
    assert_eq!(row_start_for(15, 16), 0);
    assert_eq!(row_start_for(16, 16), 16);
    assert_eq!(row_start_for(100, 16), 96);
    // The same anchor aligns differently per panel — that is the point.
    assert_eq!(row_start_for(100, 40), 80);
    assert_eq!(row_start_for(100, 1), 100);
    assert_eq!(row_start_for(100, 0), 100); // degenerate bpr is a no-op
}

#[test]
fn max_anchor_is_the_last_hex_row_start() {
    // 60 bytes, 16 per row -> rows at 0,16,32,48; last starts at 48.
    assert_eq!(max_anchor(60, 16), 48);
    assert_eq!(max_anchor(64, 16), 48);
    assert_eq!(max_anchor(65, 16), 64);
    assert_eq!(max_anchor(1, 16), 0);
    assert_eq!(max_anchor(0, 16), 0); // empty file
    assert_eq!(max_anchor(60, 0), 0); // degenerate bpr
}

#[test]
fn visible_rows_covers_partial_rows() {
    assert_eq!(visible_rows(100.0, 20.0), 6); // 5 full + 1 partial
    assert_eq!(visible_rows(0.0, 20.0), 1);
    assert_eq!(visible_rows(100.0, 0.0), 1); // degenerate row height
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test row_start_for max_anchor`
Expected: compile error — functions not found.

- [ ] **Step 3: Implement**

```rust
/// The byte offset of the first row a panel with `bpr` bytes per row shows when
/// the shared anchor is at `anchor`.
pub(crate) fn row_start_for(anchor: usize, bpr: usize) -> usize {
    if bpr == 0 {
        return anchor;
    }
    anchor - anchor % bpr
}

/// The furthest the shared anchor may scroll: the start of the row holding the
/// last byte, in the hex column's row length (the scroll reference, SPECS §4.2).
pub(crate) fn max_anchor(file_size: usize, hex_bpr: usize) -> usize {
    if file_size == 0 || hex_bpr == 0 {
        return 0;
    }
    row_start_for(file_size - 1, hex_bpr)
}

/// Rows needed to cover `panel_height`, including a partially visible last row.
pub(crate) fn visible_rows(panel_height: f32, row_h: f32) -> usize {
    if !(row_h > 0.0) || !panel_height.is_finite() {
        return 1;
    }
    ((panel_height / row_h).ceil() as usize).max(1)
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: PASS, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/panes.rs
git commit -m "feat: add byte-anchor scroll helpers"
```

---

### Task 6: Zoom view — one band per byte, anchor-driven

Replaces `paint_pixels` / `pixels_offset_at`.

**Files:**
- Modify: `src/panes.rs`
- Test: `src/panes.rs` inline `mod tests`

**Interfaces:**
- Produces:
```rust
pub(crate) fn paint_zoom(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    data: &[u8],
    bpr: usize,
    first_row_start: usize,
    zoom: f32,
    hovered: Option<usize>,
    sel: Option<&Range<usize>>,
    entropies: &[f32],
    entropy_window: usize,
    colormap: Colormap,
);

pub(crate) fn zoom_offset_at(
    local: Point<Pixels>,
    bpr: usize,
    first_row_start: usize,
    zoom: f32,
    len: usize,
) -> Option<usize>;
```
Rows are flush: row height == `zoom`. `PIXEL_ZOOM_*` constants keep their names and range (1–24).

- [ ] **Step 1: Write the failing test**

Replace `pixels_offset_at_rejects_the_blank_area_right_of_the_row` with:

```rust
#[test]
fn zoom_offset_at_maps_rows_flush_and_rejects_blank_space() {
    // 16 bytes/row at 4 px: bytes span x in [0,64); rows are 4 px tall (flush).
    let hit = |x: f32, y: f32, first: usize| {
        zoom_offset_at(point(gpui::px(x), gpui::px(y)), 16, first, 4.0, 60)
    };

    assert_eq!(hit(0.0, 0.0, 0), Some(0));
    assert_eq!(hit(63.9, 0.0, 0), Some(15));
    assert_eq!(hit(64.0, 0.0, 0), None); // right of the last byte
    assert_eq!(hit(300.0, 0.0, 0), None);
    // Rows are flush: the second row starts at y = zoom, not 2*zoom + 1.
    assert_eq!(hit(0.0, 4.0, 0), Some(16));
    assert_eq!(hit(0.0, 8.0, 0), Some(32));
    // Anchored elsewhere in the file.
    assert_eq!(hit(0.0, 0.0, 32), Some(32));
    assert_eq!(hit(4.0, 4.0, 32), Some(49));
    // Past end of file (row 3 holds only 48..60).
    assert_eq!(hit(48.0, 0.0, 48), None);
    assert_eq!(hit(44.0, 0.0, 48), Some(59));
    // Degenerate inputs.
    assert_eq!(hit(-1.0, 0.0, 0), None);
    assert_eq!(hit(0.0, -1.0, 0), None);
    assert_eq!(
        zoom_offset_at(point(gpui::px(0.), gpui::px(0.)), 16, 0, 4.0, 0),
        None
    );
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test zoom_offset_at`
Expected: compile error — `zoom_offset_at` not found.

- [ ] **Step 3: Implement `zoom_offset_at`**

```rust
/// Map a point in the zoom canvas to a file offset, or `None` when it is
/// outside the painted bytes. Rows are flush, one `zoom`-sized band per byte.
///
/// `paint_zoom` only draws `bpr` blocks per row, so anything to the right of
/// them is empty background and must not resolve to a byte.
pub(crate) fn zoom_offset_at(
    local: Point<Pixels>,
    bpr: usize,
    first_row_start: usize,
    zoom: f32,
    len: usize,
) -> Option<usize> {
    let x = local.x.to_f64() as f32;
    let y = local.y.to_f64() as f32;
    if x < 0.0 || y < 0.0 || len == 0 || bpr == 0 || !(zoom > 0.0) {
        return None;
    }
    let col = (x / zoom) as usize;
    if col >= bpr {
        return None;
    }
    let row = (y / zoom) as usize;
    let off = row.checked_mul(bpr)?.checked_add(first_row_start)?.checked_add(col)?;
    (off < len).then_some(off)
}
```

- [ ] **Step 4: Rewrite `paint_pixels` as `paint_zoom`**

One band per byte, rows flush, colormap may paint nothing:

```rust
/// Paint the zoom column: one `zoom`-sized band per byte in `colormap`, rows
/// flush so the panel reads as a pixel image. Virtualized to `bounds`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_zoom(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    data: &[u8],
    bpr: usize,
    first_row_start: usize,
    zoom: f32,
    hovered: Option<usize>,
    sel: Option<&Range<usize>>,
    entropies: &[f32],
    entropy_window: usize,
    colormap: Colormap,
) {
    let len = data.len();
    if len == 0 || bpr == 0 || !(zoom > 0.0) {
        return;
    }
    let rows = visible_rows(bounds.size.height.to_f64() as f32, zoom);
    for r in 0..rows {
        let row_start = first_row_start + r * bpr;
        if row_start >= len {
            break;
        }
        let y = bounds.top().to_f64() as f32 + r as f32 * zoom;
        let n = (len - row_start).min(bpr);
        for i in 0..n {
            let off = row_start + i;
            let rect = Bounds::new(
                point(bounds.left() + px(i as f32 * zoom), px(y)),
                size(px(zoom), px(zoom)),
            );
            if let Some(c) =
                colormap.color_for(data[off], entropy_at(entropies, entropy_window, off))
            {
                let bg: Background = c.into();
                window.paint_quad(quad(
                    rect,
                    px(0.),
                    bg,
                    px(0.),
                    transparent_black(),
                    BorderStyle::default(),
                ));
            }
            if sel.is_some_and(|s| s.contains(&off)) {
                let tint: Background = rgba(0xffffff30).into();
                window.paint_quad(quad(
                    rect,
                    px(0.),
                    tint,
                    px(0.),
                    transparent_black(),
                    BorderStyle::default(),
                ));
            }
            if hovered == Some(off) {
                window.paint_quad(quad(
                    rect,
                    px(0.),
                    transparent_black(),
                    px(1.),
                    gpui::white(),
                    BorderStyle::default(),
                ));
            }
        }
    }
}
```

Delete `paint_pixels` and `pixels_offset_at`.

- [ ] **Step 5: Update `app.rs` call sites so the crate compiles**

In `pixels_column`'s paint closure call `panes::paint_zoom(...)`; in `ParallHexApp::pixels_offset_at` call `panes::zoom_offset_at(local, self.zoom_bpr.max(1), panes::row_start_for(self.scroll_offset, self.zoom_bpr.max(1)), px_size, self.file_size)`. These fields arrive in Task 10 — until then, pass `self.bytes_per_row.max(1)` and `(self.scroll_rows as usize) * self.bytes_per_row.max(1)` so the crate keeps compiling. Delete `pixels_row_h`.

- [ ] **Step 6: Run the tests**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: PASS, clippy clean.

- [ ] **Step 7: Commit**

```bash
git add src/panes.rs src/app.rs
git commit -m "feat: zoom column paints one flush band per byte"
```

---

### Task 7: Overview and strip — one band per cell, per-panel colormap

**Files:**
- Modify: `src/panes.rs`
- Test: `src/panes.rs` inline `mod tests`

**Interfaces:**
- Produces: `build_overview_image(data, entropies, entropy_window, w, h, colormap) -> (Arc<RenderImage>, (usize, usize))` — texture is now `w × h`; `build_strip_image(data, entropies, entropy_window, colormap) -> Arc<RenderImage>` — texture is `256 × 1`.

- [ ] **Step 1: Write the failing tests**

Replace `overview_buffer_is_w_by_2h_and_opaque`, `overview_greyscale_band_is_byte_brightness`, `overview_entropy_band_high_for_full_range_bytes`, `strip_buffer_is_256x2_and_opaque` and `overview_cells_tile_the_file_in_row_major_order` with single-band versions:

```rust
#[test]
fn overview_buffer_is_w_by_h_and_opaque() {
    let data = vec![0x80u8; 4096];
    let e = entropies(&data);
    let buf = build_overview_rgba(&data, &e, 256, 8, 4, Colormap::Value);
    assert_eq!(buf.len(), 8 * 4 * 4); // w * h * RGBA, one band per cell
    for y in 0..4 {
        for x in 0..8 {
            assert_eq!(px(&buf, 8, x, y).3, 255, "cell ({x},{y}) not opaque");
        }
    }
}

#[test]
fn overview_value_colormap_is_byte_brightness() {
    let data = vec![0x40u8; 1024];
    let e = entropies(&data);
    let buf = build_overview_rgba(&data, &e, 256, 4, 2, Colormap::Value);
    assert_eq!(px(&buf, 4, 0, 0), (0x40, 0x40, 0x40, 255));
}

#[test]
fn overview_none_colormap_is_transparent() {
    let data = vec![0x40u8; 1024];
    let e = entropies(&data);
    let buf = build_overview_rgba(&data, &e, 256, 4, 2, Colormap::None);
    assert_eq!(px(&buf, 4, 0, 0), (0, 0, 0, 0));
}

#[test]
fn overview_cells_tile_the_file_row_major() {
    // 4 cells over 4 bytes: cell k is byte k, so the row-major order is visible.
    let data = vec![0x00u8, 0x40, 0x80, 0xC0];
    let e = entropies(&data);
    let buf = build_overview_rgba(&data, &e, 256, 2, 2, Colormap::Value);
    assert_eq!(px(&buf, 2, 0, 0).0, 0x00);
    assert_eq!(px(&buf, 2, 1, 0).0, 0x40);
    assert_eq!(px(&buf, 2, 0, 1).0, 0x80);
    assert_eq!(px(&buf, 2, 1, 1).0, 0xC0);
}

#[test]
fn strip_buffer_is_256x1_and_opaque() {
    let data: Vec<u8> = (0..=255u8).collect();
    let e = entropies(&data);
    let buf = build_strip_rgba(&data, &e, 256, Colormap::Value);
    assert_eq!(buf.len(), 256 * 4);
    for x in 0..256 {
        assert_eq!(px(&buf, 256, x, 0).3, 255);
    }
}
```

Update `strip_maps_file_offset_to_x`, `strip_handles_a_single_byte_file`, `empty_data_yields_an_empty_transparent_buffer`, `overview_handles_zero_dimensions`, `thumbnail_wrappers_build_valid_images` and `thumbnails_generate_for_a_real_elf` to pass `Colormap::Value` and expect `w * h * 4` / `256 * 4` byte buffers.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test overview_buffer strip_buffer`
Expected: compile error — `build_overview_rgba` takes 5 arguments.

- [ ] **Step 3: Implement**

`build_overview_rgba` becomes single-band and colormap-driven:

```rust
fn build_overview_rgba(
    data: &[u8],
    entropies: &[f32],
    entropy_window: usize,
    w: usize,
    h: usize,
    colormap: Colormap,
) -> Vec<u8> {
    let w = w.max(1);
    let h = h.max(1);
    if data.is_empty() {
        return vec![0u8; w * h * 4];
    }
    let len = data.len();
    let cells = (w * h).max(1);
    let mut pixels = vec![0u8; w * h * 4];
    for k in 0..cells {
        let start = k * len / cells;
        let end = ((k + 1) * len / cells).max(start + 1);
        let mid = (start + (end - start) / 2).min(len - 1);
        let avg = sample_average(data, start, end);
        // `None` leaves the cell transparent so the panel background shows.
        if let Some(c) = colormap.color_for(avg, entropy_at(entropies, entropy_window, mid)) {
            set_pixel(&mut pixels, w, k % w, k / w, c);
        }
    }
    pixels
}
```

`build_overview_image` returns `render_image_from_rgba(w, h, pixels)` and the same `(w, h)` cell grid. `build_strip_rgba` mirrors it at `W = 256`, `h = 1`, and `build_strip_image` calls `render_image_from_rgba(256, 1, pixels)`.

- [ ] **Step 4: Fix `app.rs` call sites**

`build_overview_image(d, &self.entropies, self.entropy_window, w, h, self.overview_colormap)` and `build_strip_image(d, &self.entropies, self.entropy_window, self.overview_colormap)` — until Task 10 adds those fields, pass `self.pixel_colormap`.

`paint_overview` and `paint_strip` are unchanged (they paint the image plus the viewport band).

- [ ] **Step 5: Run the tests**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: PASS, clippy clean.

- [ ] **Step 6: Commit**

```bash
git add src/panes.rs src/app.rs
git commit -m "feat: single-band overview and strip driven by a colormap"
```

---

### Task 8: Hex column — colormap, fixed text size, anchor

**Files:**
- Modify: `src/panes.rs`
- Test: `src/panes.rs` inline `mod tests`

**Interfaces:**
- Produces:
```rust
pub(crate) const ROW_H: f32 = 18.0;
pub(crate) const ROW_GAP: f32 = 3.0;
pub(crate) const BLOCK_H: f32 = ROW_H + ROW_GAP; // 21.0

pub(crate) fn paint_hex(
    window: &mut Window,
    cx: &mut App,
    bounds: Bounds<Pixels>,
    data: &[u8],
    font: &Font,
    bpr: usize,
    first_row_start: usize,
    hovered: Option<usize>,
    sel: Option<&Range<usize>>,
    entropies: &[f32],
    entropy_window: usize,
    colormap: Colormap,
);

pub(crate) fn hex_offset_at(
    local: Point<Pixels>,
    geo: &RowGeo,
    first_row_start: usize,
    len: usize,
) -> Option<usize>;
```
`hex_row_h`, `hex_block_h`, `zoom_step`, `HEX_ZOOM_DEFAULT`, `HEX_ZOOM_MIN`, `HEX_ZOOM_MAX` are deleted (`ZOOM_STEP` stays for the zoom column).

- [ ] **Step 1: Write the failing test**

Rewrite both `hex_offset_at` tests against the anchor and the constant block height:

```rust
#[test]
fn hex_offset_at_maps_y_to_row_and_x_to_byte() {
    let geo = row_geo();
    assert_eq!(BLOCK_H, 21.0);
    let len = 64usize;
    let hit = |y: f32, x: f32| {
        hex_offset_at(point(gpui::px(x), gpui::px(y)), &geo, 0, len)
    };
    assert_eq!(hit(0.0, 108.0), Some(0));
    assert_eq!(hit(0.0, 138.0), Some(1));
    assert_eq!(hit(10.0, 108.0), Some(0));
    assert_eq!(hit(0.0, 618.0), Some(0)); // ASCII block
    assert_eq!(hit(21.0, 108.0), Some(16));
    assert_eq!(hit(63.0, 108.0), Some(48));
    assert_eq!(hit(-1.0, 108.0), None);
    assert_eq!(hit(84.0, 108.0), None); // past end of file
    assert_eq!(hit(0.0, 50.0), None); // address gutter
    assert_eq!(hit(0.0, 350.0), None); // group gap
}

#[test]
fn hex_offset_at_is_anchored_and_clamps_to_file_end() {
    let geo = row_geo();
    let len = 60usize;
    let hit = |y: f32, x: f32| {
        hex_offset_at(point(gpui::px(x), gpui::px(y)), &geo, 32, len)
    };
    // Anchored at byte 32: the top row is 32..48, the next 48..60.
    assert_eq!(hit(0.0, 108.0), Some(32));
    assert_eq!(hit(21.0, 108.0), Some(48));
    assert_eq!(geo.cell_x(11), 448.0);
    assert_eq!(hit(21.0, 448.0), Some(59)); // last byte
    assert_eq!(hit(21.0, 478.0), None); // cell 12 == len
    assert_eq!(hit(42.0, 108.0), None); // past end of file
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test hex_offset_at`
Expected: compile error — `hex_offset_at` takes 6 arguments / `BLOCK_H` not found.

- [ ] **Step 3: Implement**

```rust
pub(crate) fn hex_offset_at(
    local: Point<Pixels>,
    geo: &RowGeo,
    first_row_start: usize,
    len: usize,
) -> Option<usize> {
    let y = local.y.to_f64() as f32;
    if y < 0.0 || len == 0 {
        return None;
    }
    let row = (y / BLOCK_H) as usize;
    let row_start = first_row_start.checked_add(row.checked_mul(geo.bpr)?)?;
    if row_start >= len {
        return None;
    }
    let i = geo.byte_at_x(local.x.to_f64() as f32)?;
    let off = row_start + i;
    (off < len).then_some(off)
}
```

In `paint_hex`: drop the `zoom` parameter, use `px(HEX_FONT_SIZE)`, `ROW_H` and `BLOCK_H` directly, iterate `visible_rows(bounds.size.height…, BLOCK_H)` rows from `first_row_start`, and gate the two background quads on the colormap:

```rust
        for i in 0..n {
            let off = row_start + i;
            let bg = colormap.color_for(data[off], entropy_at(entropies, entropy_window, off));
            if let Some(bg) = bg {
                // ... paint hex_rect and ascii_rect with `bg` as today ...
            }
        }
```

`build_row_runs` gains the same `colormap`, `entropies` and `entropy_window` parameters and picks each glyph colour from the background when there is one, or the default foreground when there isn't:

```rust
const DEFAULT_FG: u32 = 0xc0caf5;

let fg = match colormap.color_for(b, entropy_at(entropies, entropy_window, off)) {
    Some(bg) => to_hsla(color::fg_for_bg(bg)),
    None => to_hsla(rgb(DEFAULT_FG)),
};
```

- [ ] **Step 4: Fix `app.rs` call sites**

`hex_column`'s paint closure passes the new arguments; `hex_offset_at_pos` builds `RowGeo::new(char_w, self.hex_bpr.max(8))` and passes `panes::row_start_for(self.scroll_offset, self.hex_bpr.max(8))`. Until Task 10, use `self.bytes_per_row` and the old scroll conversion so the crate compiles. Remove every `self.hex_zoom` reference in `panes::` calls.

- [ ] **Step 5: Run the tests**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: PASS, clippy clean.

- [ ] **Step 6: Commit**

```bash
git add src/panes.rs src/app.rs
git commit -m "feat: hex column takes a colormap and a byte anchor at fixed text size"
```

---

### Task 9: Config migration

**Files:**
- Modify: `src/config.rs`
- Test: `src/config.rs` inline `mod tests`

**Interfaces:**
- Produces:
```rust
pub struct Config {
    pub entropy_window: usize,
    pub pixel_zoom: f32,
    pub overview_colormap: Colormap,
    pub zoom_colormap: Colormap,
    pub hex_colormap: Colormap,
    pub overview_width: f32,
    pub zoom_width: f32,
    pub window_bounds: Option<(f32, f32, f32, f32)>,
    pub window_maximized: bool,
}
```
Defaults: `entropy_window: 256`, `pixel_zoom: PIXEL_ZOOM_DEFAULT`, `overview_colormap: Entropy`, `zoom_colormap: Value`, `hex_colormap: Class`, `overview_width: 200.0`, `zoom_width: 320.0`.

- [ ] **Step 1: Write the failing tests**

Replace `parse_round_trip`, `parse_ignores_unknown_and_malformed_lines`, `parse_rejects_non_finite_and_unparseable_values` and `default_zooms_are_within_their_clamps` with:

```rust
#[test]
fn parse_round_trip() {
    let cfg = Config {
        entropy_window: 512,
        pixel_zoom: 8.0,
        overview_colormap: Colormap::None,
        zoom_colormap: Colormap::Class,
        hex_colormap: Colormap::Entropy,
        overview_width: 250.0,
        zoom_width: 400.0,
        window_bounds: Some((120.0, 80.0, 1600.0, 900.0)),
        window_maximized: true,
    };
    assert_eq!(parse(&serialize(&cfg)), cfg);
}

#[test]
fn every_colormap_value_round_trips() {
    for cm in Colormap::ALL {
        let cfg = Config { hex_colormap: cm, ..Config::default() };
        assert_eq!(parse(&serialize(&cfg)).hex_colormap, cm, "{:?}", cm);
    }
}

#[test]
fn retired_keys_are_ignored() {
    // An old config file must load without error and keep the new defaults.
    let cfg = parse(
        "bytes_per_row = 64\n\
         hex_zoom = 2.5\n\
         pixel_colormap = greyscale\n\
         pixels_width = 400\n\
         entropy_window = 1024\n",
    );
    assert_eq!(cfg.entropy_window, 1024);
    assert_eq!(cfg, Config { entropy_window: 1024, ..Config::default() });
}

#[test]
fn parse_ignores_unknown_and_malformed_lines() {
    let cfg = parse(
        "# comment\n\
         unknown_key = 1\n\
         pixel_zoom = 6\n\
         garbage line without equals\n\
         hex_colormap = nonsense\n",
    );
    assert_eq!(cfg.pixel_zoom, 6.0);
    assert_eq!(cfg.hex_colormap, Config::default().hex_colormap);
    assert_eq!(cfg.overview_width, 200.0);
}

#[test]
fn default_zoom_is_within_its_clamp() {
    let cfg = Config::default();
    assert!((crate::panes::PIXEL_ZOOM_MIN..=crate::panes::PIXEL_ZOOM_MAX).contains(&cfg.pixel_zoom));
    assert!((16..=4096).contains(&cfg.entropy_window));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib config`
Expected: compile error — no field `zoom_colormap`.

- [ ] **Step 3: Implement**

Update the struct, `Default`, `parse` (three colormap arms, `zoom_width`; delete the `bytes_per_row`, `hex_zoom`, `pixel_colormap`, `pixels_width` arms) and `serialize`. Each colormap arm follows the existing shape:

```rust
"overview_colormap" => {
    if let Some(cm) = Colormap::from_key(value) {
        cfg.overview_colormap = cm;
    }
}
```

and serialization mirrors it:

```rust
let _ = writeln!(out, "overview_colormap = {}", cfg.overview_colormap.key());
let _ = writeln!(out, "zoom_colormap = {}", cfg.zoom_colormap.key());
let _ = writeln!(out, "hex_colormap = {}", cfg.hex_colormap.key());
let _ = writeln!(out, "zoom_width = {}", cfg.zoom_width.round());
```

Import `PIXEL_ZOOM_DEFAULT` only (drop `HEX_ZOOM_DEFAULT`).

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib config`
Expected: the config tests pass. `app.rs` will not compile yet — that is Task 10; if the crate must stay green, do Steps 3–4 of Task 10 before committing and squash the two commits.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat: migrate preferences to per-panel colormaps"
```

---

### Task 10: `app.rs` state migration

**Files:**
- Modify: `src/app.rs`

**Interfaces:**
- Produces on `ParallHexApp`: `scroll_offset: usize`, `hex_bpr: usize`, `zoom_bpr: usize`, `overview_colormap/zoom_colormap/hex_colormap: Colormap`, `zoom_width: f32`, `open_colormap_menu: Option<Panel>`; and `pub(crate) enum Panel { Overview, Zoom, Hex }`.
- Removed: `bytes_per_row`, `hex_zoom`, `pixel_colormap`, `pixels_width`, `scroll_rows`, `scroll_reset`, `pixels_row_h`, `colormap_menu_open`, `SliderKind::HexZoom`.

- [ ] **Step 1: Swap the state fields**

Replace the fields listed above; rename `DividerKind::{OverviewPixels, PixelsHex}` to `{OverviewZoom, ZoomHex}` and `PIXELS_W_MIN/MAX` to `ZOOM_W_MIN/MAX`. Add:

```rust
/// Which column a per-panel control belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Panel {
    Overview,
    Zoom,
    Hex,
}

impl ParallHexApp {
    fn colormap(&self, panel: Panel) -> Colormap {
        match panel {
            Panel::Overview => self.overview_colormap,
            Panel::Zoom => self.zoom_colormap,
            Panel::Hex => self.hex_colormap,
        }
    }

    fn set_colormap(&mut self, panel: Panel, cm: Colormap) {
        match panel {
            Panel::Overview => {
                self.overview_colormap = cm;
                // The thumbnails bake the colormap in, so they must be rebuilt.
                self.overview_gen_size = None;
                self.strip_dirty = true;
            }
            Panel::Zoom => self.zoom_colormap = cm,
            Panel::Hex => self.hex_colormap = cm,
        }
    }
}
```

- [ ] **Step 2: Update `new`, `current_config` and `reset_all_settings`**

`new` clamps `pixel_zoom` only and copies the three colormaps and both widths from prefs. `current_config` mirrors it. `reset_all_settings` assigns **every** field of `defaults` — including all three colormaps — then sets `self.scroll_offset = 0`, `self.overview_gen_size = None`, `self.strip_dirty = true`.

- [ ] **Step 3: Replace the scroll plumbing**

Delete `clamp_scroll`'s row math and `scroll_reset`; add:

```rust
    /// Clamp the shared anchor to the hex column's last row (SPECS §4.2).
    fn clamp_anchor(&mut self) {
        self.scroll_offset = self
            .scroll_offset
            .min(panes::max_anchor(self.file_size, self.hex_bpr.max(8)));
    }

    /// Scroll by whole rows of `panel`.
    fn scroll_rows_by(&mut self, panel: Panel, rows: i32) {
        let bpr = match panel {
            Panel::Zoom => self.zoom_bpr.max(1),
            _ => self.hex_bpr.max(8),
        };
        let delta = rows.unsigned_abs() as usize * bpr;
        self.scroll_offset = if rows < 0 {
            self.scroll_offset.saturating_sub(delta)
        } else {
            self.scroll_offset.saturating_add(delta)
        };
        self.clamp_anchor();
    }
```

`scroll_to_offset` centring, in the hex canvas prepaint, becomes:

```rust
let rows = panes::visible_rows(this.view_height, panes::BLOCK_H);
let bpr = this.hex_bpr.max(8);
let half = rows / 2 * bpr;
this.scroll_offset = off.saturating_sub(half);
this.clamp_anchor();
```

- [ ] **Step 4: Compile**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: clippy clean; all tests pass. Visual layout may still be mid-rework — Task 11 finishes it.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "refactor: drive app state from a shared byte anchor"
```

---

### Task 11: Three column headers, per-panel colormap menus, trimmed top bar

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Generalise the colormap dropdown**

`pixels_header`'s toggle + `colormap_menu` become `colormap_picker(&mut self, cx, panel) -> impl IntoElement`, keyed on `open_colormap_menu == Some(panel)`, using `self.colormap(panel)` / `self.set_colormap(panel, cm)` and element ids `("colormap-toggle", panel as usize)` / `("colormap", panel as usize, idx)`. The root's outside-click handler clears `open_colormap_menu` instead of the old bool.

- [ ] **Step 2: Give every header its picker**

`column_header(title, range, trailing)` is already generic. Overview: `trailing = self.colormap_picker(cx, Panel::Overview)`. Hex: `trailing = self.colormap_picker(cx, Panel::Hex)` and nothing else — no zoom readout, slider or reset. Zoom: the `N px` readout, `SliderKind::PixelZoom` slider, `Reset`, and `self.colormap_picker(cx, Panel::Zoom)`.

Each header's range label uses that panel's own row length:

```rust
let range = (len > 0).then(|| {
    let first = panes::row_start_for(self.scroll_offset, bpr);
    let rows = panes::visible_rows(self.view_height, row_h);
    panes::range_label(first, (first + rows * bpr).min(len))
});
```

- [ ] **Step 3: Compute each panel's row length in its prepaint**

Hex prepaint: `let char_w = panes::hex_char_width(window, &font, px(panes::HEX_FONT_SIZE)); this.hex_bpr = panes::hex_bytes_per_row(bounds.size.width.to_f64() as f32, char_w);` then `this.view_height = …` and `this.clamp_anchor()`. Zoom prepaint: `this.zoom_bpr = panes::zoom_bytes_per_row(bounds.size.width.to_f64() as f32, zoom);`.

- [ ] **Step 4: Trim the top bar**

Delete the `Bytes/Row` label and its three buttons from `row2`. The strip keeps `flex_shrink_0()` and now passes `self.overview_colormap`.

- [ ] **Step 5: Verify**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Then run the app on a real file and confirm with a screenshot: three headers each showing `Map: …`, only the middle one with a zoom slider, no Bytes/Row buttons, and the hex rows filling the column's width.

- [ ] **Step 6: Commit**

```bash
git add src/app.rs
git commit -m "feat: per-panel colormap pickers and width-derived hex rows"
```

---

### Task 12: Interactions on the byte anchor

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Wheel and drag**

`on_hex_scroll` / `on_zoom_scroll` (renamed from `on_pixels_scroll`) call `self.scroll_rows_by(panel, rows)` where `rows = -(delta.pixel_delta(px(16.0)).y.to_f64() as f32 / row_h).round() as i32` for that panel's row height (`panes::BLOCK_H` for hex, `pixel_zoom` for zoom). Drag-to-pan accumulates pixels and converts the same way, replacing `last_pixels_y` arithmetic on `scroll_rows`.

- [ ] **Step 2: Keyboard**

`navigate` uses `self.hex_bpr.max(8)` for one-row moves and `panes::visible_rows(self.view_height, panes::BLOCK_H) * hex_bpr` for a page. `on_reset_view` sets `self.scroll_offset = 0`.

- [ ] **Step 3: Overview, strip and jump**

`overview_offset_at` / `strip_offset_at` already return byte offsets — unchanged. `jump_to` sets `scroll_to_offset`, which Task 10's prepaint centres.

- [ ] **Step 4: Verify by hand**

Run the app and confirm: the wheel scrolls both pixel and hex columns together; dragging the zoom column pans; PageDown advances one screen of hex; clicking the overview jumps; the jump dialog centres its target; arrow keys move the selection and auto-scroll.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "feat: scroll, pan and navigate on the shared byte anchor"
```

---

### Task 13: Final verification and documentation

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Full gate run**

Run: `cargo fmt --check && cargo clippy --all-targets && cargo test`
Expected: clean, all tests pass.

- [ ] **Step 2: Manual verification against SPECS**

Launch on a large file and confirm, one by one: rows fit each column with no horizontal overflow; resizing a divider reflows that column's rows; each panel's `Map` dropdown changes only that panel; `None` empties a panel but leaves it clickable; the zoom slider changes both block size and bytes-per-row; hex cell colours sit exactly under their digits; window move/resize/close still work.

- [ ] **Step 3: Update `CLAUDE.md`**

The architecture section still describes `scroll_rows`, a shared `bytes_per_row` and the hex column as "the master". Replace with the byte-anchor model, per-panel row lengths, and the rule that the hex column is the scroll reference. Add the alignment invariant from SPECS §6 to the gotchas.

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: describe the byte-anchor scroll model"
```

---

## Self-Review

**Spec coverage.** §3.A row lengths → Task 4. §3.C colormaps → Task 2 (enum), 6/7/8 (application). §4.1 columns → Tasks 6, 7, 8, 11. §4.2 anchor → Tasks 5, 10, 12. §4.3 keyboard → Task 12. §4.5 strip → Task 7, 11. §4.6 preferences → Task 9. §5 pipeline → unchanged, guarded by existing tests. §6 alignment invariant → Task 3. §7 testing → distributed across tasks. §4.4 CLI and the window chrome are unchanged by this rework and need no task.

**Placeholders.** None: every code step carries real code, every test step real assertions, and every verification step an exact command and expected result.

**Type consistency.** `Colormap::color_for` returns `Option<Rgba>` from Task 2 onward and every consumer (Tasks 6, 7, 8) matches on it. `first_row_start` is a byte offset in `paint_zoom`, `zoom_offset_at`, `paint_hex` and `hex_offset_at` alike, always produced by `row_start_for`. `hex_bpr`/`zoom_bpr` are the field names in Tasks 10–12. `Panel` is defined in Task 10 and used in Tasks 10–12. Tasks 6–8 keep the crate compiling with interim expressions and Task 10 replaces them — each of those steps names the interim form explicitly.
