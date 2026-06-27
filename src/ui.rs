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
    self, App, ExtraField, ExtrasEditor, FormState, GroupEntry, InputState, Mode, Pane,
    PrefixState, ScreenPos, TestStatus, View,
};
use crate::filebrowser::{FileBrowser, FileBrowserMode, FileBrowserPane};
use crate::pty::SessionStatus;
use crate::sftp::SftpConnectionStatus;
use crate::terminal_widget::TerminalView;

/// Calculates the terminal-screen rectangle inside the session border and tabs.
///
/// Mouse events arrive in frame coordinates, while PTY selection and rendering
/// use inner terminal coordinates. This helper mirrors the layout used by
/// `ui::render_session_view` so selection math matches what the user sees.
pub fn session_inner_rect(cols: u16, rows: u16, has_tabs: bool) -> Rect {
    let tab_h = if has_tabs { 1u16 } else { 0 };
    Rect {
        x: 1,
        y: 1,
        width: cols.saturating_sub(2),
        height: rows.saturating_sub(2 + tab_h),
    }
}

/// Converts a frame coordinate into a terminal-screen coordinate if it is inside.
pub fn frame_to_screen(col: u16, row: u16, inner: Rect) -> Option<ScreenPos> {
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
pub fn frame_to_screen_clamped(col: u16, row: u16, inner: Rect) -> ScreenPos {
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

/// Renders the entire application for the current frame.
///
/// Rendering reads from `App` but does not mutate it. The base view is drawn
/// first, then the tab bar and any active modal overlays are drawn on top.
pub fn render(frame: &mut Frame, app: &App) {
    let has_tabs = app.has_tabs();
    let [main_area, tab_bar_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(if has_tabs { 1 } else { 0 }),
    ])
    .areas(frame.area());

    match app.view {
        View::Hosts => render_hosts_view(frame, app, main_area),
        View::Session(idx) => render_session_view(frame, app, idx, main_area),
        View::FileTransfer(idx) => {
            if let Some(ft) = app.file_transfers.get(idx) {
                render_file_transfer_view(frame, ft, main_area);
                match &ft.mode {
                    FileBrowserMode::PasswordPrompt(input) => {
                        render_password_prompt(frame, &ft.alias, input);
                    }
                    FileBrowserMode::Creating(input) => {
                        render_mkdir_prompt(frame, input);
                    }
                    FileBrowserMode::ConfirmTransfer {
                        file, direction, ..
                    } => {
                        let dir_label = match direction {
                            crate::filebrowser::TransferDirection::Upload => "Upload",
                            crate::filebrowser::TransferDirection::Download => "Download",
                        };
                        render_confirm(
                            frame,
                            "Confirm Transfer",
                            &format!("{dir_label} '{file}'?\nFile already exists at destination."),
                        );
                    }
                    FileBrowserMode::ConfirmDelete { name, is_dir } => {
                        let kind = if *is_dir { "directory" } else { "file" };
                        render_confirm(
                            frame,
                            "Confirm Delete",
                            &format!("Delete {kind} '{name}'?"),
                        );
                    }
                    _ => {}
                }
            }
        }
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
        Mode::TestResult { alias, status } => {
            let status = status.lock().unwrap();
            render_test_result(frame, alias, &status);
        }
        Mode::TabHelp => render_tab_help(frame),
        Mode::Normal | Mode::Searching => {}
    }
}

/// Renders the host-browser layout: groups, hosts, details, and optional search.
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

/// Renders one embedded SSH session inside a bordered terminal panel.
///
/// The terminal contents come from a cloned `vt100::Screen`; overlays such as
/// scrollback position, disconnect status, and clipboard notices are ratatui UI
/// elements layered over the parsed terminal output.
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
    let sel = app.selection.map(|s| s.normalized());
    frame.render_widget(TerminalView::new(&screen).with_selection(sel), inner);

    if !is_dead && !screen.hide_cursor() {
        let (cursor_row, cursor_col) = screen.cursor_position();
        let x = inner.x + cursor_col;
        let y = inner.y + cursor_row;
        if x < inner.x + inner.width && y < inner.y + inner.height {
            frame.set_cursor_position(Position::new(x, y));
        }
    }

    let scrollback = session.scrollback_pos();
    if scrollback > 0 {
        let overlay_area = Rect {
            x: inner.x,
            y: inner.y + inner.height.saturating_sub(1),
            width: inner.width,
            height: 1,
        };
        let line = Line::from(vec![
            Span::styled(
                format!(" [{scrollback} lines up] "),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "scroll down or type to return",
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), overlay_area);
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
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled("Ctrl+T x to close", Style::default().fg(Color::DarkGray)),
        ]);
        frame.render_widget(Paragraph::new(line), overlay_area);
    }

    if app.clipboard_notice_visible() && inner.width > 0 && inner.height > 0 {
        let message = " Copied to clipboard ";
        let overlay_area = Rect {
            x: inner.x,
            y: inner.y,
            width: (message.len() as u16).min(inner.width),
            height: 1,
        };
        let line = Line::from(Span::styled(
            message,
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(Clear, overlay_area);
        frame.render_widget(Paragraph::new(line), overlay_area);
    }
}

/// Renders the dual-pane file transfer view for one `FileBrowser`.
///
/// Layout: two panes side-by-side showing local and remote directory listings,
/// with an optional progress bar row when a transfer is active, and a status
/// line at the bottom.
fn render_file_transfer_view(frame: &mut Frame, fb: &FileBrowser, area: Rect) {
    let has_progress = fb.has_active_transfer()
        || fb
            .active_transfer_progress()
            .is_some_and(|p| !matches!(p.status, crate::transfer::TransferStatus::InProgress));
    let show_search_bar =
        matches!(fb.mode, FileBrowserMode::Searching(_)) || !fb.focused_search().value().is_empty();

    let progress_height = if has_progress { 1 } else { 0 };
    let search_height = if show_search_bar { 1 } else { 0 };

    let vertical = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(progress_height),
        Constraint::Length(search_height),
        Constraint::Length(1),
    ])
    .split(area);
    let (panes_area, progress_area, search_area, status_area) =
        (vertical[0], vertical[1], vertical[2], vertical[3]);

    let [local_area, remote_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .areas(panes_area);

    render_file_pane(
        frame,
        &file_pane_title(
            "Local",
            &fb.local_path.to_string_lossy(),
            fb.local_search.value(),
        ),
        &fb.visible_local_entries(),
        fb.local_selected,
        fb.focus == FileBrowserPane::Local,
        local_area,
    );

    let status = fb.connection_status.lock().unwrap().clone();
    match status {
        SftpConnectionStatus::Connected | SftpConnectionStatus::ConnectedWithData { .. } => {
            render_file_pane(
                frame,
                &file_pane_title("Remote", &fb.remote_path, fb.remote_search.value()),
                &fb.visible_remote_entries(),
                fb.remote_selected,
                fb.focus == FileBrowserPane::Remote,
                remote_area,
            );
        }
        SftpConnectionStatus::Connecting => {
            render_file_pane_message(frame, "Remote", "Connecting...", Color::Yellow, remote_area);
        }
        SftpConnectionStatus::NeedsPassword => {
            render_file_pane_message(
                frame,
                "Remote",
                "Password required...",
                Color::Yellow,
                remote_area,
            );
        }
        SftpConnectionStatus::Failed(ref msg) => {
            let display = format!("{msg}\n\nPress R to reconnect");
            render_file_pane_message(frame, "Remote", &display, Color::Red, remote_area);
        }
    }

    if let Some(progress) = fb.active_transfer_progress() {
        render_transfer_progress(frame, &progress, progress_area);
    }

    if show_search_bar {
        render_file_search_bar(frame, fb, search_area);
    }

    render_file_transfer_status(frame, fb, status_area);
}

/// Renders the file transfer progress bar.
///
/// Format: ` >> Uploading file.txt  45% ████████░░░░  2.1/4.7 MB`
/// The bar width adapts to available terminal columns.
fn render_transfer_progress(
    frame: &mut Frame,
    progress: &crate::transfer::TransferProgress,
    area: Rect,
) {
    use crate::transfer::TransferStatus;

    let pct = if progress.total_bytes > 0 {
        ((progress.bytes_transferred as f64 / progress.total_bytes as f64) * 100.0) as u16
    } else {
        0
    };

    let transferred = format_size(progress.bytes_transferred);
    let total = format_size(progress.total_bytes);

    let prefix = format!(" >> {} {}  {}% ", progress.label, progress.file_name, pct);
    let suffix = format!("  {}/{} ", transferred, total);

    let bar_width = (area.width as usize)
        .saturating_sub(prefix.len() + suffix.len())
        .max(4);

    let filled = (bar_width as u64 * progress.bytes_transferred)
        .checked_div(progress.total_bytes)
        .unwrap_or(0) as usize;
    let empty = bar_width.saturating_sub(filled);

    let bar_color = match progress.status {
        TransferStatus::InProgress => Color::Cyan,
        TransferStatus::Completed => Color::Green,
        TransferStatus::Failed(_) => Color::Red,
        TransferStatus::Cancelled => Color::Yellow,
    };

    let line = Line::from(vec![
        Span::styled(
            prefix,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("\u{2588}".repeat(filled), Style::default().fg(bar_color)),
        Span::styled(
            "\u{2591}".repeat(empty),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(suffix, Style::default().fg(Color::DarkGray)),
    ]);

    frame.render_widget(Paragraph::new(line), area);
}

/// Renders one pane of the file browser with directory listing.
fn render_file_pane(
    frame: &mut Frame,
    title: &str,
    entries: &[&crate::sftp::FileEntry],
    selected: usize,
    focused: bool,
    area: Rect,
) {
    let instructions = if focused {
        Line::from(vec![
            " j/k ".into(),
            "Nav".blue().bold(),
            " y ".into(),
            "Copy".blue().bold(),
            " d ".into(),
            "Del".blue().bold(),
            " m ".into(),
            "Mkdir".blue().bold(),
            " / ".into(),
            "Search".blue().bold(),
            " . ".into(),
            "Hidden".blue().bold(),
            " s ".into(),
            "Sort ".blue().bold(),
        ])
    } else {
        Line::default()
    };

    let border_color = if focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let block = Block::bordered()
        .title(Line::from(format!(" {title} ").bold()).left_aligned())
        .title_bottom(instructions.centered())
        .border_set(border::THICK)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);

    let items: Vec<ListItem> = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let symlink_suffix = if entry.is_symlink { " @" } else { "" };

            let (name_span, size_str) = if entry.is_dir {
                (
                    Span::styled(
                        format!("{}/{symlink_suffix}", entry.name),
                        Style::default().fg(Color::Yellow),
                    ),
                    String::new(),
                )
            } else {
                (
                    if entry.is_symlink {
                        Span::styled(
                            format!("{}{symlink_suffix}", entry.name),
                            Style::default().fg(Color::Magenta),
                        )
                    } else {
                        Span::raw(entry.name.clone())
                    },
                    format_size(entry.size),
                )
            };

            let perms_str = entry.permissions_string().unwrap_or_default();
            let right_side = if perms_str.is_empty() {
                size_str.clone()
            } else if size_str.is_empty() {
                perms_str.clone()
            } else {
                format!("{perms_str}  {size_str}")
            };

            let available_width = inner.width as usize;
            let name_width = name_span.width();
            let right_width = right_side.len();

            let padding = if available_width > name_width + right_width + 2 {
                available_width - name_width - right_width - 1
            } else {
                1
            };

            let style = if i == selected {
                if focused {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Black).bg(Color::White)
                }
            } else {
                Style::default()
            };

            ListItem::new(
                Line::from(vec![
                    Span::raw(" "),
                    name_span,
                    Span::raw(" ".repeat(padding)),
                    Span::styled(right_side, Style::default().fg(Color::DarkGray)),
                ])
                .style(style),
            )
        })
        .collect();

    let list = List::new(items).block(block);
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(list, area, &mut state);
}

/// Builds pane titles and includes committed query text for filtered panes.
fn file_pane_title(label: &str, path: &str, search: &str) -> String {
    let query = search.trim();
    if query.is_empty() {
        format!("{label}: {path}")
    } else {
        format!("{label}: {path}  /{query}")
    }
}

/// Renders the filebrowser search input for the pane that currently owns it.
fn render_file_search_bar(frame: &mut Frame, fb: &FileBrowser, area: Rect) {
    let (pane, input, active) = match fb.mode {
        FileBrowserMode::Searching(FileBrowserPane::Local) => {
            (FileBrowserPane::Local, &fb.local_search, true)
        }
        FileBrowserMode::Searching(FileBrowserPane::Remote) => {
            (FileBrowserPane::Remote, &fb.remote_search, true)
        }
        _ => (fb.focus, fb.focused_search(), false),
    };

    let label = match pane {
        FileBrowserPane::Local => "Local",
        FileBrowserPane::Remote => "Remote",
    };
    let prompt_style = if active {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let hint = if active {
        if input.value().is_empty() {
            "  (Enter to keep, Esc to clear)"
        } else {
            ""
        }
    } else {
        "  (/ to edit)"
    };
    let prefix = format!("{label} / ");
    let line = Line::from(vec![
        Span::styled(prefix.clone(), prompt_style),
        Span::styled(input.value().to_string(), Style::default().fg(Color::White)),
        Span::styled(hint, Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(line), area);

    if active {
        let cursor_x = area.x + prefix.len() as u16 + input.visual_cursor() as u16;
        if cursor_x < area.x + area.width {
            frame.set_cursor_position(Position::new(cursor_x, area.y));
        }
    }
}

/// Renders a file pane with a centered status message instead of a listing.
fn render_file_pane_message(
    frame: &mut Frame,
    title: &str,
    message: &str,
    color: Color,
    area: Rect,
) {
    let block = Block::bordered()
        .title(Line::from(format!(" {title} ").bold()).left_aligned())
        .border_set(border::THICK)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines: Vec<Line> = message
        .lines()
        .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(color))).centered())
        .collect();
    let line_count = lines.len() as u16;

    if inner.height > 0 {
        let start_y = inner.y + inner.height.saturating_sub(line_count) / 2;
        let msg_area = Rect {
            x: inner.x,
            y: start_y,
            width: inner.width,
            height: line_count.min(inner.height),
        };
        frame.render_widget(Paragraph::new(lines), msg_area);
    }
}

/// Renders the status/instructions bar at the bottom of the file transfer view.
fn render_file_transfer_status(frame: &mut Frame, fb: &FileBrowser, area: Rect) {
    match &fb.mode {
        FileBrowserMode::TransferError(msg) => {
            let line = Line::from(vec![
                Span::styled(
                    " Error: ",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::styled(msg.as_str(), Style::default().fg(Color::Red)),
            ]);
            frame.render_widget(Paragraph::new(line), area);
        }
        _ => {
            if let Some(ref err) = fb.error {
                let line = Line::from(vec![
                    Span::styled(
                        " Error: ",
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(err.as_str(), Style::default().fg(Color::Red)),
                ]);
                frame.render_widget(Paragraph::new(line), area);
            } else {
                let sort_label = fb.sort_by.label();
                let dir_arrow = if fb.sort_ascending { "↑" } else { "↓" };
                let hidden_label = if fb.show_hidden { "on" } else { "off" };
                let line = Line::from(vec![
                    Span::styled(
                        format!(" Sort: {sort_label}{dir_arrow}"),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("  Hidden: {hidden_label}"),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        "  Tab: switch pane  Esc: close",
                        Style::default().fg(Color::DarkGray),
                    ),
                ]);
                frame.render_widget(Paragraph::new(line), area);
            }
        }
    }
}

/// Formats a byte count into a human-readable string (B, KB, MB, GB).
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Renders the password prompt overlay for SFTP authentication.
fn render_password_prompt(frame: &mut Frame, alias: &str, input: &tui_input::Input) {
    let area = centered_rect(50, 5, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::bordered()
        .title(Line::from(format!(" Passphrase/Password for {alias} ").bold()).centered())
        .title_bottom(
            Line::from(vec![
                " Enter ".into(),
                "Submit".blue().bold(),
                " Esc ".into(),
                "Cancel".blue().bold(),
            ])
            .centered(),
        )
        .border_set(border::THICK)
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let masked: String = "*".repeat(input.value().len());
    let line = Line::from(vec![
        Span::styled(
            "Password: ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(masked),
    ]);
    frame.render_widget(Paragraph::new(line), inner);

    let cursor_x = inner.x + 10 + input.visual_cursor() as u16;
    if cursor_x < inner.x + inner.width {
        frame.set_cursor_position(Position::new(cursor_x, inner.y));
    }
}

/// Renders the mkdir input overlay.
fn render_mkdir_prompt(frame: &mut Frame, input: &tui_input::Input) {
    let area = centered_rect(50, 5, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::bordered()
        .title(Line::from(" Create Directory ".bold()).centered())
        .title_bottom(
            Line::from(vec![
                " Enter ".into(),
                "Create".blue().bold(),
                " Esc ".into(),
                "Cancel".blue().bold(),
            ])
            .centered(),
        )
        .border_set(border::THICK);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let line = Line::from(vec![
        Span::styled(
            "Name: ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(input.value().to_string()),
    ]);
    frame.render_widget(Paragraph::new(line), inner);

    let cursor_x = inner.x + 6 + input.visual_cursor() as u16;
    if cursor_x < inner.x + inner.width {
        frame.set_cursor_position(Position::new(cursor_x, inner.y));
    }
}

/// Renders the bottom host/session tab bar.
///
/// Session tabs include unread and disconnected styling. The host browser is
/// treated as the first tab in the navigation model, so it is rendered alongside
/// session tabs instead of as a separate mode indicator.
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

    for (i, ft) in app.file_transfers.iter().enumerate() {
        spans.push(Span::raw(" "));
        let is_active = matches!(app.view, View::FileTransfer(idx) if idx == i);
        let label = format!(" F:{} ", ft.alias);

        let style = if is_active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Magenta)
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

/// Renders a connection-spawn error overlay.
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

/// Renders the asynchronous reachability-test status overlay.
///
/// The result is read from `TestStatus`, which may still be `Testing` while the
/// background TCP probe is running.
fn render_test_result(frame: &mut Frame, alias: &str, status: &TestStatus) {
    let area = centered_rect(60, 7, frame.area());
    frame.render_widget(Clear, area);

    match status {
        TestStatus::Testing => {
            let block = Block::bordered()
                .title(Line::from(" Testing Connection ".bold()).centered())
                .title_bottom(Line::from(vec![" Esc ".into(), "Cancel".blue().bold()]).centered())
                .border_set(border::THICK)
                .border_style(Style::default().fg(Color::Yellow));

            let text = vec![
                Line::from(format!("Host '{alias}':")),
                Line::default(),
                Line::from(Span::styled(
                    "Connecting...",
                    Style::default().fg(Color::Yellow),
                )),
            ];

            frame.render_widget(Paragraph::new(text).block(block), area);
        }
        TestStatus::Done { success, message } => {
            let (title, border_color) = if *success {
                (" Reachable ", Color::Green)
            } else {
                (" Unreachable ", Color::Red)
            };

            let block = Block::bordered()
                .title(Line::from(title.bold()).centered())
                .title_bottom(
                    Line::from(vec![" Enter/Esc ".into(), "Dismiss".blue().bold()]).centered(),
                )
                .border_set(border::THICK)
                .border_style(Style::default().fg(border_color));

            let result_color = if *success { Color::Green } else { Color::Red };

            let text = vec![
                Line::from(format!("Host '{alias}':")),
                Line::default(),
                Line::from(Span::styled(
                    message.to_string(),
                    Style::default().fg(result_color),
                )),
            ];

            frame.render_widget(Paragraph::new(text).block(block), area);
        }
    }
}

/// Renders the Ctrl+T tab-command help overlay.
fn render_tab_help(frame: &mut Frame) {
    let area = centered_rect(40, 16, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::bordered()
        .title(Line::from(" Tab Keys ".bold()).centered())
        .title_bottom(Line::from(vec![" any key ".into(), "Dismiss".blue().bold()]).centered())
        .border_set(border::THICK)
        .border_style(Style::default().fg(Color::Cyan));

    let key_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let text = vec![
        Line::from(vec![
            Span::styled("^T h  ", key_style),
            Span::raw("Switch to hosts"),
        ]),
        Line::from(vec![
            Span::styled("^T 1-9", key_style),
            Span::raw(" Switch to tab N"),
        ]),
        Line::from(vec![
            Span::styled("^T n  ", key_style),
            Span::raw("Next tab"),
        ]),
        Line::from(vec![
            Span::styled("^T p  ", key_style),
            Span::raw("Previous tab"),
        ]),
        Line::from(vec![
            Span::styled("^T x  ", key_style),
            Span::raw("Close current tab"),
        ]),
        Line::from(vec![
            Span::styled("^T f  ", key_style),
            Span::raw("File transfer (from session)"),
        ]),
        Line::from(vec![
            Span::styled("^T s  ", key_style),
            Span::raw("New local shell"),
        ]),
        Line::from(vec![
            Span::styled("^T ^T ", key_style),
            Span::raw("Send literal ^T"),
        ]),
    ];

    frame.render_widget(Paragraph::new(text).block(block), area);
}

/// Builds a consistently styled pane border that reflects focus state.
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

/// Renders the group/filter navigation pane.
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
            "Del ".blue().bold(),
        ])
    } else {
        Line::default()
    };

    let block = pane_border("Groups", focused).title_bottom(instructions.centered());

    let list = List::new(items).block(block);
    let mut state = ListState::default().with_selected(Some(app.group_selected));
    frame.render_stateful_widget(list, area, &mut state);
}

/// Renders the host list for the active group filter or search query.
///
/// Header rows are visually distinct from host rows, matching the navigation
/// behavior in `App` where headers are skipped for host actions.
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
            " Enter ".into(),
            "Connect".blue().bold(),
            " t ".into(),
            "Test".blue().bold(),
            " a ".into(),
            "Add".blue().bold(),
            " e ".into(),
            "Edit".blue().bold(),
            " c ".into(),
            "Clone".blue().bold(),
            " d ".into(),
            "Del".blue().bold(),
            " f ".into(),
            "Files ".blue().bold(),
        ])
    } else {
        Line::default()
    };

    let block = pane_border("Hosts", focused).title_bottom(instructions.centered());

    let list = List::new(items).block(block);
    let mut state = ListState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

/// Renders details for the currently selected host.
///
/// Long free-form comments are wrapped to fit the pane width while preserving
/// alignment with the other key/value rows.
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

    if !host.details.is_empty() {
        let label_width: usize = 17; // "{key:>15}: " = 15 + 2
        let content_width = (area.width as usize).saturating_sub(label_width + 2); // 2 for border
        if content_width > 0 {
            let wrapped = textwrap::wrap(&host.details, content_width);
            for (i, chunk) in wrapped.iter().enumerate() {
                if i == 0 {
                    lines.push(Line::from(vec![
                        label("Comments"),
                        Span::raw(chunk.to_string()),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::raw(" ".repeat(label_width)),
                        Span::raw(chunk.to_string()),
                    ]));
                }
            }
        }
    }

    lines.push(Line::from(vec![
        label("Last Accessed"),
        Span::raw(&host.last_accessed),
    ]));

    let detail = Paragraph::new(lines).block(block);
    frame.render_widget(detail, area);
}

/// Returns a fixed-size rectangle centered within another rectangle.
///
/// Modal overlays use this helper so their layout remains independent from the
/// main pane layout underneath.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    area
}

/// Renders the add/edit host form overlay.
///
/// Cursor placement uses `tui-input`'s visual cursor so multi-byte characters
/// and wide glyphs keep the terminal cursor aligned with displayed text.
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

/// Renders the add/rename group input overlay.
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

/// Renders the search bar and places the cursor when search mode is active.
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

/// Renders the SSH extra-options list overlay.
///
/// If an inner key/value entry form is active, rendering is delegated to
/// `render_extras_entry`; otherwise this draws the list of existing extras and
/// any list-level validation error.
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

/// Renders the inner add/edit form for a single SSH extra option.
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

/// Renders a generic yes/no confirmation overlay.
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
