use std::path::Path;

use crossterm::event::{Event, KeyCode, KeyModifiers};
use tui_input::backend::crossterm::EventHandler;

use crate::app::{App, Mode, Pane, PrefixState};
use crate::keys;
use crate::storage::persist;

/// Handles keys while the SSH extra-options sub-dialog is open.
///
/// The extras UI has two nested states: list navigation and key/value entry.
/// This function routes keys to the appropriate state so the main host form
/// handler does not need to understand the inner editor details.
pub fn handle_extras_key(app: &mut App, ev: &Event, code: KeyCode) {
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

/// Handles keyboard input while the host browser is active.
///
/// This covers normal navigation, search, modal forms, confirmations, and the
/// Ctrl+T tab prefix when sessions exist. It persists immediately after actions
/// that transition back to normal mode with dirty config.
pub fn handle_hosts_key(
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
                KeyCode::Char('j') | KeyCode::Down => {
                    if app.focus == Pane::Groups {
                        app.cancel_search()
                    }
                    app.move_down()
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if app.focus == Pane::Groups {
                        app.cancel_search()
                    }
                    app.move_up()
                }
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

/// Handles keyboard input while an embedded SSH session is active.
///
/// Hosttui reserves Ctrl+T as a prefix for tab/session commands. All other keys
/// are encoded into terminal bytes and forwarded to the active PTY. Pressing
/// Ctrl+T twice sends a literal Ctrl+T to the remote program.
pub fn handle_session_key(app: &mut App, key: &crossterm::event::KeyEvent) {
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

/// Replays pasted text through host-view key handling.
///
/// This preserves validation/error-clearing behavior because forms and search
/// inputs see the same events they would receive from typed characters.
pub fn handle_hosts_paste(app: &mut App, text: &str, path: &Path) -> anyhow::Result<()> {
    for ch in text.chars() {
        let key = keys::paste_key(ch);
        let ev = Event::Key(key);
        handle_hosts_key(app, &ev, key.code, key.modifiers, path)?;
    }
    Ok(())
}
