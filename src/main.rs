use std::path::Path;
use std::time::Duration;

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton,
    MouseEventKind,
};
use ratatui::layout::Rect;
use tui_input::backend::crossterm::EventHandler;

use hosttui::app::{App, Mode, Pane, PrefixState, ScreenPos, Selection, View};
use hosttui::keys;
use hosttui::model::Config;
use hosttui::sshconfig;
use hosttui::storage;
use hosttui::ui;

/// Handles keys while the SSH extra-options sub-dialog is open.
///
/// The extras UI has two nested states: list navigation and key/value entry.
/// This function routes keys to the appropriate state so the main host form
/// handler does not need to understand the inner editor details.
fn handle_extras_key(app: &mut App, ev: &Event, code: KeyCode) {
    let Some(form) = app.form_state_mut() else {
        return;
    };
    let Some(ed) = form.extras_editor.as_mut() else {
        return;
    };

    if ed.entry.is_some() {
        match code {
            KeyCode::Esc => form.extras_cancel_entry(),
            KeyCode::Enter => {
                form.extras_commit_entry();
            }
            KeyCode::Tab | KeyCode::BackTab => {
                if let Some(entry) = ed.entry.as_mut() {
                    entry.toggle_field();
                }
            }
            _ => {
                if let Some(entry) = ed.entry.as_mut()
                    && entry.active_input().handle_event(ev).is_some()
                {
                    ed.error = None;
                }
            }
        }
    } else {
        match code {
            KeyCode::Esc => form.close_extras(),
            KeyCode::Char('a') => form.extras_begin_add(),
            KeyCode::Char('e') => form.extras_begin_edit(),
            KeyCode::Char('d') => form.extras_delete_selected(),
            KeyCode::Down | KeyCode::Char('j') => form.extras_move_down(),
            KeyCode::Up | KeyCode::Char('k') => form.extras_move_up(),
            _ => {}
        }
    }
}

/// Persists both hosttui's source config and the generated OpenSSH fragment.
///
/// App methods only set `dirty`; the event layer calls this after successful
/// mutations so disk writes stay centralized and both files remain in sync.
fn persist(path: &Path, config: &Config) -> anyhow::Result<()> {
    storage::save(path, config)?;
    let ssh_path = sshconfig::ssh_config_path()?;
    sshconfig::export(&ssh_path, config)?;
    Ok(())
}

/// Handles keyboard input while the host browser is active.
///
/// This covers normal navigation, search, modal forms, confirmations, and the
/// Ctrl+T tab prefix when sessions exist. It persists immediately after actions
/// that transition back to normal mode with dirty config.
fn handle_hosts_key(
    app: &mut App,
    ev: &Event,
    code: KeyCode,
    modifiers: KeyModifiers,
    path: &Path,
) -> anyhow::Result<()> {
    match &app.mode {
        Mode::Normal => {
            if matches!(app.prefix, PrefixState::Pending) {
                app.prefix = PrefixState::Inactive;
                match code {
                    KeyCode::Char('h') | KeyCode::Char('0') => {}
                    KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                        let idx = (c as usize) - ('1' as usize);
                        app.switch_to_session(idx);
                    }
                    KeyCode::Char('n') => app.next_tab(),
                    KeyCode::Char('p') => app.prev_tab(),
                    KeyCode::Char('?') => app.mode = Mode::TabHelp,
                    _ => {}
                }
                return Ok(());
            }
            match code {
                KeyCode::Char('q') => app.exit = true,
                KeyCode::Esc => {
                    if app.search.value().is_empty() {
                        app.exit = true;
                    } else {
                        app.cancel_search();
                    }
                }
                KeyCode::Char('j') | KeyCode::Down => app.move_down(),
                KeyCode::Char('k') | KeyCode::Up => app.move_up(),
                KeyCode::Tab => app.toggle_focus(),
                KeyCode::Right => app.host_focus(),
                KeyCode::Left => app.group_focus(),
                KeyCode::Enter if app.focus == Pane::Hosts => {
                    let (cols, rows) = crossterm::terminal::size()?;
                    app.open_session(rows.saturating_sub(3), cols.saturating_sub(2));
                    if app.dirty {
                        persist(path, &app.config)?;
                        app.dirty = false;
                    }
                }
                KeyCode::Char('a') if app.focus == Pane::Hosts => app.start_adding(),
                KeyCode::Char('c') if app.focus == Pane::Hosts => app.start_add_from_host(),
                KeyCode::Char('e') if app.focus == Pane::Hosts => app.start_editing(),
                KeyCode::Char('e') if app.focus == Pane::Groups => app.start_editing_group(),
                KeyCode::Char('d') => app.start_delete(),
                KeyCode::Char('g') if app.focus == Pane::Groups => {
                    app.start_adding_group();
                }
                KeyCode::Char('/') => app.start_search(),
                KeyCode::Char('t')
                    if modifiers.contains(KeyModifiers::CONTROL) && app.has_active_sessions() =>
                {
                    app.prefix = PrefixState::Pending;
                }
                KeyCode::Char('t')
                    if !modifiers.contains(KeyModifiers::CONTROL) && app.focus == Pane::Hosts =>
                {
                    app.test_host()
                }
                _ => {}
            }
        }
        Mode::Searching => match code {
            KeyCode::Esc => app.cancel_search(),
            KeyCode::Enter => app.commit_search(),
            KeyCode::Down => app.move_down(),
            KeyCode::Up => app.move_up(),
            _ => {
                if app.search.handle_event(ev).is_some() {
                    app.refresh_search();
                }
            }
        },
        Mode::Adding(_) | Mode::Editing { .. } => {
            let extras_open = app
                .form_state_mut()
                .map(|f| f.extras_editor.is_some())
                .unwrap_or(false);

            if extras_open {
                handle_extras_key(app, ev, code);
            } else {
                match code {
                    KeyCode::Esc => app.cancel_mode(),
                    KeyCode::Enter => {
                        app.submit_form();
                        if matches!(app.mode, Mode::Normal) && app.dirty {
                            persist(path, &app.config)?;
                            app.dirty = false;
                        }
                    }
                    KeyCode::Tab | KeyCode::Down => {
                        if let Some(form) = app.form_state_mut() {
                            form.next_field();
                        }
                    }
                    KeyCode::BackTab | KeyCode::Up => {
                        if let Some(form) = app.form_state_mut() {
                            form.prev_field();
                        }
                    }
                    KeyCode::Char('k') if modifiers.contains(KeyModifiers::CONTROL) => {
                        if let Some(form) = app.form_state_mut() {
                            form.open_extras();
                        }
                    }
                    _ => {
                        if let Some(form) = app.form_state_mut()
                            && form.active_input().handle_event(ev).is_some()
                        {
                            form.error = None;
                        }
                    }
                }
            }
        }
        Mode::AddingGroup(_) | Mode::EditingGroup { .. } => match code {
            KeyCode::Esc => app.cancel_mode(),
            KeyCode::Enter => {
                app.submit_form();
                if matches!(app.mode, Mode::Normal) && app.dirty {
                    persist(path, &app.config)?;
                    app.dirty = false;
                }
            }
            _ => {
                if let Some(input) = app.input_state_mut()
                    && input.buffer.handle_event(ev).is_some()
                {
                    input.error = None;
                }
            }
        },
        Mode::ConnectError { .. } | Mode::TestResult { .. } => match code {
            KeyCode::Enter | KeyCode::Esc => app.cancel_mode(),
            _ => {}
        },
        Mode::TabHelp => {
            app.cancel_mode();
        }
        Mode::ConfirmDelete(_) | Mode::ConfirmDeleteGroup(_) => match code {
            KeyCode::Char('y') => {
                app.confirm_delete();
                persist(path, &app.config)?;
                app.dirty = false;
            }
            KeyCode::Char('n') | KeyCode::Esc => app.cancel_mode(),
            _ => {}
        },
    }
    Ok(())
}

/// Calculates the terminal-screen rectangle inside the session border and tabs.
///
/// Mouse events arrive in frame coordinates, while PTY selection and rendering
/// use inner terminal coordinates. This helper mirrors the layout used by
/// `ui::render_session_view` so selection math matches what the user sees.
fn session_inner_rect(cols: u16, rows: u16, has_tabs: bool) -> Rect {
    let tab_h = if has_tabs { 1u16 } else { 0 };
    Rect {
        x: 1,
        y: 1,
        width: cols.saturating_sub(2),
        height: rows.saturating_sub(2 + tab_h),
    }
}

/// Converts a frame coordinate into a terminal-screen coordinate if it is inside.
fn frame_to_screen(col: u16, row: u16, inner: Rect) -> Option<ScreenPos> {
    if col >= inner.x
        && col < inner.x + inner.width
        && row >= inner.y
        && row < inner.y + inner.height
    {
        Some(ScreenPos {
            row: row - inner.y,
            col: col - inner.x,
        })
    } else {
        None
    }
}

/// Converts a frame coordinate into the nearest terminal-screen coordinate.
///
/// Drag selections should continue to update even if the mouse leaves the
/// terminal area, so this clamps to the edge instead of returning `None`.
fn frame_to_screen_clamped(col: u16, row: u16, inner: Rect) -> ScreenPos {
    ScreenPos {
        col: col
            .max(inner.x)
            .min(inner.x + inner.width.saturating_sub(1))
            - inner.x,
        row: row
            .max(inner.y)
            .min(inner.y + inner.height.saturating_sub(1))
            - inner.y,
    }
}

/// Copies the active terminal selection to the system clipboard.
///
/// The selection is read from the parsed vt100 screen, not from ratatui's frame
/// buffer. After a successful copy the selection is cleared and a short-lived
/// notification is shown in the session view.
fn copy_selection(app: &mut App) {
    let Some(sel) = app.selection else { return };
    if sel.anchor == sel.end {
        app.clear_selection();
        return;
    }

    let (start, end) = sel.normalized();
    let mut copied = false;
    if let View::Session(idx) = app.view
        && let Some(session) = app.sessions.get(idx)
    {
        let screen = session.screen();
        let text = screen.contents_between(start.row, start.col, end.row, end.col);
        if !text.is_empty()
            && let Ok(mut clipboard) = arboard::Clipboard::new()
        {
            copied = clipboard.set_text(text).is_ok();
        }
    }
    app.clear_selection();
    if copied {
        app.show_clipboard_notice();
    }
}

/// Handles keyboard input while an embedded SSH session is active.
///
/// Hosttui reserves Ctrl+T as a prefix for tab/session commands. All other keys
/// are encoded into terminal bytes and forwarded to the active PTY. Pressing
/// Ctrl+T twice sends a literal Ctrl+T to the remote program.
fn handle_session_key(app: &mut App, key: &crossterm::event::KeyEvent) {
    app.clear_selection();
    if matches!(app.mode, Mode::TabHelp) {
        app.cancel_mode();
        return;
    }
    match app.prefix {
        PrefixState::Pending => {
            app.prefix = PrefixState::Inactive;
            match key.code {
                KeyCode::Char('h') | KeyCode::Char('0') => app.switch_to_hosts(),
                KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                    let idx = (c as usize) - ('1' as usize);
                    app.switch_to_session(idx);
                }
                KeyCode::Char('n') => app.next_tab(),
                KeyCode::Char('p') => app.prev_tab(),
                KeyCode::Char('x') => app.close_current_session(),
                KeyCode::Char('?') => app.mode = Mode::TabHelp,
                KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(session) = app.active_session_mut() {
                        session.write(&[0x14]);
                    }
                }
                _ => {}
            }
        }
        PrefixState::Inactive => {
            if key.code == KeyCode::Char('t') && key.modifiers.contains(KeyModifiers::CONTROL) {
                app.prefix = PrefixState::Pending;
            } else if let Some(bytes) = keys::encode(key)
                && let Some(session) = app.active_session_mut()
            {
                session.write(&bytes);
            }
        }
    }
}

/// Converts one pasted character into a synthetic key event for host forms.
///
/// `tui-input` consumes crossterm key events rather than raw paste strings, so
/// host-view paste is replayed character-by-character through the normal key
/// handling path. Session paste uses raw PTY paste handling instead.
fn paste_key(ch: char) -> KeyEvent {
    let code = match ch {
        '\n' | '\r' => KeyCode::Enter,
        '\t' => KeyCode::Tab,
        ch => KeyCode::Char(ch),
    };
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

/// Replays pasted text through host-view key handling.
///
/// This preserves validation/error-clearing behavior because forms and search
/// inputs see the same events they would receive from typed characters.
fn handle_hosts_paste(app: &mut App, text: &str, path: &Path) -> anyhow::Result<()> {
    for ch in text.chars() {
        let key = paste_key(ch);
        let ev = Event::Key(key);
        handle_hosts_key(app, &ev, key.code, key.modifiers, path)?;
    }
    Ok(())
}

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
        app.clear_expired_notifications();

        terminal.draw(|frame| ui::render(frame, app))?;

        let timeout = if app.has_active_sessions() || matches!(app.mode, Mode::TestResult { .. }) {
            Duration::from_millis(16)
        } else {
            Duration::from_secs(1)
        };

        if !event::poll(timeout)? {
            continue;
        }

        let ev = event::read()?;

        if let Event::Resize(cols, rows) = ev {
            let (session_rows, session_cols) = if app.has_active_sessions() {
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
                    View::Hosts => {}
                    View::Session(idx) => {
                        app.clear_selection();
                        if let Some(session) = app.sessions.get(idx) {
                            session.scroll_up(3);
                        }
                    }
                },
                MouseEventKind::ScrollDown => match app.view {
                    View::Hosts => {}
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
                        let inner = session_inner_rect(cols, rows, app.has_active_sessions());
                        if let Some(pos) = frame_to_screen(mouse.column, mouse.row, inner) {
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
                    let has_tabs = app.has_active_sessions();
                    if let Some(sel) = app.selection.as_mut() {
                        let (cols, rows) = crossterm::terminal::size()?;
                        let inner = session_inner_rect(cols, rows, has_tabs);
                        sel.end = frame_to_screen_clamped(mouse.column, mouse.row, inner);
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    if let Some(sel) = app.selection {
                        let (cols, rows) = crossterm::terminal::size()?;
                        let inner = session_inner_rect(cols, rows, app.has_active_sessions());
                        app.selection = Some(Selection {
                            anchor: sel.anchor,
                            end: frame_to_screen_clamped(mouse.column, mouse.row, inner),
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
