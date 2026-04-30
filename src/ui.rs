use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Position, Rect},
    style::{Color, Modifier, Style, Stylize},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Clear, List, ListItem, ListState, Paragraph},
};

use std::sync::atomic::Ordering;

use crate::app::{
    self, App, ExtraField, ExtrasEditor, FormState, GroupEntry, InputState, Mode, Pane, PrefixState,
    View,
};
use crate::pty::SessionStatus;
use crate::terminal_widget::TerminalView;

pub fn render(frame: &mut Frame, app: &App) {
    let has_tabs = app.has_active_sessions();
    let [main_area, tab_bar_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(if has_tabs { 1 } else { 0 }),
    ])
    .areas(frame.area());

    match app.view {
        View::Hosts => render_hosts_view(frame, app, main_area),
        View::Session(idx) => render_session_view(frame, app, idx, main_area),
    }

    if has_tabs {
        render_tab_bar(frame, app, tab_bar_area);
    }

    match &app.mode {
        Mode::Adding(form) => {
            render_form(frame, "Add Host", form);
            if let Some(ed) = &form.extras_editor {
                render_extras(frame, form, ed);
            }
        }
        Mode::Editing { form, .. } => {
            render_form(frame, "Edit Host", form);
            if let Some(ed) = &form.extras_editor {
                render_extras(frame, form, ed);
            }
        }
        Mode::ConfirmDelete(alias) => {
            render_confirm(frame, "Confirm Delete", &format!("Delete host '{alias}'?"))
        }
        Mode::ConfirmDeleteGroup(name) => render_confirm(
            frame,
            "Confirm Delete",
            &format!("Delete group '{name}'?\nHosts will become ungrouped."),
        ),
        Mode::AddingGroup(input) => render_group_input(frame, "New Group", input),
        Mode::EditingGroup { input, .. } => render_group_input(frame, "Rename Group", input),
        Mode::ConnectError { alias, message } => render_connect_error(frame, alias, message),
        Mode::TabHelp => render_tab_help(frame),
        Mode::Normal | Mode::Searching => {}
    }
}

fn render_hosts_view(frame: &mut Frame, app: &App, area: Rect) {
    let show_search_bar = matches!(app.mode, Mode::Searching) || !app.search.value().is_empty();
    let [main_area, search_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(if show_search_bar { 1 } else { 0 }),
    ])
    .areas(area);

    let [groups_area, hosts_area, detail_area] = Layout::horizontal([
        Constraint::Percentage(33),
        Constraint::Percentage(33),
        Constraint::Percentage(34),
    ])
    .areas(main_area);

    render_groups_pane(frame, app, groups_area);
    render_host_list(frame, app, hosts_area);
    render_detail(frame, app, detail_area);

    if show_search_bar {
        render_search_bar(frame, app, search_area);
    }
}

fn render_session_view(frame: &mut Frame, app: &App, idx: usize, area: Rect) {
    let Some(session) = app.sessions.get(idx) else {
        return;
    };
    let screen = session.screen();
    let is_dead = matches!(session.status(), SessionStatus::Exited(_));

    let border_style = if is_dead {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::Cyan)
    };
    let block = Block::bordered()
        .title(Line::from(format!(" {} ", session.alias).bold()).centered())
        .border_set(border::ROUNDED)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(TerminalView::new(&screen), inner);

    if !is_dead && !screen.hide_cursor() {
        let (cursor_row, cursor_col) = screen.cursor_position();
        let x = inner.x + cursor_col;
        let y = inner.y + cursor_row;
        if x < inner.x + inner.width && y < inner.y + inner.height {
            frame.set_cursor_position(Position::new(x, y));
        }
    }

    if is_dead {
        let overlay_area = Rect {
            x: inner.x,
            y: inner.y + inner.height.saturating_sub(1),
            width: inner.width,
            height: 1,
        };
        let line = Line::from(vec![
            Span::styled(
                " [disconnected] ",
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Ctrl+T x to close", Style::default().fg(Color::DarkGray)),
        ]);
        frame.render_widget(Paragraph::new(line), overlay_area);
    }
}

fn render_tab_bar(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = Vec::new();

    let hosts_style = if matches!(app.view, View::Hosts) {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    spans.push(Span::styled(" Hosts ", hosts_style));

    for (i, session) in app.sessions.iter().enumerate() {
        spans.push(Span::raw(" "));
        let is_active = matches!(app.view, View::Session(idx) if idx == i);
        let is_dead = matches!(session.status(), SessionStatus::Exited(_));
        let has_unread = session.unread.load(Ordering::SeqCst);

        let label = if let SessionStatus::Exited(code) = session.status() {
            let code_str = code.map_or("?".into(), |c| c.to_string());
            format!(" {}:{} [{}] ", i + 1, session.alias, code_str)
        } else {
            format!(" {}:{} ", i + 1, session.alias)
        };

        let style = if is_active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if is_dead {
            Style::default().fg(Color::DarkGray)
        } else if has_unread {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        spans.push(Span::styled(label, style));
    }

    if matches!(app.prefix, PrefixState::Pending) {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            "^T-",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let used: usize = spans.iter().map(|s| s.width()).sum();
    let hint = "^T ? help";
    let hint_width = hint.len();
    if area.width as usize > used + hint_width + 1 {
        let pad = area.width as usize - used - hint_width;
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(hint, Style::default().fg(Color::DarkGray)));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_connect_error(frame: &mut Frame, alias: &str, message: &str) {
    let area = centered_rect(60, 7, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::bordered()
        .title(Line::from(" Connection Failed ".bold()).centered())
        .title_bottom(Line::from(vec![" Enter/Esc ".into(), "Dismiss".blue().bold()]).centered())
        .border_set(border::THICK)
        .border_style(Style::default().fg(Color::Red));

    let text = vec![
        Line::from(format!("Could not reach '{alias}':")),
        Line::default(),
        Line::from(Span::styled(
            message.to_string(),
            Style::default().fg(Color::Red),
        )),
    ];

    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn render_tab_help(frame: &mut Frame) {
    let area = centered_rect(40, 11, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::bordered()
        .title(Line::from(" Tab Keys ".bold()).centered())
        .title_bottom(Line::from(vec![" any key ".into(), "Dismiss".blue().bold()]).centered())
        .border_set(border::THICK)
        .border_style(Style::default().fg(Color::Cyan));

    let key_style = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let text = vec![
        Line::from(vec![Span::styled("^T h  ", key_style), Span::raw("Switch to hosts")]),
        Line::from(vec![Span::styled("^T 1-9", key_style), Span::raw(" Switch to tab N")]),
        Line::from(vec![Span::styled("^T n  ", key_style), Span::raw("Next tab")]),
        Line::from(vec![Span::styled("^T p  ", key_style), Span::raw("Previous tab")]),
        Line::from(vec![Span::styled("^T x  ", key_style), Span::raw("Close current tab")]),
        Line::from(vec![Span::styled("^T ^T ", key_style), Span::raw("Send literal ^T")]),
    ];

    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn pane_border(title: &str, focused: bool) -> Block<'_> {
    let style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Block::bordered()
        .title(Line::from(format!(" {title} ").bold()).centered())
        .border_set(border::THICK)
        .border_style(style)
}

fn render_groups_pane(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Pane::Groups;
    let items: Vec<ListItem> = app
        .group_entries()
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let label = match entry {
                GroupEntry::All => "all".to_string(),
                GroupEntry::Named(name) => name.clone(),
                GroupEntry::Ungrouped => "ungrouped".to_string(),
            };
            let style = if i == app.group_selected {
                if focused {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::White)
                        .add_modifier(Modifier::BOLD)
                }
            } else {
                Style::default()
            };
            ListItem::new(Line::from(format!(" {label}")).style(style))
        })
        .collect();

    let instructions = if focused {
        Line::from(vec![
            " g ".into(),
            "New".blue().bold(),
            " e ".into(),
            "Rename".blue().bold(),
            " d ".into(),
            "Del".blue().bold(),
        ])
    } else {
        Line::default()
    };

    let block = pane_border("Groups", focused).title_bottom(instructions.centered());

    let list = List::new(items).block(block);
    let mut state = ListState::default().with_selected(Some(app.group_selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_host_list(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Pane::Hosts;
    let items: Vec<ListItem> = app
        .items()
        .iter()
        .enumerate()
        .map(|(i, item)| match item {
            app::ListItem::GroupHeader(name) => {
                ListItem::new(Line::from(format!(" ▸ {name}")).bold().fg(Color::Yellow))
            }
            app::ListItem::Host(alias) => {
                let style = if i == app.selected {
                    if focused {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::White)
                            .add_modifier(Modifier::BOLD)
                    }
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(format!("   {alias}")).style(style))
            }
        })
        .collect();

    let instructions = if focused {
        Line::from(vec![
            " ⏎ ".into(),
            "Connect".blue().bold(),
            " a ".into(),
            "Add".blue().bold(),
            " e ".into(),
            "Edit".blue().bold(),
            " d ".into(),
            "Del".blue().bold(),
        ])
    } else {
        Line::default()
    };

    let block = pane_border("Hosts", focused).title_bottom(instructions.centered());

    let list = List::new(items).block(block);
    let mut state = ListState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_detail(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered()
        .title(Line::from(" Details ".bold()).centered())
        .border_set(border::THICK)
        .border_style(Style::default().fg(Color::DarkGray));

    let Some(host) = app.selected_host() else {
        let empty = Paragraph::new("No host selected").block(block);
        frame.render_widget(empty, area);
        return;
    };

    let label = |key: &str| Span::styled(format!("{key:>15}: "), Style::default().fg(Color::Cyan));

    let mut lines = vec![
        Line::from(vec![label("Alias"), Span::raw(&host.alias)]),
        Line::from(vec![label("Hostname"), Span::raw(&host.hostname)]),
        Line::from(vec![label("User"), Span::raw(&host.user)]),
        Line::from(vec![label("Port"), Span::raw(host.port.to_string())]),
    ];

    if let Some(ref id) = host.identity_file {
        lines.push(Line::from(vec![label("Identity File"), Span::raw(id)]));
    }

    if let Some(ref group) = host.group {
        lines.push(Line::from(vec![label("Group"), Span::raw(group)]));
    }

    for (key, val) in &host.extra {
        lines.push(Line::from(vec![label(key), Span::raw(val)]));
    }

    lines.push(Line::from(vec![
        label("Comments"),
        Span::raw(&host.details),
    ]));

    let detail = Paragraph::new(lines).block(block);
    frame.render_widget(detail, area);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    area
}

fn render_form(frame: &mut Frame, title: &str, form: &FormState) {
    let area = centered_rect(75, (form.fields.len() as u16) + 6, frame.area());
    frame.render_widget(Clear, area);

    let extras_label = format!(" Ctrl+K Extras ({}) ", form.extras.len());
    let instructions = Line::from(vec![
        " Tab ".into(),
        "Next".blue().bold(),
        " Enter ".into(),
        "Save".blue().bold(),
        extras_label.into(),
        " Esc ".into(),
        "Cancel ".blue().bold(),
    ]);

    let block = Block::bordered()
        .title(Line::from(format!(" {title} ").bold()).centered())
        .title_bottom(instructions.centered())
        .border_set(border::THICK);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let constraints: Vec<Constraint> = form
        .fields
        .iter()
        .map(|_| Constraint::Length(1))
        .chain(form.error.as_ref().map(|_| Constraint::Length(2)))
        .collect();

    let rows = Layout::vertical(constraints).split(inner);

    let label_width = 17; // "{:>15}: " renders to 17 columns
    for (i, (field, input)) in form.fields.iter().enumerate() {
        let is_active = i == form.active;
        let label_style = if is_active {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let value_style = if is_active {
            Style::default().fg(Color::White)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let line = Line::from(vec![
            Span::styled(format!("{:>15}: ", field.label()), label_style),
            Span::styled(input.value().to_string(), value_style),
        ]);
        frame.render_widget(Paragraph::new(line), rows[i]);

        if is_active {
            let row = rows[i];
            let cursor_x = row.x + label_width + input.visual_cursor() as u16;
            if cursor_x < row.x + row.width {
                frame.set_cursor_position(Position::new(cursor_x, row.y));
            }
        }
    }

    if let Some(ref err) = form.error {
        let err_idx = form.fields.len();
        let err_line = Line::from(Span::styled(
            format!("  {err}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(Paragraph::new(err_line), rows[err_idx]);
    }
}

fn render_group_input(frame: &mut Frame, title: &str, input: &InputState) {
    let height = if input.error.is_some() { 5 } else { 3 };
    let area = centered_rect(40, height, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::bordered()
        .title(Line::from(format!(" {title} ").bold()).centered())
        .border_set(border::THICK);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![Line::from(vec![
        Span::styled(
            "Name: ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(input.buffer.value().to_string()),
    ])];

    if let Some(ref err) = input.error {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            err.as_str(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
    }

    frame.render_widget(Paragraph::new(lines), inner);

    let cursor_x = inner.x + 6 + input.buffer.visual_cursor() as u16;
    if cursor_x < inner.x + inner.width {
        frame.set_cursor_position(Position::new(cursor_x, inner.y));
    }
}

fn render_search_bar(frame: &mut Frame, app: &App, area: Rect) {
    let active = matches!(app.mode, Mode::Searching);
    let prompt_style = if active {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let value = app.search.value();
    let hint = if active {
        if value.is_empty() {
            "  (Enter to keep, Esc to clear)"
        } else {
            ""
        }
    } else {
        "  (/ to edit)"
    };
    let line = Line::from(vec![
        Span::styled("/ ", prompt_style),
        Span::styled(value.to_string(), Style::default().fg(Color::White)),
        Span::styled(hint, Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(line), area);

    if active {
        let cursor_x = area.x + 2 + app.search.visual_cursor() as u16;
        if cursor_x < area.x + area.width {
            frame.set_cursor_position(Position::new(cursor_x, area.y));
        }
    }
}

fn render_extras(frame: &mut Frame, form: &FormState, ed: &ExtrasEditor) {
    if ed.entry.is_some() {
        render_extras_entry(frame, ed);
        return;
    }

    let height = (form.extras.len().max(1) as u16).min(12) + 4 + ed.error.is_some() as u16;
    let area = centered_rect(60, height, frame.area());
    frame.render_widget(Clear, area);

    let instructions = Line::from(vec![
        " a ".into(),
        "Add".blue().bold(),
        " e ".into(),
        "Edit".blue().bold(),
        " d ".into(),
        "Del".blue().bold(),
        " Esc ".into(),
        "Close".blue().bold(),
    ]);

    let block = Block::bordered()
        .title(Line::from(" Extras ".bold()).centered())
        .title_bottom(instructions.centered())
        .border_set(border::THICK);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let constraints: Vec<Constraint> = if form.extras.is_empty() {
        vec![Constraint::Min(1)]
    } else {
        let mut v: Vec<Constraint> = form.extras.iter().map(|_| Constraint::Length(1)).collect();
        if ed.error.is_some() {
            v.push(Constraint::Length(1));
        }
        v
    };
    let rows = Layout::vertical(constraints).split(inner);

    if form.extras.is_empty() {
        let line = Line::from(Span::styled(
            "  (no extras — press 'a' to add)",
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(Paragraph::new(line), rows[0]);
    } else {
        for (i, (k, v)) in form.extras.iter().enumerate() {
            let style = if i == ed.selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let line = Line::from(format!(" {k} = {v}")).style(style);
            frame.render_widget(Paragraph::new(line), rows[i]);
        }
        if let Some(ref err) = ed.error {
            let err_line = Line::from(Span::styled(
                format!(" {err}"),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ));
            frame.render_widget(Paragraph::new(err_line), rows[form.extras.len()]);
        }
    }
}

fn render_extras_entry(frame: &mut Frame, ed: &ExtrasEditor) {
    let entry = ed.entry.as_ref().expect("entry must be set");
    let height = if ed.error.is_some() { 6 } else { 5 };
    let area = centered_rect(60, height, frame.area());
    frame.render_widget(Clear, area);

    let title = if entry.editing_index.is_some() {
        " Edit Extra "
    } else {
        " New Extra "
    };
    let instructions = Line::from(vec![
        " Tab ".into(),
        "Switch".blue().bold(),
        " Enter ".into(),
        "Save".blue().bold(),
        " Esc ".into(),
        "Cancel".blue().bold(),
    ]);
    let block = Block::bordered()
        .title(Line::from(title.bold()).centered())
        .title_bottom(instructions.centered())
        .border_set(border::THICK);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let constraints: Vec<Constraint> = if ed.error.is_some() {
        vec![
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(2),
        ]
    } else {
        vec![Constraint::Length(1), Constraint::Length(1)]
    };
    let rows = Layout::vertical(constraints).split(inner);

    let label_width = 9; // "{:>7}: "

    let render_field =
        |frame: &mut Frame, row: Rect, label: &str, input: &tui_input::Input, active: bool| {
            let label_style = if active {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            let value_style = if active {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let line = Line::from(vec![
                Span::styled(format!("{:>7}: ", label), label_style),
                Span::styled(input.value().to_string(), value_style),
            ]);
            frame.render_widget(Paragraph::new(line), row);

            if active {
                let cursor_x = row.x + label_width + input.visual_cursor() as u16;
                if cursor_x < row.x + row.width {
                    frame.set_cursor_position(Position::new(cursor_x, row.y));
                }
            }
        };

    render_field(
        frame,
        rows[0],
        "Key",
        &entry.key,
        entry.active == ExtraField::Key,
    );
    render_field(
        frame,
        rows[1],
        "Value",
        &entry.value,
        entry.active == ExtraField::Value,
    );

    if let Some(ref err) = ed.error {
        let err_line = Line::from(Span::styled(
            format!("  {err}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(Paragraph::new(err_line), rows[2]);
    }
}

fn render_confirm(frame: &mut Frame, title: &str, message: &str) {
    let line_count = message.lines().count() as u16;
    let area = centered_rect(45, line_count + 4, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::bordered()
        .title(Line::from(format!(" {title} ").bold()).centered())
        .border_set(border::THICK);

    let mut text: Vec<Line> = message.lines().map(Line::from).collect();
    text.push(Line::default());
    text.push(Line::from(vec![
        " y ".into(),
        "Yes".red().bold(),
        "  n ".into(),
        "No".blue().bold(),
    ]));

    let paragraph = Paragraph::new(text).block(block);
    frame.render_widget(paragraph, area);
}
