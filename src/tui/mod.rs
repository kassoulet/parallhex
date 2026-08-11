//! The terminal frontend, built on ratatui.
//!
//! Renders the same three synchronized columns as the gpui frontend over the same
//! `core` geometry. The two graphical columns become half-block characters: two
//! byte rows per text row, `fg` the upper byte and `bg` the lower one.

pub(crate) mod app;
pub(crate) mod blit;
pub(crate) mod input;
pub(crate) mod render;

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use crate::tui::app::TuiApp;

type Term = Terminal<CrosstermBackend<io::Stdout>>;

/// Run the terminal UI against `file`.
///
/// # Errors
///
/// Returns the underlying `io::Error` if no path was given, if the file cannot be
/// opened or mapped, or if the terminal cannot be driven.
pub fn run(file: Option<PathBuf>) -> io::Result<()> {
    // Unlike the gpui frontend there is no file dialog to fall back on, so a path
    // is required rather than optional.
    let Some(path) = file else {
        return Err(io::Error::other(
            "no file given; usage: parallhex-tui [FILE]",
        ));
    };
    let mut app = TuiApp::new(&path)?;
    // Paint immediately with a flat colormap; entropy arrives from the thread.
    app.request_entropy();

    install_panic_hook();
    let mut term = setup_terminal()?;
    let result = event_loop(&mut term, &mut app);
    // Restore before propagating, so an error is not printed into a raw
    // alternate screen.
    restore_terminal()?;
    app.save_config();
    result
}

/// Restore the terminal, then defer to the previous hook.
///
/// Without this a panic leaves the terminal in raw mode on the alternate screen,
/// which is the signature TUI failure and needs `reset` to recover from.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        previous(info);
    }));
}

fn setup_terminal() -> io::Result<Term> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(out))
}

fn restore_terminal() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}

fn event_loop(term: &mut Term, app: &mut TuiApp) -> io::Result<()> {
    while !app.quit {
        term.draw(|f| render::draw(f, app))?;

        // A short poll rather than a blocking read, so a finished entropy pass is
        // picked up and redrawn without needing a keystroke to wake the loop.
        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
            // Windows and some terminals report Press *and* Release, which would
            // otherwise double every keystroke.
            && key.kind == KeyEventKind::Press
            && let Some(action) = input::key_to_action(key, app.jump.is_some(), app.focus)
        {
            app.apply(action);
        }
        app.drain_entropy();

        // The copy escape is written here rather than in `apply`, which keeps the
        // state machine free of I/O and therefore testable.
        if let Some(text) = app.last_copied.take() {
            let seq = blit::osc52(&text);
            let mut out = io::stdout();
            out.write_all(seq.as_bytes())?;
            out.flush()?;
        }
    }
    Ok(())
}
