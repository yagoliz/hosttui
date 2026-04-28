use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use tui_input::backend::crossterm::EventHandler;

use hosttui::app::{App, Mode, Pane};
use hosttui::model::{Config, Host};
use hosttui::ssh;
use hosttui::sshconfig;
use hosttui::storage;
use hosttui::ui;

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Result of the probe step. The outer bool is whether to proceed to ssh.
enum ProbeOutcome {
    Connect,
    Failed(String),
    Cancelled,
}

fn run_probe<B: ratatui::backend::Backend>(
    terminal: &mut ratatui::Terminal<B>,
    app: &mut App,
    host: &Host,
) -> anyhow::Result<ProbeOutcome>
where
    B::Error: Send + Sync + 'static,
{
    app.mode = Mode::Connecting {
        alias: host.alias.clone(),
    };

    let (tx, rx) = mpsc::channel();
    let hostname = host.hostname.clone();
    let port = host.port;
    thread::spawn(move || {
        let _ = tx.send(ssh::probe(&hostname, port, PROBE_TIMEOUT));
    });

    loop {
        terminal.draw(|frame| ui::render(frame, app))?;

        if event::poll(POLL_INTERVAL)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && key.code == KeyCode::Esc
        {
            return Ok(ProbeOutcome::Cancelled);
        }

        match rx.try_recv() {
            Ok(Ok(())) => return Ok(ProbeOutcome::Connect),
            Ok(Err(e)) => return Ok(ProbeOutcome::Failed(e.to_string())),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Ok(ProbeOutcome::Failed("probe thread died".into()));
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}

fn handle_extras_key(app: &mut App, ev: &Event, code: KeyCode) {
    let Some(form) = app.form_state_mut() else {
        return;
    };
    let Some(ed) = form.extras_editor.as_mut() else {
        return;
    };

    if ed.entry.is_some() {
        // Inner Key/Value editor.
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
        // Browsing the extras list.
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

fn main() -> anyhow::Result<()> {
    let path = storage::config_path()?;
    let config = storage::load(&path)?;
    let mut app = App::new(config);

    ratatui::run(|terminal| {
        while !app.exit {
            terminal.draw(|frame| ui::render(frame, &app))?;

            let ev = event::read()?;
            if let Event::Key(key) = ev {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match &app.mode {
                    Mode::Normal => match key.code {
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
                            if let Some(host) = app.selected_host().cloned() {
                                match run_probe(terminal, &mut app, &host)? {
                                    ProbeOutcome::Connect => {
                                        app.mode = Mode::Normal;
                                        ssh::connect(&host)?;
                                        terminal.clear()?;
                                    }
                                    ProbeOutcome::Failed(msg) => {
                                        app.mode = Mode::ConnectError {
                                            alias: host.alias.clone(),
                                            message: msg,
                                        };
                                    }
                                    ProbeOutcome::Cancelled => {
                                        app.mode = Mode::Normal;
                                    }
                                }
                            }
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
                        _ => {}
                    },
                    Mode::Searching => match key.code {
                        KeyCode::Esc => app.cancel_search(),
                        KeyCode::Enter => app.commit_search(),
                        KeyCode::Down => app.move_down(),
                        KeyCode::Up => app.move_up(),
                        _ => {
                            if app.search.handle_event(&ev).is_some() {
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
                            handle_extras_key(&mut app, &ev, key.code);
                        } else {
                            match key.code {
                                KeyCode::Esc => app.cancel_mode(),
                                KeyCode::Enter => {
                                    app.submit_form();
                                    if matches!(app.mode, Mode::Normal) && app.dirty {
                                        persist(&path, &app.config)?;
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
                                KeyCode::Char('k')
                                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                {
                                    if let Some(form) = app.form_state_mut() {
                                        form.open_extras();
                                    }
                                }
                                _ => {
                                    if let Some(form) = app.form_state_mut()
                                        && form.active_input().handle_event(&ev).is_some()
                                    {
                                        form.error = None;
                                    }
                                }
                            }
                        }
                    }
                    Mode::AddingGroup(_) | Mode::EditingGroup { .. } => match key.code {
                        KeyCode::Esc => app.cancel_mode(),
                        KeyCode::Enter => {
                            app.submit_form();
                            if matches!(app.mode, Mode::Normal) && app.dirty {
                                persist(&path, &app.config)?;
                                app.dirty = false;
                            }
                        }
                        _ => {
                            if let Some(input) = app.input_state_mut()
                                && input.buffer.handle_event(&ev).is_some()
                            {
                                input.error = None;
                            }
                        }
                    },
                    Mode::Connecting { .. } => {
                        // The probe sub-loop owns this mode; shouldn't reach
                        // here, but drop back to Normal if it does.
                        app.mode = Mode::Normal;
                    }
                    Mode::ConnectError { .. } => match key.code {
                        KeyCode::Enter | KeyCode::Esc => app.cancel_mode(),
                        _ => {}
                    },
                    Mode::ConfirmDelete(_) | Mode::ConfirmDeleteGroup(_) => match key.code {
                        KeyCode::Char('y') => {
                            app.confirm_delete();
                            persist(&path, &app.config)?;
                            app.dirty = false;
                        }
                        KeyCode::Char('n') | KeyCode::Esc => app.cancel_mode(),
                        _ => {}
                    },
                }
            }
        }
        Ok(())
    })
}
