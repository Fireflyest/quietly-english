//! Terminal lifecycle: alternate screen, raw mode, mouse capture.

use std::io::{self, Stdout};

use anyhow::{Context, Result};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

pub struct TerminalSession {
    pub terminal: Tui,
    restored: bool,
    /// Whether the app is currently receiving mouse events.
    mouse: bool,
}

impl TerminalSession {
    pub fn new() -> Result<Self> {
        enable_raw_mode().context("cannot enter raw mode: run this in a real terminal")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture, Hide)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        // `Terminal::new` starts from a blank internal buffer, so it believes the
        // screen is already empty and only paints changed cells. Anything the
        // terminal or a previous process left behind would survive. Wipe both the
        // real screen and the buffer before the first frame.
        terminal.clear()?;
        Ok(Self {
            terminal,
            restored: false,
            mouse: true,
        })
    }

    /// Force a full repaint. Needed after a resize, where the old frame would
    /// otherwise linger in the columns or rows the new layout no longer covers.
    pub fn clear(&mut self) -> Result<()> {
        self.terminal.clear()?;
        Ok(())
    }

    /// Turn mouse reporting on or off.
    ///
    /// While it is on the app receives scroll, click and drag events, but the
    /// terminal can no longer select text. Releasing it hands the mouse back so the
    /// user can copy from the transcript.
    pub fn set_mouse(&mut self, on: bool) -> Result<()> {
        if self.mouse == on {
            return Ok(());
        }
        let mut stdout = io::stdout();
        if on {
            execute!(stdout, EnableMouseCapture)?;
        } else {
            execute!(stdout, DisableMouseCapture)?;
        }
        self.mouse = on;
        Ok(())
    }

    pub fn mouse_enabled(&self) -> bool {
        self.mouse
    }

    pub fn restore(&mut self) {
        if self.restored {
            return;
        }
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture, Show);
        self.restored = true;
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Make sure a panic does not leave the terminal in the alternate screen.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            Show
        );
        default(info);
    }));
}
