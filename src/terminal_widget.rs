use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Widget,
};

use crate::app::ScreenPos;

/// Ratatui widget that paints a parsed `vt100::Screen` into a frame area.
///
/// The PTY reader produces an in-memory terminal screen rather than writing
/// directly to stdout. This widget bridges that representation into ratatui by
/// copying each visible cell's text and style into the frame buffer.
pub struct TerminalView<'a> {
    screen: &'a vt100::Screen,
    selection: Option<(ScreenPos, ScreenPos)>,
}

impl<'a> TerminalView<'a> {
    /// Creates a terminal widget for a parsed screen snapshot.
    pub fn new(screen: &'a vt100::Screen) -> Self {
        Self {
            screen,
            selection: None,
        }
    }

    /// Adds an optional normalized selection range to highlight while rendering.
    ///
    /// Selection coordinates are terminal-screen coordinates, not frame
    /// coordinates. The range end is exclusive so it can be shared with
    /// `vt100::Screen::contents_between`.
    pub fn with_selection(mut self, selection: Option<(ScreenPos, ScreenPos)>) -> Self {
        self.selection = selection;
        self
    }
}

/// Returns whether a terminal cell lies inside a normalized selection range.
///
/// `start` is inclusive and `end` is exclusive. Multi-line selections include
/// every cell between the starting column on the first row and the ending column
/// on the final row.
fn is_selected(row: u16, col: u16, start: &ScreenPos, end: &ScreenPos) -> bool {
    if row < start.row || row > end.row {
        return false;
    }
    if start.row == end.row {
        return col >= start.col && col < end.col;
    }
    if row == start.row {
        return col >= start.col;
    }
    if row == end.row {
        return col < end.col;
    }
    true
}

impl Widget for TerminalView<'_> {
    /// Renders visible terminal cells into ratatui's buffer.
    ///
    /// The widget clips to the smaller of the frame area and the parsed terminal
    /// size. Empty terminal cells are drawn as spaces with their style so
    /// background colors still appear correctly.
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (screen_rows, screen_cols) = self.screen.size();
        let rows = area.height.min(screen_rows);
        let cols = area.width.min(screen_cols);

        for row in 0..rows {
            for col in 0..cols {
                let Some(cell) = self.screen.cell(row, col) else {
                    continue;
                };

                let x = area.x + col;
                let y = area.y + row;

                let fg = convert_color(cell.fgcolor());
                let bg = convert_color(cell.bgcolor());

                let mut modifiers = Modifier::empty();
                if cell.bold() {
                    modifiers |= Modifier::BOLD;
                }
                if cell.italic() {
                    modifiers |= Modifier::ITALIC;
                }
                if cell.underline() {
                    modifiers |= Modifier::UNDERLINED;
                }
                if cell.inverse() {
                    modifiers |= Modifier::REVERSED;
                }

                let mut style = Style::default().fg(fg).bg(bg).add_modifier(modifiers);

                if let Some((ref start, ref end)) = self.selection
                    && is_selected(row, col, start, end)
                {
                    style = style.add_modifier(Modifier::REVERSED);
                }

                let contents = cell.contents();

                if contents.is_empty() {
                    buf[(x, y)].set_char(' ').set_style(style);
                } else {
                    buf[(x, y)].set_symbol(contents).set_style(style);
                }
            }
        }
    }
}

/// Converts a vt100 color into the equivalent ratatui color value.
///
/// `Color::Default` maps to `Reset` so ratatui leaves the terminal's default
/// foreground/background in effect rather than forcing a specific color.
fn convert_color(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}
