use std::path::Path;
use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyModifiers};
use tui_input::Input;
use tui_input::backend::crossterm::EventHandler;

use crate::app::{App, Mode, Pane, PrefixState, View};
use crate::filebrowser::{FileBrowserMode, FileBrowserPane};
use crate::keys;
use crate::sftp::{ConnectOutcome, SftpConnection, SftpConnectionStatus};
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
                    if modifiers.contains(KeyModifiers::CONTROL) && app.has_tabs() =>
                {
                    app.prefix = PrefixState::Pending;
                }
                KeyCode::Char('t')
                    if !modifiers.contains(KeyModifiers::CONTROL) && app.focus == Pane::Hosts =>
                {
                    app.test_host()
                }
                KeyCode::Char('f') if app.focus == Pane::Hosts => {
                    app.open_file_transfer();
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
                KeyCode::Char('f') => {
                    if let View::Session(idx) = app.view
                        && let Some(session) = app.sessions.get(idx)
                    {
                        let alias = session.alias.clone();
                        if let Some(host) = app.config.find(&alias).cloned() {
                            app.open_file_transfer_for_host(host);
                        }
                    }
                }
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

/// Handles keyboard input while a file transfer view is active.
///
/// Supports navigation in both local/remote panes, directory entry, parent
/// traversal, pane switching, sort cycling, hidden file toggling, and the
/// Ctrl+T tab prefix for switching between views. Modal sub-states like
/// password prompt and mkdir are handled inline.
pub fn handle_file_transfer_key(app: &mut App, ev: &Event, code: KeyCode, modifiers: KeyModifiers) {
    // Handle Ctrl+T prefix first — shared with session/hosts views
    if matches!(app.prefix, PrefixState::Pending) {
        app.prefix = PrefixState::Inactive;
        match code {
            KeyCode::Char('h') | KeyCode::Char('0') => app.switch_to_hosts(),
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                let idx = (c as usize) - ('1' as usize);
                app.switch_to_session(idx);
            }
            KeyCode::Char('n') => app.next_tab(),
            KeyCode::Char('p') => app.prev_tab(),
            KeyCode::Char('x') => app.close_current_file_transfer(),
            KeyCode::Char('?') => app.mode = Mode::TabHelp,
            _ => {}
        }
        return;
    }

    let Some(ft) = app.active_file_transfer_mut() else {
        return;
    };

    match &ft.mode {
        FileBrowserMode::Normal => {
            handle_file_transfer_normal(app, code, modifiers);
        }
        FileBrowserMode::PasswordPrompt(_) => {
            handle_file_transfer_password(app, ev, code);
        }
        FileBrowserMode::Creating(_) => {
            handle_file_transfer_mkdir(app, ev, code);
        }
        FileBrowserMode::ConfirmTransfer { .. } => {
            handle_file_transfer_confirm(app, code);
        }
        FileBrowserMode::TransferError(_) => match code {
            KeyCode::Enter | KeyCode::Esc => {
                let ft = app.active_file_transfer_mut().unwrap();
                ft.mode = FileBrowserMode::Normal;
            }
            _ => {}
        },
    }
}

/// Handles keys in normal file browser navigation mode.
fn handle_file_transfer_normal(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let Some(ft) = app.active_file_transfer_mut() else {
        return;
    };

    // Clear transient errors on any keypress
    ft.error = None;

    match code {
        KeyCode::Char('j') | KeyCode::Down => ft.move_down(),
        KeyCode::Char('k') | KeyCode::Up => ft.move_up(),
        KeyCode::Enter => {
            ft.enter_dir();
        }
        KeyCode::Backspace | KeyCode::Char('h') if !modifiers.contains(KeyModifiers::CONTROL) => {
            ft.go_parent();
        }
        KeyCode::Tab => ft.toggle_focus(),
        KeyCode::Char('.') => ft.toggle_hidden(),
        KeyCode::Char('s') => ft.cycle_sort(),
        KeyCode::Char('S') => ft.toggle_sort_direction(),
        KeyCode::Char('r') => {
            ft.refresh_local();
            ft.refresh_remote();
        }
        KeyCode::Char('m') => {
            ft.mode = FileBrowserMode::Creating(Input::default());
        }
        KeyCode::Esc => {
            app.close_current_file_transfer();
        }
        KeyCode::Char('t') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.prefix = PrefixState::Pending;
        }
        _ => {}
    }
}

/// Handles keys while the password prompt is shown in the file transfer view.
fn handle_file_transfer_password(app: &mut App, ev: &Event, code: KeyCode) {
    let View::FileTransfer(idx) = app.view else {
        return;
    };
    let Some(ft) = app.file_transfers.get_mut(idx) else {
        return;
    };

    match code {
        KeyCode::Esc => {
            *ft.connection_status.lock().unwrap() =
                SftpConnectionStatus::Failed("Authentication cancelled".into());
            ft.mode = FileBrowserMode::Normal;
        }
        KeyCode::Enter => {
            let FileBrowserMode::PasswordPrompt(ref input) = ft.mode else {
                return;
            };
            let password = input.value().to_string();
            let host = ft.host.clone();
            let sftp_arc = Arc::clone(&ft.sftp);
            let status_arc = Arc::clone(&ft.connection_status);

            *status_arc.lock().unwrap() = SftpConnectionStatus::Connecting;
            ft.mode = FileBrowserMode::Normal;

            std::thread::spawn(move || {
                match SftpConnection::connect_with_password(&host, &password) {
                    Ok(conn) => {
                        let home = conn.home_dir().unwrap_or_else(|_| "/".into());
                        let entries = conn.list_dir(&home).unwrap_or_default();
                        *sftp_arc.lock().unwrap() = Some(conn);
                        *status_arc.lock().unwrap() = SftpConnectionStatus::ConnectedWithData {
                            home_dir: home,
                            entries,
                        };
                    }
                    Err(ConnectOutcome::NeedsPassword) => {
                        *status_arc.lock().unwrap() =
                            SftpConnectionStatus::Failed("Authentication failed".into());
                    }
                    Err(ConnectOutcome::Failed(msg)) => {
                        *status_arc.lock().unwrap() = SftpConnectionStatus::Failed(msg);
                    }
                }
            });
        }
        _ => {
            if let FileBrowserMode::PasswordPrompt(ref mut input) = ft.mode {
                input.handle_event(ev);
            }
        }
    }
}

/// Handles keys while the mkdir input is shown.
fn handle_file_transfer_mkdir(app: &mut App, ev: &Event, code: KeyCode) {
    let Some(ft) = app.active_file_transfer_mut() else {
        return;
    };

    match code {
        KeyCode::Esc => {
            ft.mode = FileBrowserMode::Normal;
        }
        KeyCode::Enter => {
            let FileBrowserMode::Creating(ref input) = ft.mode else {
                return;
            };
            let name = input.value().trim().to_string();
            if name.is_empty() {
                ft.mode = FileBrowserMode::Normal;
                return;
            }

            match ft.focus {
                FileBrowserPane::Local => {
                    let new_path = ft.local_path.join(&name);
                    match std::fs::create_dir(&new_path) {
                        Ok(()) => {
                            ft.mode = FileBrowserMode::Normal;
                            ft.refresh_local();
                        }
                        Err(e) => {
                            ft.error = Some(format!("mkdir failed: {e}"));
                            ft.mode = FileBrowserMode::Normal;
                        }
                    }
                }
                FileBrowserPane::Remote => {
                    let remote_new = format!("{}/{}", ft.remote_path.trim_end_matches('/'), name);
                    let result = {
                        let sftp_guard = ft.sftp.lock().unwrap();
                        if let Some(ref conn) = *sftp_guard {
                            conn.sftp()
                                .mkdir(std::path::Path::new(&remote_new), 0o755)
                                .map_err(|e| e.to_string())
                        } else {
                            Err("Not connected".into())
                        }
                    };
                    match result {
                        Ok(()) => {
                            ft.mode = FileBrowserMode::Normal;
                            ft.refresh_remote();
                        }
                        Err(e) => {
                            ft.error = Some(format!("mkdir failed: {e}"));
                            ft.mode = FileBrowserMode::Normal;
                        }
                    }
                }
            }
        }
        _ => {
            if let FileBrowserMode::Creating(ref mut input) = ft.mode {
                input.handle_event(ev);
            }
        }
    }
}

/// Handles keys for the transfer confirmation dialog.
fn handle_file_transfer_confirm(app: &mut App, code: KeyCode) {
    let Some(ft) = app.active_file_transfer_mut() else {
        return;
    };
    match code {
        KeyCode::Char('y') | KeyCode::Enter => {
            // Transfer execution will be implemented in Phase 5
            ft.mode = FileBrowserMode::Normal;
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            ft.mode = FileBrowserMode::Normal;
        }
        _ => {}
    }
}
