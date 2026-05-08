use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crossterm::event::{Event, KeyCode, KeyModifiers};
use tui_input::Input;
use tui_input::backend::crossterm::EventHandler;

use crate::app::{App, Mode, Pane, PrefixState, View};
use crate::filebrowser::{FileBrowser, FileBrowserMode, FileBrowserPane, TransferDirection};
use crate::keys;
use crate::sftp::{ConnectOutcome, SftpConnection, SftpConnectionStatus};
use crate::storage::persist;
use crate::transfer::{self, TransferRequest};

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
        FileBrowserMode::ConfirmDelete { .. } => {
            handle_file_transfer_delete(app, code);
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

/// Post-navigation actions that require releasing the `FileBrowser` borrow
/// before mutating `App`-level state.
enum PostAction {
    None,
    InitiateTransfer,
    CancelTransfer,
    DeleteSelected,
    Reconnect,
    Close,
    SetPrefix,
}

/// Handles keys in normal file browser navigation mode.
///
/// Uses a two-phase pattern: first compute the action while borrowing the
/// `FileBrowser`, then release that borrow and dispatch actions that need
/// `&mut App`.
fn handle_file_transfer_normal(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let action = {
        let Some(ft) = app.active_file_transfer_mut() else {
            return;
        };

        ft.error = None;

        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                ft.move_down();
                PostAction::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                ft.move_up();
                PostAction::None
            }
            KeyCode::Enter => {
                let is_dir = ft.selected_entry().is_none_or(|e| e.is_dir);
                if is_dir {
                    ft.enter_dir();
                    PostAction::None
                } else {
                    PostAction::InitiateTransfer
                }
            }
            KeyCode::Char('y') => PostAction::InitiateTransfer,
            KeyCode::Char('c') => PostAction::CancelTransfer,
            KeyCode::Backspace | KeyCode::Char('h')
                if !modifiers.contains(KeyModifiers::CONTROL) =>
            {
                ft.go_parent();
                PostAction::None
            }
            KeyCode::Left => {
                ft.local_focus();
                PostAction::None
            }
            KeyCode::Right => {
                ft.remote_focus();
                PostAction::None
            }
            KeyCode::Tab => {
                ft.toggle_focus();
                PostAction::None
            }
            KeyCode::Char('.') => {
                ft.toggle_hidden();
                PostAction::None
            }
            KeyCode::Char('s') => {
                ft.cycle_sort();
                PostAction::None
            }
            KeyCode::Char('S') => {
                ft.toggle_sort_direction();
                PostAction::None
            }
            KeyCode::Char('r') => {
                ft.refresh_local();
                ft.refresh_remote();
                PostAction::None
            }
            KeyCode::Char('m') => {
                ft.mode = FileBrowserMode::Creating(Input::default());
                PostAction::None
            }
            KeyCode::Char('d') => PostAction::DeleteSelected,
            KeyCode::Char('R') => PostAction::Reconnect,
            KeyCode::Esc => PostAction::Close,
            KeyCode::Char('t') if modifiers.contains(KeyModifiers::CONTROL) => {
                PostAction::SetPrefix
            }
            _ => PostAction::None,
        }
    };

    match action {
        PostAction::InitiateTransfer => initiate_transfer(app),
        PostAction::CancelTransfer => cancel_active_transfer(app),
        PostAction::DeleteSelected => initiate_delete(app),
        PostAction::Reconnect => reconnect_sftp(app),
        PostAction::Close => app.close_current_file_transfer(),
        PostAction::SetPrefix => app.prefix = PrefixState::Pending,
        PostAction::None => {}
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

/// Replays pasted text into whichever `tui_input::Input` is active in the
/// file transfer view (password prompt or mkdir input).
pub fn handle_file_transfer_paste(app: &mut App, text: &str) {
    let Some(ft) = app.active_file_transfer_mut() else {
        return;
    };
    let input = match &mut ft.mode {
        FileBrowserMode::PasswordPrompt(input) => input,
        FileBrowserMode::Creating(input) => input,
        _ => return,
    };
    for ch in text.chars() {
        let ev = Event::Key(keys::paste_key(ch));
        input.handle_event(&ev);
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
///
/// The dialog is shown when the destination file already exists. On
/// confirmation, the transfer starts overwriting the existing file.
fn handle_file_transfer_confirm(app: &mut App, code: KeyCode) {
    let Some(ft) = app.active_file_transfer_mut() else {
        return;
    };

    match code {
        KeyCode::Char('y') | KeyCode::Enter => {
            let (file, direction) = match &ft.mode {
                FileBrowserMode::ConfirmTransfer { file, direction } => (file.clone(), *direction),
                _ => return,
            };

            let local_path = ft.local_path.join(&file);
            let remote_path = format!("{}/{}", ft.remote_path.trim_end_matches('/'), file);

            ft.mode = FileBrowserMode::Normal;
            start_transfer(ft, direction, local_path, remote_path, false);
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            ft.mode = FileBrowserMode::Normal;
        }
        _ => {}
    }
}

/// Determines source/destination and starts a file transfer or shows a
/// confirmation dialog if the destination already exists.
fn initiate_transfer(app: &mut App) {
    let Some(ft) = app.active_file_transfer_mut() else {
        return;
    };

    let entry = match ft.selected_entry() {
        Some(e) => e.clone(),
        None => return,
    };

    let (direction, local_path, remote_path) = match ft.focus {
        FileBrowserPane::Local => {
            let local = ft.local_path.join(&entry.name);
            let remote = format!("{}/{}", ft.remote_path.trim_end_matches('/'), entry.name);
            (TransferDirection::Upload, local, remote)
        }
        FileBrowserPane::Remote => {
            let remote = format!("{}/{}", ft.remote_path.trim_end_matches('/'), entry.name);
            let local = ft.local_path.join(&entry.name);
            (TransferDirection::Download, local, remote)
        }
    };

    let dest_exists = match direction {
        TransferDirection::Upload => {
            let sftp_guard = ft.sftp.lock().unwrap();
            sftp_guard
                .as_ref()
                .map(|conn| conn.stat(&remote_path).is_ok())
                .unwrap_or(false)
        }
        TransferDirection::Download => local_path.exists(),
    };

    if dest_exists && !entry.is_dir {
        ft.mode = FileBrowserMode::ConfirmTransfer {
            file: entry.name.clone(),
            direction,
        };
    } else {
        start_transfer(ft, direction, local_path, remote_path, entry.is_dir);
    }
}

/// Spawns a background transfer thread and pushes the handle into the browser.
fn start_transfer(
    ft: &mut FileBrowser,
    direction: TransferDirection,
    local_path: PathBuf,
    remote_path: String,
    is_dir: bool,
) {
    let request = match (direction, is_dir) {
        (TransferDirection::Upload, false) => TransferRequest::Upload {
            local_path,
            remote_path,
        },
        (TransferDirection::Download, false) => TransferRequest::Download {
            remote_path,
            local_path,
        },
        (TransferDirection::Upload, true) => TransferRequest::UploadDir {
            local_path,
            remote_path,
        },
        (TransferDirection::Download, true) => TransferRequest::DownloadDir {
            remote_path,
            local_path,
        },
    };

    let handle = transfer::spawn_transfer(Arc::clone(&ft.sftp), request);
    ft.transfers.push(handle);
}

/// Attempts to reconnect a disconnected SFTP session on a background thread.
///
/// Only triggers when the connection status is `Failed`. Resets the status to
/// `Connecting` and spawns a fresh connection attempt using the same host config.
fn reconnect_sftp(app: &mut App) {
    let Some(ft) = app.active_file_transfer_mut() else {
        return;
    };

    let status = ft.connection_status.lock().unwrap().clone();
    if !matches!(status, SftpConnectionStatus::Failed(_)) {
        return;
    }

    let host = ft.host.clone();
    let sftp_arc = Arc::clone(&ft.sftp);
    let status_arc = Arc::clone(&ft.connection_status);

    *status_arc.lock().unwrap() = SftpConnectionStatus::Connecting;
    ft.error = None;

    std::thread::spawn(move || match SftpConnection::connect(&host) {
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
            *status_arc.lock().unwrap() = SftpConnectionStatus::NeedsPassword;
        }
        Err(ConnectOutcome::Failed(msg)) => {
            *status_arc.lock().unwrap() = SftpConnectionStatus::Failed(msg);
        }
    });
}

/// Shows a confirmation dialog for deleting the selected file or directory.
fn initiate_delete(app: &mut App) {
    let Some(ft) = app.active_file_transfer_mut() else {
        return;
    };
    let Some(entry) = ft.selected_entry().cloned() else {
        return;
    };
    ft.mode = FileBrowserMode::ConfirmDelete {
        name: entry.name,
        is_dir: entry.is_dir,
    };
}

/// Handles keys for the delete confirmation dialog.
fn handle_file_transfer_delete(app: &mut App, code: KeyCode) {
    let Some(ft) = app.active_file_transfer_mut() else {
        return;
    };

    match code {
        KeyCode::Char('y') | KeyCode::Enter => {
            let (name, is_dir) = match &ft.mode {
                FileBrowserMode::ConfirmDelete { name, is_dir } => (name.clone(), *is_dir),
                _ => return,
            };
            ft.mode = FileBrowserMode::Normal;

            match ft.focus {
                FileBrowserPane::Local => {
                    let target = ft.local_path.join(&name);
                    let result = if is_dir {
                        std::fs::remove_dir_all(&target)
                    } else {
                        std::fs::remove_file(&target)
                    };
                    match result {
                        Ok(()) => ft.refresh_local(),
                        Err(e) => ft.error = Some(format!("Delete failed: {e}")),
                    }
                }
                FileBrowserPane::Remote => {
                    let target = format!("{}/{}", ft.remote_path.trim_end_matches('/'), name);
                    let result = {
                        let sftp_guard = ft.sftp.lock().unwrap();
                        if let Some(ref conn) = *sftp_guard {
                            if is_dir {
                                rmdir_recursive(conn, &target)
                            } else {
                                conn.sftp()
                                    .unlink(Path::new(&target))
                                    .map_err(|e| e.to_string())
                            }
                        } else {
                            Err("Not connected".into())
                        }
                    };
                    match result {
                        Ok(()) => ft.refresh_remote(),
                        Err(e) => ft.error = Some(format!("Delete failed: {e}")),
                    }
                }
            }
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            ft.mode = FileBrowserMode::Normal;
        }
        _ => {}
    }
}

/// Recursively removes a remote directory via SFTP.
///
/// SFTP's `rmdir` only works on empty directories, so we walk the tree
/// depth-first, removing files and subdirectories before the parent.
fn rmdir_recursive(conn: &crate::sftp::SftpConnection, path: &str) -> Result<(), String> {
    let entries = conn.list_dir(path).map_err(|e| e.to_string())?;
    for entry in &entries {
        let child = format!("{}/{}", path.trim_end_matches('/'), entry.name);
        if entry.is_dir {
            rmdir_recursive(conn, &child)?;
        } else {
            conn.sftp()
                .unlink(Path::new(&child))
                .map_err(|e| format!("unlink '{child}': {e}"))?;
        }
    }
    conn.sftp()
        .rmdir(Path::new(path))
        .map_err(|e| format!("rmdir '{path}': {e}"))
}

/// Signals the most recent active transfer to cancel at the next chunk boundary.
fn cancel_active_transfer(app: &mut App) {
    let Some(ft) = app.active_file_transfer_mut() else {
        return;
    };
    if let Some(handle) = ft.transfers.last() {
        handle.cancel.store(true, Ordering::Relaxed);
    }
}
