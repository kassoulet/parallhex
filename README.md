# Parall-Hex

[![CI](https://github.com/kassoulet/parallhex/actions/workflows/ci.yml/badge.svg)](https://github.com/kassoulet/parallhex/actions/workflows/ci.yml)

A native binary/hex explorer: one wide window showing the same region of a file
through three synchronized columns.

![ParallHex showing libc.so.6: an entropy overview of the whole file, a
byte-class pixel map, and class-coloured hex with a selection](docs/screenshot.png)

*`libc.so.6` at offset 0. The overview (left) maps entropy over the whole 2 MiB —
the green body is compiled code, the red bands at each end are the tables. The
zoom column (middle) switches to the byte-class palette, where the section
boundaries become visible as texture changes: high bytes in orange, a block of
printable strings in blue. On the right, 78 selected bytes are tinted across
the hex and ASCII cells.*

| Column | Shows |
|---|---|
| **Overview** (left) | The whole file downsampled to one band per cell, with a marker for the region the zoom column is showing. |
| **Zoom** (middle) | One coloured block per byte — a true pixel image of the bytes, at 1–24 px per byte. |
| **Hex / ASCII** (right) | Conventional `ADDR  HH HH …  ASCII` rows, with each cell's background coloured by the byte. |

All three scroll together off a single shared byte anchor, so you can read
structure at three scales at once: an ELF's sections in the overview, a run of
padding in the zoom column, and the exact bytes in hex.

Files are `mmap`ed and every column paints only what is on screen, so a
multi-gigabyte file opens instantly and costs no more to scroll than a small one.

## Two frontends

The same three columns over the same file, in a window or in a terminal:

| Binary | Toolkit | Notes |
|---|---|---|
| `parallhex-gpui` | [gpui](https://crates.io/crates/gpui) | The full app: mouse, resizable columns, a zoom control. |
| `parallhex-tui` | [ratatui](https://ratatui.rs) | Keyboard-driven, over ssh or on a headless host. |

Both share all the byte-level semantics — colours, entropy, row geometry, the
scroll anchor — so they cannot disagree about what a byte looks like or where it
sits. They also share one preferences file.

## Build and run

Requires a Rust toolchain supporting edition 2024.

```sh
cargo run --bin parallhex-gpui                        # windowed, no file
cargo run --bin parallhex-gpui -- path/to/file.bin    # open a file
cargo run --bin parallhex-tui  -- path/to/file.bin    # in the terminal
cargo run --release --bin parallhex-gpui              # prefer release for large files
```

`--` is needed before a filename that begins with a dash. `-h` / `--help` prints
usage; each binary names itself. Extra positional arguments after the first are
ignored. The windowed frontend can start with no file and open one from a dialog;
the terminal one requires a path, since it has no dialog to fall back on.

The executables are named for their toolkits so they can sit alongside another
`parallhex` build. The preferences directory stays `parallhex` for both.

**Building the terminal frontend alone** needs none of gpui's link-time
libraries, which is the point of it:

```sh
cargo build --no-default-features --features tui-frontend
```

CI asserts that gpui cannot reach that build's dependency tree.

## Terminal frontend

```
┌ Overview · Entropy ┐┌ Zoom · Class ─────────┐┌ Hex · Class ────────────────────┐
│▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀││▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀││00000000  7F 45 4C 46  .ELF.... │
│▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀││▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀││00000010  03 00 3E 00  ..>..... │
└────────────────────┘└───────────────────────┘└─────────────────────────────────┘
 0x00000000 · 0x7F '.' · H=1.886 · win 256 B
```

The two graphical columns use half-block characters (`▀`), so each text row
carries two byte rows: the foreground paints the upper byte, the background the
lower one. This needs a terminal with 24-bit or 256-colour support.

| Key | Action |
|---|---|
| `Tab` / `Shift+Tab` | Move focus between the three columns |
| `←` `→` | Cursor ±1 byte |
| `↑` `↓` | Cursor ± one row *of the focused column* |
| `PgUp` `PgDn` | One screen of rows |
| `Home` `End` | First / last byte |
| `1` `2` `3` `4` | Focused column's colormap → None / Value / Class / Entropy |
| `Shift`+arrows | Extend the selection |
| `y` / `Y` | Copy the selection as hex / ASCII |
| `g` | Jump to offset (`Enter` submits, `Esc` cancels) |
| `-` / `+` (or `=`) | Halve / double the entropy window |
| `q`, `Esc`, `Ctrl+C` | Save preferences and quit |

`↑`/`↓` follow the focused column's idea of a row, which differs per column: the
hex column's byte row, the zoom column's width in bytes, or — in the overview —
the slice of the file one half-row stands for, making it a coarse whole-file seek.

Copying uses an OSC 52 escape, so it reaches the system clipboard through ssh and
tmux without a clipboard library. Some terminals disable OSC 52, and the write
cannot be confirmed, so a copy always reports success.

## Colormaps

Each column independently picks how it colours bytes, from the `Color: … ▾`
dropdown in its header:

| Mode | Meaning |
|---|---|
| `None` | Nothing is painted. The panel keeps its background but stays fully interactive — this mutes a column rather than disabling it. |
| `Value` | Byte value as greyscale brightness (`0x00` black → `0xFF` white). |
| `Class` | Byte-class palette (after binvis.io): `0x00` black, control cyan, printable ASCII blue, high bytes orange, `0xFF` white. |
| `Entropy` | Shannon entropy over a sliding window, `0.0` deep purple → `4.0` green/cyan → `8.0` red/yellow. |

Defaults are Entropy for the overview, Value for the zoom column and Class for
hex. Entropy is computed per block on a background thread, so a large file
paints immediately and recolours when the pass lands.

## Keyboard

`Ctrl` is the accelerator on Linux and Windows, `Cmd` on macOS.

| Key | Action |
|---|---|
| `Ctrl/Cmd+O` | Open file |
| `Ctrl/Cmd+G` | Jump to offset (hex, `0x` prefix optional) |
| `Ctrl/Cmd+C` | Copy selection as hex |
| `Shift+Ctrl/Cmd+C` | Copy selection as ASCII |
| `Ctrl/Cmd+0` | Reset scroll to the start of the file |
| `Shift+Ctrl/Cmd+L` | Reset the column widths |
| `Ctrl/Cmd+Q` | Save preferences and quit |
| `=` / `-` | Zoom the column under the pointer in / out |
| `←` `→` | Move the cursor one byte |
| `↑` `↓` | Move the cursor one hex row |
| `PageUp` / `PageDown` | Move by one screen of hex rows |
| `Home` / `End` | First / last byte |

In the jump dialog: `Enter` submits, `Esc` cancels, `Ctrl/Cmd+V` pastes.

"Reset all settings" and "clear selection" have no key binding — they are the
top-bar button and `Alt`+right-click respectively.

## Mouse

| Gesture | Effect |
|---|---|
| Hover anywhere | Status bar reads out the offset, byte value and entropy under the pointer |
| Click / drag the overview or the top-bar strip | Navigate to that offset |
| Click a byte in the zoom column | Select it |
| Drag the zoom column | Pan (content follows the cursor) |
| `Ctrl`+wheel over the zoom column | Zoom |
| Drag in hex | Select a byte range |
| Middle-drag, or `Ctrl`/`Alt`+drag, in hex | Pan instead of selecting |
| Right-click in hex | Copy the selection (or hovered byte) as hex |
| `Alt`+right-click in hex | Clear the selection |
| Drag the scrollbar on the hex column's right edge | Scroll all three columns |
| Drag a divider between columns | Resize |
| Wheel | Scroll by whole rows *of the column under the pointer* |

## Preferences

Written to `config.txt` in the platform config directory — on Linux
`$XDG_CONFIG_HOME/parallhex/config.txt` (falling back to `~/.config`), on
Windows `%APPDATA%\parallhex\`, on macOS
`~/Library/Application Support/parallhex/`.

It is a plain `key = value` text file, safe to hand-edit: unknown keys,
malformed lines and non-finite numbers are skipped, and values are clamped on
load, so an edited or older file can never prevent startup.

| Key | Default | Range |
|---|---|---|
| `entropy_window` | `256` | 16–4096 bytes |
| `pixel_zoom` | `4` | 1–24 px per byte |
| `overview_colormap` | `entropy` | `none`, `value`, `class`, `entropy` |
| `zoom_colormap` | `value` | as above |
| `hex_colormap` | `class` | as above |
| `overview_width` | `200` | 140–2000 px |
| `zoom_width` | `320` | 200–3000 px |
| `window_x`, `window_y`, `window_width`, `window_height` | unset | window ≥ 1000×600 |
| `window_maximized` | `false` | `true` / `1` for true |

Changes are saved at most every 2 seconds, and flushed on quit. Window geometry
is restored only if it still intersects a connected display, so unplugging a
monitor cannot strand the window off-screen.

## Development

```sh
cargo test                          # all unit tests
cargo test parse_hex_with_prefix    # one test by name
cargo fmt                           # required: `cargo fmt --check` gates commits
cargo clippy --all-targets          # required: pedantic, warnings denied
```

Tests are inline `#[cfg(test)] mod tests` blocks in each module; there is no
`tests/` directory. The pixel and geometry maths lives in pure functions in
`panes.rs` precisely so it can be tested without opening a window.

Commits are gated by [`prek`](https://prek.j178.dev) (see `prek.toml`), which
runs `cargo test --all-targets`, `cargo fmt --check` and
`cargo clippy --all-targets`. Run all three before committing rather than
discovering failures in the hook.

GitHub Actions runs the same three gates on every push and pull request, then
builds and tests the TUI-only and core-only configurations and asserts gpui stays
out of the TUI's dependency tree (`.github/workflows/ci.yml`). The workflow can be
run locally with [`act`](https://github.com/nektos/act):

```sh
act push -j gates -P ubuntu-24.04=catthehacker/ubuntu:act-24.04
```

Building the **gpui** frontend on Linux needs:

```sh
sudo apt-get install libasound2-dev libfreetype-dev libopus-dev \
                     libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev pkg-config
```

The Wayland and fontconfig bindings are `dlopen`ed, so they matter only at
runtime. The **terminal** frontend needs none of these.

`cargo test` runs the whole suite; `cargo test --no-default-features` runs the
core's own tests with no toolkit compiled at all, which is what keeps `core`
honest about being toolkit-neutral.

`Cargo.toml` denies all compiler warnings and the whole of `clippy::pedantic`.
A short, curated list of exceptions lives in the `#![allow(...)]` at the top of
`src/lib.rs` — see the comment there for why it cannot live in `Cargo.toml`. The
crate's public surface is kept deliberately tiny for the same reason: several
pedantic lints fire only on publicly reachable items.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the module layout and the
invariants worth knowing before changing the rendering code.

## Licence

Licensed under either of

- MIT ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)

at your option — the usual dual licence for Rust projects.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you shall be dual licensed as above, without any
additional terms or conditions.
