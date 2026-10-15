//! OCA TUI — Ratatui frontend (AGENTS rule 8: zero business logic; every
//! mutation goes through `oca_core::Core`, every render input comes from it).

mod app;
mod editor;

pub use app::App;
pub use editor::{Editor, KeyOutcome};

use std::io;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    read,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use oca_audio::CpalHost;
use oca_core::Core;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// Restores the terminal even when setup or the event loop fails midway.
struct TermGuard;

impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            DisableBracketedPaste
        );
    }
}

/// Run the interactive editor on an already-open project.
pub fn run(core: &mut Core) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let _guard = TermGuard;

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let mut app = App::new(core, Arc::new(CpalHost));
    loop {
        terminal.draw(|frame| app.draw(core, frame))?;
        if app.should_quit() {
            break;
        }
        if crossterm::event::poll(Duration::from_millis(250))? {
            match read()? {
                Event::Key(key) => app.on_key(core, key)?,
                Event::Mouse(mouse) => app.on_mouse(core, mouse),
                Event::Paste(text) => app.on_paste(core, &text),
                _ => {}
            }
        }
        app.tick(core)?;
    }
    let _ = terminal.show_cursor();
    Ok(())
}
