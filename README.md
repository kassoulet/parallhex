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

## Build and run

Requires a Rust toolchain supporting edition 2024.

```sh
cargo run                       # launch with no file
cargo run -- path/to/file.bin   # open a file on startup
cargo run --release             # for large files, prefer release
```

`--` is needed before a filename that begins with a dash. `-h` / `--help`
prints usage. Extra positional arguments after the first are ignored.

```
Usage: parallhex-gpui [OPTIONS] [FILE]
```

The executable is `parallhex-gpui`, named for its toolkit so it can sit alongside
another `parallhex` build. The preferences directory stays `parallhex`.

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

GitHub Actions runs the same three gates on every push and pull request
(`.github/workflows/ci.yml`). Building on Linux needs:

```sh
sudo apt-get install libasound2-dev libfreetype-dev libopus-dev \
                     libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev pkg-config
```

The Wayland and fontconfig bindings are `dlopen`ed, so they matter only at
runtime. The workflow can be run locally with
[`act`](https://github.com/nektos/act):

```sh
act push -j gates -P ubuntu-24.04=catthehacker/ubuntu:act-24.04
```

`Cargo.toml` denies all compiler warnings and the whole of `clippy::pedantic`.
A short, curated list of exceptions lives in the `#![allow(...)]` at the top of
`src/main.rs` — see the comment there for why it cannot live in `Cargo.toml`.

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
