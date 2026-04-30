use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Widget,
};

pub struct TerminalView<'a> {
    screen: &'a vt100::Screen,
}

impl<'a> TerminalView<'a> {
    pub fn new(screen: &'a vt100::Screen) -> Self {
        Self { screen }
    }
}

impl Widget for TerminalView<'_> {
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

                let style = Style::default().fg(fg).bg(bg).add_modifier(modifiers);
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

fn convert_color(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}
