# Three-column rework — design

Date: 2026-08-02 · Branch: `gpui-port` · Spec updated: `SPECS.md`

## Why

The app grew from a "four synchronized panes" design into three columns, but the
model underneath never caught up:

- All panels shared one `bytes_per_row` chosen from a 16/32/64 button group, so a
  panel narrower or wider than that choice either wasted width or overflowed it.
- Both the hex column and the zoom column had independent zooms, which
  interacted badly with a shared row length.
- The zoom and overview panels each drew *two* bands per byte (a colormap band
  over an entropy band), fixing what each panel could say and halving its
  vertical density.
- Colour mode was a single global choice (`pixel_colormap`) applied to the zoom
  column only; the hex column was hardcoded to the class palette and the
  overview to greyscale.
- The hex column's class backgrounds were drawn half a byte to the right of the
  digits they belonged to.

## Decisions

**Row length is per panel; the panels share a byte anchor.** Each column derives
its own row length from its own width, and they anchor to the same
`scroll_offset` rather than the same row index. This is the only arrangement
where all three constraints hold at once: content fits each panel's width,
nothing scrolls horizontally, and the middle column's zoom is free to change
density. Cost, accepted: rows no longer line up across columns.

**Zoom belongs to the middle column only.** The hex text size becomes fixed and
the overview always fits the panel, so the Bytes/Row buttons and the hex zoom
slider are removed along with their preferences.

**One band per byte.** The zoom view and the overview each paint a single band in
their own colormap, rows flush, so the middle column reads as a true pixel image
and the overview texture is `w × h` rather than `w × 2h`.

**Colormap is per panel, with `None` meaning no colormap.** `Colormap` becomes
`None | Value | Class | Entropy` (`Greyscale` → `Value`, `ByteClass` → `Class`).
Under `None` no per-byte colour is painted at all: the hex column shows plain
text on the panel background and the pixel panels show nothing. Each header
carries the existing `Map: … ▾` dropdown. Defaults: overview `Entropy`, zoom
`Value`, hex `Class`.

**The top-bar strip stays** as a compact always-visible file map, now single-band
and following the overview's colormap.

## The alignment bug

Two independent off-by-one errors, both from `RowGeo` and `build_row_text`
disagreeing about the same character layout:

1. `RowGeo::hex_start` includes `ADDR_X` (8 px) of gutter padding, but the row
   text was painted at `origin.x`. At the fixed text size a monospace glyph is
   ≈7.8 px, so every background sat one hex digit — half a byte — right of its
   digits. This is the symptom that was reported.
2. `RowGeo::hex_w` counts `bpr / 8` group gaps (4 for a 32-byte row) while the
   text builder emits a space only *between* groups (3). The ASCII block's
   backgrounds were therefore a full character right of its glyphs.

Fixes: paint the text at `origin.x + ADDR_X`, and count `(n - 1) / 8` gaps. The
regression test asserts glyph x == rect x for every byte in both blocks, so
neither can drift again.

## Migration

`bytes_per_row`, `hex_zoom`, `pixel_colormap` and `pixels_width` are retired.
The config parser already ignores unknown keys and clamps out-of-range values, so
existing files load silently and are rewritten in the new form. `pixel_zoom`
keeps its meaning (the middle column). `pixels_width` becomes `zoom_width`.

## Testing

Pure functions in `panes.rs`, unit-tested without a window: the alignment
identity; `hex_bytes_per_row` / `zoom_bytes_per_row` fit, snapping and minimum;
anchor → per-panel first row with end-of-file clamping; hit-test rejection past
the last byte of a row; the existing entropy and config round-trip tests
extended to the new keys and `none`.

## Explicitly out of scope

- A right-click context menu in the hex column (SPECS documents the current
  right-click-copies behaviour instead).
- Any change to the jump dialog, CLI, window chrome or persistence mechanism
  beyond the key changes above.
- The status bar's `no file loaded` readout fallback, which is a separate
  cosmetic bug.
