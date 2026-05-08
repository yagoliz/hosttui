use std::path::Path;
use std::time::Duration;

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind, MouseButton, MouseEventKind,
};

use hosttui::app::{App, Mode, Selection, View, copy_selection};
use hosttui::handlers::{
    handle_file_transfer_key, handle_file_transfer_paste, handle_hosts_key, handle_hosts_paste,
    handle_session_key,
};
use hosttui::storage;
use hosttui::ui;

/// Main application event loop.
///
/// Each iteration refreshes runtime session state, draws a frame, then waits for
/// terminal events. Active sessions use a short poll timeout so PTY output and
/// transient notifications repaint smoothly even when the user is not pressing
/// keys.
fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App, path: &Path) -> anyhow::Result<()> {
    while !app.exit {
        for session in &mut app.sessions {
            session.update_status();
        }
        app.close_exited_sessions();
        app.poll_file_transfers();
        app.clear_expired_notifications();

        terminal.draw(|frame| ui::render(frame, app))?;

        let timeout = if app.has_tabs() || matches!(app.mode, Mode::TestResult { .. }) {
            Duration::from_millis(16)
        } else {
            Duration::from_secs(1)
        };

        if !event::poll(timeout)? {
            continue;
        }

        let ev = event::read()?;

        if let Event::Resize(cols, rows) = ev {
            let (session_rows, session_cols) = if app.has_tabs() {
                (rows.saturating_sub(3), cols.saturating_sub(2))
            } else {
                (rows, cols)
            };
            for session in &app.sessions {
                session.resize(session_rows, session_cols);
            }
            continue;
        }

        if let Event::Mouse(mouse) = ev {
            match mouse.kind {
                MouseEventKind::ScrollUp => match app.view {
                    View::Hosts | View::FileTransfer(_) => {}
                    View::Session(idx) => {
                        app.clear_selection();
                        if let Some(session) = app.sessions.get(idx) {
                            session.scroll_up(3);
                        }
                    }
                },
                MouseEventKind::ScrollDown => match app.view {
                    View::Hosts | View::FileTransfer(_) => {}
                    View::Session(idx) => {
                        app.clear_selection();
                        if let Some(session) = app.sessions.get(idx) {
                            session.scroll_down(3);
                        }
                    }
                },
                MouseEventKind::Down(MouseButton::Left) => {
                    if matches!(app.view, View::Session(_)) {
                        let (cols, rows) = crossterm::terminal::size()?;
                        let inner = ui::session_inner_rect(cols, rows, app.has_tabs());
                        if let Some(pos) = ui::frame_to_screen(mouse.column, mouse.row, inner) {
                            app.selection = Some(Selection {
                                anchor: pos,
                                end: pos,
                            });
                        } else {
                            app.clear_selection();
                        }
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    let has_tabs = app.has_tabs();
                    if let Some(sel) = app.selection.as_mut() {
                        let (cols, rows) = crossterm::terminal::size()?;
                        let inner = ui::session_inner_rect(cols, rows, has_tabs);
                        sel.end = ui::frame_to_screen_clamped(mouse.column, mouse.row, inner);
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    if let Some(sel) = app.selection {
                        let (cols, rows) = crossterm::terminal::size()?;
                        let inner = ui::session_inner_rect(cols, rows, app.has_tabs());
                        app.selection = Some(Selection {
                            anchor: sel.anchor,
                            end: ui::frame_to_screen_clamped(mouse.column, mouse.row, inner),
                        });
                        copy_selection(app);
                    }
                }
                _ => {}
            }
            continue;
        }

        if let Event::Paste(text) = ev {
            match app.view {
                View::Hosts => handle_hosts_paste(app, &text, path)?,
                View::Session(_) => {
                    app.clear_selection();
                    if let Some(session) = app.active_session_mut() {
                        session.paste(&text);
                    }
                }
                View::FileTransfer(_) => handle_file_transfer_paste(app, &text),
            }
            continue;
        }

        if let Event::Key(key) = ev {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match app.view {
                View::Hosts => handle_hosts_key(app, &ev, key.code, key.modifiers, path)?,
                View::Session(_) => handle_session_key(app, &key),
                View::FileTransfer(_) => {
                    handle_file_transfer_key(app, &ev, key.code, key.modifiers)
                }
            }
        }
    }
    Ok(())
}

/// Binary entry point.
///
/// Terminal setup and teardown are kept in one function so mouse capture,
/// bracketed paste, and ratatui raw-screen state are restored even if `run`
/// returns an error.
fn main() -> anyhow::Result<()> {
    let path = storage::config_path()?;
    let config = storage::load(&path)?;
    let mut app = App::new(config);

    let mut terminal = ratatui::init();
    crossterm::execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let result = run(&mut terminal, &mut app, &path);
    let _ = crossterm::execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture
    );
    ratatui::restore();
    result
}
