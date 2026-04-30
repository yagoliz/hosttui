use std::path::Path;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use tui_input::backend::crossterm::EventHandler;

use hosttui::app::{App, Mode, Pane, PrefixState, View};
use hosttui::keys;
use hosttui::model::Config;
use hosttui::sshconfig;
use hosttui::storage;
use hosttui::ui;

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

fn persist(path: &Path, config: &Config) -> anyhow::Result<()> {
    storage::save(path, config)?;
    let ssh_path = sshconfig::ssh_config_path()?;
    sshconfig::export(&ssh_path, config)?;
    Ok(())
}

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
                    app.open_session(rows.saturating_sub(1), cols);
                }
                KeyCode::Char('a') if app.focus == Pane::Hosts => app.start_adding(),
                KeyCode::Char('e') if app.focus == Pane::Hosts => app.start_editing(),
                KeyCode::Char('e') if app.focus == Pane::Groups => {
                    app.start_editing_group();
                }
                KeyCode::Char('d') => app.start_delete(),
                KeyCode::Char('g') if app.focus == Pane::Groups => {
                    app.start_adding_group();
                }
                KeyCode::Char('/') => app.start_search(),
                KeyCode::Char('t') if modifiers.contains(KeyModifiers::CONTROL) => {
                    if app.has_active_sessions() {
                        app.prefix = PrefixState::Pending;
                    }
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
        Mode::ConnectError { .. } => match code {
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

fn handle_session_key(app: &mut App, key: &crossterm::event::KeyEvent) {
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

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    path: &Path,
) -> anyhow::Result<()> {
    while !app.exit {
        for session in &mut app.sessions {
            session.update_status();
        }
        app.close_exited_sessions();

        terminal.draw(|frame| ui::render(frame, app))?;

        let timeout = if app.has_active_sessions() {
            Duration::from_millis(16)
        } else {
            Duration::from_secs(1)
        };

        if !event::poll(timeout)? {
            continue;
        }

        let ev = event::read()?;

        if let Event::Resize(cols, rows) = ev {
            let session_rows = if app.has_active_sessions() {
                rows.saturating_sub(1)
            } else {
                rows
            };
            for session in &app.sessions {
                session.resize(session_rows, cols);
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

fn main() -> anyhow::Result<()> {
    let path = storage::config_path()?;
    let config = storage::load(&path)?;
    let mut app = App::new(config);

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app, &path);
    ratatui::restore();
    result
}
