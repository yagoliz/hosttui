use std::path::Path;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use tui_input::backend::crossterm::EventHandler;

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

            let ev = event::read()?;
            if let Event::Key(key) = ev {
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
                        _ => {
                            if let Some(form) = app.form_state_mut()
                                && form.active_input().handle_event(&ev).is_some()
                            {
                                form.error = None;
                            }
                        }
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
                        _ => {
                            if let Some(input) = app.input_state_mut()
                                && input.buffer.handle_event(&ev).is_some()
                            {
                                input.error = None;
                            }
                        }
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
