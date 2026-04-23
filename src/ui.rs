use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Clear, List, ListItem, ListState, Paragraph},
};

use crate::app::{self, App, FormState, GroupEntry, InputState, Mode, Pane};

pub fn render(frame: &mut Frame, app: &App) {
    let show_search_bar = matches!(app.mode, Mode::Searching) || !app.search.is_empty();
    let [main_area, search_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(if show_search_bar { 1 } else { 0 }),
    ])
    .areas(frame.area());

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

    match &app.mode {
        Mode::Adding(form) => render_form(frame, "Add Host", form),
        Mode::Editing { form, .. } => render_form(frame, "Edit Host", form),
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
        Mode::Normal | Mode::Searching => {}
    }
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
    let area = centered_rect(50, (form.fields.len() as u16) + 5, frame.area());
    frame.render_widget(Clear, area);

    let instructions = Line::from(vec![
        " Tab ".into(),
        "Next".blue().bold(),
        " Shift+Tab ".into(),
        "Prev".blue().bold(),
        " Enter ".into(),
        "Save".blue().bold(),
        " Esc ".into(),
        "Cancel".blue().bold(),
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

    for (i, (field, value)) in form.fields.iter().enumerate() {
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

        let display_value = if is_active {
            format!("{value}_")
        } else {
            value.clone()
        };

        let line = Line::from(vec![
            Span::styled(format!("{:>15}: ", field.label()), label_style),
            Span::styled(display_value, value_style),
        ]);
        frame.render_widget(Paragraph::new(line), rows[i]);
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
        Span::raw(format!("{}_", input.buffer)),
    ])];

    if let Some(ref err) = input.error {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            err.as_str(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
    }

    frame.render_widget(Paragraph::new(lines), inner);
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
    let buffer = if active {
        format!("{}_", app.search)
    } else {
        app.search.clone()
    };
    let hint = if active {
        if app.search.is_empty() {
            "  (Enter to keep, Esc to clear)"
        } else {
            ""
        }
    } else {
        "  (/ to edit)"
    };
    let line = Line::from(vec![
        Span::styled("/ ", prompt_style),
        Span::styled(buffer, Style::default().fg(Color::White)),
        Span::styled(hint, Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
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
