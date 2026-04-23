use std::path::Path;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use hosttui::app::{App, Mode, Pane};
use hosttui::model::Config;
use hosttui::ssh;
use hosttui::sshconfig;
use hosttui::storage;
use hosttui::ui;

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

            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match &app.mode {
                    Mode::Normal => match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => app.exit = true,
                        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
                        KeyCode::Char('k') | KeyCode::Up => app.move_up(),
                        KeyCode::Tab => app.toggle_focus(),
                        KeyCode::Enter if app.focus == Pane::Hosts => {
                            if let Some(host) = app.selected_host().cloned() {
                                ssh::connect(&host)?;
                                terminal.clear()?;
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
                        _ => {}
                    },
                    Mode::Adding(_) | Mode::Editing { .. } => match key.code {
                        KeyCode::Esc => app.cancel_mode(),
                        KeyCode::Enter => {
                            app.submit_form();
                            if matches!(app.mode, Mode::Normal) && app.dirty {
                                persist(&path, &app.config)?;
                                app.dirty = false;
                            }
                        }
                        KeyCode::Tab => {
                            if let Some(form) = app.form_state_mut() {
                                form.next_field();
                            }
                        }
                        KeyCode::BackTab => {
                            if let Some(form) = app.form_state_mut() {
                                form.prev_field();
                            }
                        }
                        KeyCode::Backspace => {
                            if let Some(form) = app.form_state_mut() {
                                form.active_buffer().pop();
                            }
                        }
                        KeyCode::Char(c) => {
                            if key.modifiers.contains(KeyModifiers::CONTROL) {
                                continue;
                            }
                            if let Some(form) = app.form_state_mut() {
                                form.error = None;
                                form.active_buffer().push(c);
                            }
                        }
                        _ => {}
                    },
                    Mode::AddingGroup(_) | Mode::EditingGroup { .. } => match key.code {
                        KeyCode::Esc => app.cancel_mode(),
                        KeyCode::Enter => {
                            app.submit_form();
                            if matches!(app.mode, Mode::Normal) && app.dirty {
                                persist(&path, &app.config)?;
                                app.dirty = false;
                            }
                        }
                        KeyCode::Backspace => {
                            if let Some(input) = app.input_state_mut() {
                                input.buffer.pop();
                            }
                        }
                        KeyCode::Char(c) => {
                            if key.modifiers.contains(KeyModifiers::CONTROL) {
                                continue;
                            }
                            if let Some(input) = app.input_state_mut() {
                                input.error = None;
                                input.buffer.push(c);
                            }
                        }
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
