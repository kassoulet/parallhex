//! Terminal frontend state and its state machine.
//!
//! Every key becomes an `Action` (see `input`), and `apply` is the only thing that
//! mutates state. Keeping the two apart is what lets the whole interaction model
//! be tested without a terminal.

use std::fs::File;
use std::io;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use memmap2::{Mmap, MmapOptions};

use crate::core::color::Colormap;
use crate::core::config;
use crate::core::entropy;
use crate::core::geom::{self, ByteSource, CopyKind, Nav};

/// Which column has focus. The discriminants index `TuiApp::colormaps`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Focus {
    Overview = 0,
    Zoom = 1,
    Hex = 2,
}

impl Focus {
    const ORDER: [Focus; 3] = [Focus::Overview, Focus::Zoom, Focus::Hex];

    fn step(self, delta: i32) -> Focus {
        let i =
            i32::try_from(Self::ORDER.iter().position(|f| *f == self).unwrap_or(0)).unwrap_or(0);
        let n = i32::try_from(Self::ORDER.len()).unwrap_or(3);
        Self::ORDER[usize::try_from((i + delta).rem_euclid(n)).unwrap_or(0)]
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Focus::Overview => "Overview",
            Focus::Zoom => "Zoom",
            Focus::Hex => "Hex",
        }
    }
}

/// Everything a key press can ask for. Produced by `input::key_to_action` and
/// consumed by `TuiApp::apply`; nothing else mutates state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Action {
    FocusNext,
    FocusPrev,
    Move(Nav),
    Extend(Nav),
    SetColormap(Colormap),
    CopyHex,
    CopyAscii,
    OpenJump,
    JumpChar(char),
    JumpBackspace,
    JumpSubmit,
    JumpCancel,
    EntropyDouble,
    EntropyHalve,
    Quit,
}

/// The measured geometry of the last frame, written by the renderer and read by
/// the input layer so `↑`/`↓` know how long a row of the focused panel is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct PanelLayout {
    pub overview_cols: usize,
    pub zoom_cols: usize,
    pub hex_cols: usize,
    pub text_rows: usize,
}

/// The file's bytes. An enum rather than a trait object so tests can build state
/// over an owned buffer while the real thing maps a file.
pub(crate) enum Bytes {
    Mapped(Mmap),
    #[cfg(test)]
    Owned(Vec<u8>),
}

impl Bytes {
    pub(crate) fn as_slice(&self) -> &[u8] {
        match self {
            Bytes::Mapped(m) => &m[..],
            #[cfg(test)]
            Bytes::Owned(v) => v,
        }
    }
}

pub(crate) struct TuiApp {
    pub data: Arc<Bytes>,
    pub file_size: usize,
    /// The shared byte anchor, exactly as in the gpui frontend: the byte each
    /// panel puts on its centre line.
    pub anchor: usize,
    pub cursor: usize,
    pub selection: Option<Range<usize>>,
    /// Where a shift-extended selection started. `None` once a plain move clears
    /// it, which is what makes an unshifted arrow collapse the selection.
    sel_anchor: Option<usize>,
    pub focus: Focus,
    pub colormaps: [Colormap; 3],
    pub entropy_window: usize,
    pub entropies: Arc<Vec<f32>>,
    pub message: Option<String>,
    pub layout: PanelLayout,
    /// The jump prompt's text while open. `Some` means the prompt owns the
    /// keyboard, which `input` needs to know to treat printable keys as text.
    pub jump: Option<String>,
    /// Set by a copy action; the event loop writes it to the terminal. Keeping the
    /// escape write out of `apply` is what keeps `apply` free of I/O.
    pub last_copied: Option<String>,
    pub quit: bool,
    /// The config as loaded. Retained whole so a save round-trips the settings
    /// only the gpui frontend uses instead of resetting them.
    pub loaded_cfg: config::Config,

    // Background entropy. A whole-file pass takes about a second on a
    // multi-gigabyte file, which would stall the redraw.
    pub entropy_tx: Sender<(u64, Vec<f32>)>,
    entropy_rx: Receiver<(u64, Vec<f32>)>,
    /// Bumped per request so a pass whose window was superseded mid-compute is
    /// discarded rather than applied out of order.
    pub entropy_gen: u64,
    pub entropy_computing: bool,
    /// Set when a request arrives while a pass is in flight, so holding `+`
    /// queues one re-run rather than one pass per keypress.
    pub entropy_pending: bool,
}

impl TuiApp {
    /// Map `path` and build the initial state.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the file cannot be opened, its metadata read, or
    /// the mapping created, and for an empty file — there is nothing to explore
    /// and every downstream index would need a special case.
    pub(crate) fn new(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = usize::try_from(file.metadata()?.len()).unwrap_or(usize::MAX);
        if len == 0 {
            return Err(io::Error::other("file is empty"));
        }
        // SAFETY: the same contract the gpui frontend accepts -- the file may be
        // modified underneath us, which would show as changed bytes, not UB in
        // our own code.
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        let cfg = config::load();
        let (tx, rx) = channel();
        Ok(Self {
            data: Arc::new(Bytes::Mapped(mmap)),
            file_size: len,
            anchor: 0,
            cursor: 0,
            selection: None,
            sel_anchor: None,
            // Hex starts focused: it is the scroll reference, so it is where
            // cursor movement behaves most predictably.
            focus: Focus::Hex,
            colormaps: [cfg.overview_colormap, cfg.zoom_colormap, cfg.hex_colormap],
            entropy_window: cfg
                .entropy_window
                .clamp(geom::ENTROPY_WINDOW_MIN, geom::ENTROPY_WINDOW_MAX),
            entropies: Arc::new(Vec::new()),
            message: None,
            layout: PanelLayout::default(),
            jump: None,
            last_copied: None,
            quit: false,
            loaded_cfg: cfg,
            entropy_tx: tx,
            entropy_rx: rx,
            entropy_gen: 0,
            entropy_computing: false,
            entropy_pending: false,
        })
    }

    /// Bytes covered by one row of `focus`.
    ///
    /// Each panel means something different by "a row", and `↑`/`↓` follow the
    /// focused one:
    /// - hex: whole 8-byte groups that fit its width, one cell per character;
    /// - zoom: its width in cells, since a byte is one half-cell;
    /// - overview: the slice of the file one half-row stands for, which makes
    ///   `↑`/`↓` there a deliberately coarse whole-file seek.
    pub(crate) fn bpr_for(&self, focus: Focus) -> usize {
        match focus {
            Focus::Hex => geom::hex_bytes_per_row(self.layout.hex_cols as f32, 1.0, 0.0),
            Focus::Zoom => self.layout.zoom_cols.max(1),
            Focus::Overview => self
                .file_size
                .div_ceil((self.layout.text_rows * 2).max(1))
                .max(1),
        }
    }

    /// The hex column's row length, which owns the shared anchor's clamp.
    fn hex_bpr(&self) -> usize {
        self.bpr_for(Focus::Hex).max(8)
    }

    pub(crate) fn byte_source(&self, focus: Focus) -> ByteSource<'_> {
        ByteSource {
            data: self.data.as_slice(),
            entropies: &self.entropies,
            entropy_window: self.entropy_window,
            colormap: self.colormaps[focus as usize],
        }
    }

    pub(crate) fn colormap(&self, focus: Focus) -> Colormap {
        self.colormaps[focus as usize]
    }

    /// Apply one action. The only place state changes.
    pub(crate) fn apply(&mut self, action: Action) {
        match action {
            Action::FocusNext => self.focus = self.focus.step(1),
            Action::FocusPrev => self.focus = self.focus.step(-1),
            Action::Move(nav) => {
                self.sel_anchor = None;
                self.selection = None;
                self.move_cursor(nav);
            }
            Action::Extend(nav) => {
                let from = self.sel_anchor.unwrap_or(self.cursor);
                self.sel_anchor = Some(from);
                self.move_cursor(nav);
                let (a, b) = (from.min(self.cursor), from.max(self.cursor) + 1);
                self.selection = Some(a..b.min(self.file_size));
            }
            Action::SetColormap(cm) => self.colormaps[self.focus as usize] = cm,
            Action::CopyHex => self.copy(CopyKind::Hex),
            Action::CopyAscii => self.copy(CopyKind::Ascii),
            Action::OpenJump => {
                self.jump = Some(format!("0x{:X}", self.cursor));
                self.message = None;
            }
            Action::JumpChar(c) => {
                if let Some(s) = self.jump.as_mut() {
                    s.push(c);
                }
            }
            Action::JumpBackspace => {
                if let Some(s) = self.jump.as_mut() {
                    s.pop();
                }
            }
            Action::JumpSubmit => self.submit_jump(),
            Action::JumpCancel => self.jump = None,
            Action::EntropyDouble => self.scale_entropy_window(2),
            Action::EntropyHalve => self.scale_entropy_window(-2),
            Action::Quit => self.quit = true,
        }
    }

    fn move_cursor(&mut self, nav: Nav) {
        if self.file_size == 0 {
            return;
        }
        let bpr = self.bpr_for(self.focus).max(1);
        let page = self.layout.text_rows.max(1) * self.hex_bpr();
        self.cursor = geom::nav_next(
            nav,
            self.cursor.min(self.file_size - 1),
            bpr,
            page,
            self.file_size,
        );
        self.reveal_cursor();
    }

    /// Keep the cursor on screen by re-anchoring to it, then clamp the anchor the
    /// way the gpui frontend does — the hex column is the scroll reference.
    fn reveal_cursor(&mut self) {
        self.anchor = self
            .cursor
            .min(geom::max_anchor(self.file_size, self.hex_bpr()));
    }

    fn copy(&mut self, kind: CopyKind) {
        // With no selection, copy the byte under the cursor, matching the gpui
        // frontend's fallback.
        let range = self
            .selection
            .clone()
            .unwrap_or(self.cursor..self.cursor + 1);
        if let Some(text) = geom::selection_text(self.data.as_slice(), &range, kind) {
            let n = range.end.min(self.file_size) - range.start.min(self.file_size);
            self.message = Some(format!("copied {n} bytes"));
            self.last_copied = Some(text);
        }
    }

    fn submit_jump(&mut self) {
        let Some(text) = self.jump.take() else { return };
        match geom::parse_offset(&text) {
            Some(o) if o < self.file_size => {
                self.cursor = o;
                self.reveal_cursor();
                self.message = None;
            }
            Some(o) => {
                self.message = Some(format!(
                    "offset 0x{o:X} is out of range (file is 0x{:X} bytes)",
                    self.file_size
                ));
            }
            None => self.message = Some("invalid offset".to_owned()),
        }
    }

    /// Double or halve the entropy window, clamped. `factor` is 2 or -2.
    fn scale_entropy_window(&mut self, factor: i32) {
        let next = if factor > 0 {
            self.entropy_window.saturating_mul(2)
        } else {
            self.entropy_window / 2
        }
        .clamp(geom::ENTROPY_WINDOW_MIN, geom::ENTROPY_WINDOW_MAX);
        if next != self.entropy_window {
            self.entropy_window = next;
            self.message = Some(format!("entropy window {next} B"));
            self.request_entropy();
        }
    }

    /// The config to write: everything as loaded, with only what this frontend
    /// owns overwritten. Because `load` reads every field and `save` writes every
    /// field, the gpui-only settings — window geometry, pixel zoom, column widths
    /// — round-trip untouched instead of being reset to defaults.
    pub(crate) fn config_to_save(&self) -> config::Config {
        config::Config {
            overview_colormap: self.colormaps[Focus::Overview as usize],
            zoom_colormap: self.colormaps[Focus::Zoom as usize],
            hex_colormap: self.colormaps[Focus::Hex as usize],
            entropy_window: self.entropy_window,
            ..self.loaded_cfg
        }
    }

    pub(crate) fn save_config(&self) {
        config::save(&self.config_to_save());
    }

    /// Start a whole-file entropy pass on a background thread, or mark one
    /// pending if a pass is already running.
    pub(crate) fn request_entropy(&mut self) {
        if self.entropy_computing {
            self.entropy_pending = true;
            return;
        }
        self.entropy_computing = true;
        self.entropy_gen += 1;
        let generation = self.entropy_gen;
        let window = self.entropy_window;
        let data = Arc::clone(&self.data);
        let tx = self.entropy_tx.clone();
        std::thread::spawn(move || {
            let v = entropy::block_entropies(data.as_slice(), window);
            // A closed receiver just means the app exited first.
            let _ = tx.send((generation, v));
        });
    }

    /// Apply any finished pass. Called once per loop tick.
    pub(crate) fn drain_entropy(&mut self) -> bool {
        let mut applied = false;
        while let Ok((generation, v)) = self.entropy_rx.try_recv() {
            if generation != self.entropy_gen {
                continue; // superseded mid-compute
            }
            self.entropies = Arc::new(v);
            self.entropy_computing = false;
            applied = true;
            if self.entropy_pending {
                self.entropy_pending = false;
                self.request_entropy();
            }
        }
        applied
    }
}

#[cfg(test)]
impl TuiApp {
    /// State over an owned buffer of `len` ascending bytes, so the state machine
    /// is testable without a file or a terminal.
    pub(crate) fn for_test(len: usize) -> Self {
        Self::for_test_data((0..len).map(|i| (i % 256) as u8).collect())
    }

    pub(crate) fn for_test_data(data: Vec<u8>) -> Self {
        let len = data.len();
        let (tx, rx) = channel();
        Self {
            data: Arc::new(Bytes::Owned(data)),
            file_size: len,
            anchor: 0,
            cursor: 0,
            selection: None,
            sel_anchor: None,
            focus: Focus::Hex,
            colormaps: [Colormap::Entropy, Colormap::Value, Colormap::Class],
            entropy_window: 256,
            entropies: Arc::new(Vec::new()),
            message: None,
            layout: PanelLayout {
                overview_cols: 20,
                zoom_cols: 32,
                hex_cols: 80,
                text_rows: 24,
            },
            jump: None,
            last_copied: None,
            quit: false,
            loaded_cfg: config::Config::default(),
            entropy_tx: tx,
            entropy_rx: rx,
            entropy_gen: 0,
            entropy_computing: false,
            entropy_pending: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_panel_defines_its_own_row_length() {
        let app = TuiApp::for_test(4096);
        // Zoom: one byte per half-cell, so a row is the column width.
        assert_eq!(app.bpr_for(Focus::Zoom), 32);
        // Hex: whole 8-byte groups that fit, one cell per character, no gutter.
        assert_eq!(
            app.bpr_for(Focus::Hex),
            geom::hex_bytes_per_row(80.0, 1.0, 0.0)
        );
        // Overview: one half-row stands for this slice of the file, which makes
        // up/down there a coarse whole-file seek rather than a byte step.
        assert_eq!(app.bpr_for(Focus::Overview), 4096_usize.div_ceil(24 * 2));
    }

    #[test]
    fn colormap_action_lands_on_the_focused_panel() {
        let mut app = TuiApp::for_test(4096);
        app.focus = Focus::Zoom;
        app.apply(Action::SetColormap(Colormap::Entropy));
        assert_eq!(app.colormap(Focus::Zoom), Colormap::Entropy);
        // The others are untouched.
        assert_eq!(app.colormap(Focus::Hex), Colormap::Class);
        assert_eq!(app.colormap(Focus::Overview), Colormap::Entropy);
    }

    #[test]
    fn focus_cycles_and_wraps_both_ways() {
        let mut app = TuiApp::for_test(16);
        // Hex starts focused: it is the scroll reference.
        assert_eq!(app.focus, Focus::Hex);
        app.apply(Action::FocusNext);
        assert_eq!(app.focus, Focus::Overview);
        app.apply(Action::FocusPrev);
        assert_eq!(app.focus, Focus::Hex);
        app.apply(Action::FocusPrev);
        assert_eq!(app.focus, Focus::Zoom);
    }

    #[test]
    fn a_plain_move_clears_the_selection_and_shift_extends_it() {
        let mut app = TuiApp::for_test_data(vec![0xDE, 0xAD, 0xBE, 0xEF]);
        app.apply(Action::Extend(Nav::Right));
        assert_eq!(app.selection, Some(0..2));
        app.apply(Action::Extend(Nav::Right));
        assert_eq!(app.selection, Some(0..3));
        app.apply(Action::Move(Nav::Right));
        assert_eq!(app.selection, None, "an unshifted arrow collapses it");
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn copy_falls_back_to_the_byte_under_the_cursor() {
        let mut app = TuiApp::for_test_data(vec![0xDE, 0xAD, 0xBE, 0xEF]);
        app.cursor = 2;
        app.apply(Action::CopyHex);
        assert_eq!(app.last_copied.as_deref(), Some("BE"));
        assert_eq!(app.message.as_deref(), Some("copied 1 bytes"));
    }

    #[test]
    fn copy_uses_the_selection_when_there_is_one() {
        let mut app = TuiApp::for_test_data(vec![0xDE, 0xAD, 0xBE, 0xEF]);
        app.apply(Action::Extend(Nav::Right));
        app.apply(Action::Extend(Nav::Right));
        app.apply(Action::CopyHex);
        assert_eq!(app.last_copied.as_deref(), Some("DE AD BE"));
        app.apply(Action::CopyAscii);
        assert_eq!(app.last_copied.as_deref(), Some("..."));
    }

    #[test]
    fn a_submitted_offset_moves_the_cursor() {
        let mut app = TuiApp::for_test(0x1000);
        app.apply(Action::OpenJump);
        // The prompt is prefilled with the cursor, so clear it first.
        app.jump = Some(String::new());
        for c in "0x2_0".chars() {
            app.apply(Action::JumpChar(c));
        }
        app.apply(Action::JumpSubmit);
        assert_eq!(app.cursor, 0x20);
        assert!(app.jump.is_none());
    }

    #[test]
    fn an_out_of_range_offset_reports_instead_of_moving() {
        let mut app = TuiApp::for_test(0x10);
        app.apply(Action::OpenJump);
        app.jump = Some("FFFF".to_owned());
        app.apply(Action::JumpSubmit);
        assert_eq!(app.cursor, 0, "cursor must not move");
        assert!(
            app.message
                .as_deref()
                .unwrap_or("")
                .contains("out of range"),
            "got {:?}",
            app.message
        );
        assert!(app.jump.is_none());
    }

    #[test]
    fn cancel_discards_the_prompt() {
        let mut app = TuiApp::for_test(0x1000);
        app.apply(Action::OpenJump);
        app.apply(Action::JumpChar('9'));
        app.apply(Action::JumpCancel);
        assert!(app.jump.is_none());
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn the_entropy_window_doubles_and_halves_within_its_clamp() {
        let mut app = TuiApp::for_test(4096);
        app.entropy_window = geom::ENTROPY_WINDOW_MAX;
        app.apply(Action::EntropyDouble);
        assert_eq!(app.entropy_window, geom::ENTROPY_WINDOW_MAX, "clamped high");
        app.entropy_window = geom::ENTROPY_WINDOW_MIN;
        app.apply(Action::EntropyHalve);
        assert_eq!(app.entropy_window, geom::ENTROPY_WINDOW_MIN, "clamped low");
        app.entropy_window = 256;
        app.apply(Action::EntropyDouble);
        assert_eq!(app.entropy_window, 512);
        app.apply(Action::EntropyHalve);
        assert_eq!(app.entropy_window, 256);
    }

    #[test]
    fn a_stale_entropy_result_is_dropped() {
        let mut app = TuiApp::for_test(4096);
        app.entropy_gen = 7;
        // Tagged with an older generation: its window was superseded.
        app.entropy_tx.send((3, vec![1.0, 2.0])).unwrap();
        app.drain_entropy();
        assert!(app.entropies.is_empty(), "stale generation was applied");
        app.entropy_tx.send((7, vec![4.0])).unwrap();
        app.drain_entropy();
        assert_eq!(&**app.entropies, &[4.0]);
    }

    #[test]
    fn holding_the_key_coalesces_instead_of_queueing() {
        let mut app = TuiApp::for_test(4096);
        app.entropy_computing = true;
        let gen_before = app.entropy_gen;
        app.apply(Action::EntropyDouble);
        app.apply(Action::EntropyDouble);
        app.apply(Action::EntropyDouble);
        // One pass in flight, one queued -- never three.
        assert!(app.entropy_pending);
        assert_eq!(
            app.entropy_gen, gen_before,
            "no new pass started while one was in flight"
        );
    }

    #[test]
    fn saving_preserves_the_gpui_only_settings() {
        // The TUI must round-trip window geometry and pixel zoom, or it would
        // silently reset the gpui app's window position on every exit.
        let mut app = TuiApp::for_test(16);
        app.loaded_cfg.window_bounds = Some((10.0, 20.0, 1600.0, 900.0));
        app.loaded_cfg.pixel_zoom = 7.0;
        app.loaded_cfg.overview_width = 321.0;
        app.colormaps[Focus::Hex as usize] = Colormap::Entropy;
        app.entropy_window = 1024;

        let out = app.config_to_save();
        assert_eq!(out.window_bounds, Some((10.0, 20.0, 1600.0, 900.0)));
        assert_eq!(out.pixel_zoom, 7.0);
        assert_eq!(out.overview_width, 321.0);
        assert_eq!(out.hex_colormap, Colormap::Entropy);
        assert_eq!(out.entropy_window, 1024);
    }

    #[test]
    fn the_cursor_stays_visible_and_the_anchor_stays_clamped() {
        let mut app = TuiApp::for_test(4096);
        app.apply(Action::Move(Nav::End));
        assert_eq!(app.cursor, 4095);
        let last = geom::max_anchor(4096, app.bpr_for(Focus::Hex).max(8));
        assert!(app.anchor <= last, "anchor {} > {last}", app.anchor);
        app.apply(Action::Move(Nav::Home));
        assert_eq!(app.cursor, 0);
        assert_eq!(app.anchor, 0);
    }
}
