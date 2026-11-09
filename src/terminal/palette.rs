use scarlet_ui::{Color, TextGridBuffer};

/// Default colors and the 16 configurable ANSI slots. Extended indexed and
/// explicit RGB colors belong to the application running inside the terminal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalPalette {
    pub background: Color,
    pub foreground: Color,
    pub cursor: Color,
    pub ansi: [Color; 16],
}

impl Default for TerminalPalette {
    fn default() -> Self {
        Self {
            background: Color::rgb(12, 14, 18),
            foreground: Color::rgb(230, 232, 235),
            cursor: Color::rgba(230, 232, 235, 0.65),
            ansi: [
                (0, 0, 0),
                (205, 49, 49),
                (13, 188, 121),
                (229, 229, 16),
                (36, 114, 200),
                (188, 63, 188),
                (17, 168, 205),
                (229, 229, 229),
                (102, 102, 102),
                (241, 76, 76),
                (35, 209, 139),
                (245, 245, 67),
                (59, 142, 234),
                (214, 112, 214),
                (41, 184, 219),
                (255, 255, 255),
            ]
            .map(|(r, g, b)| Color::rgb(r, g, b)),
        }
    }
}

impl TerminalPalette {
    pub(super) fn resolve(&self, value: vt100::Color, fallback: Color) -> Color {
        match value {
            vt100::Color::Default => fallback,
            vt100::Color::Rgb(r, g, b) => Color::rgb(r, g, b),
            vt100::Color::Idx(index @ 0..=15) => self.ansi[index as usize],
            vt100::Color::Idx(index @ 16..=231) => {
                let n = index - 16;
                let c = |v| if v == 0 { 0 } else { 55 + v * 40 };
                Color::rgb(c(n / 36), c(n / 6 % 6), c(n % 6))
            }
            vt100::Color::Idx(index) => {
                let level = 8 + (index - 232) * 10;
                Color::rgb(level, level, level)
            }
        }
    }

    pub(super) fn recolor(
        &self,
        mut grid: TextGridBuffer,
        colors: &[(vt100::Color, vt100::Color)],
    ) -> TextGridBuffer {
        for row in 0..grid.rows() {
            for col in 0..grid.columns() {
                let (fg, bg) = colors
                    .get(row * grid.columns() + col)
                    .copied()
                    .unwrap_or((vt100::Color::Default, vt100::Color::Default));
                let mut cell = grid.cell(col, row).unwrap();
                cell.foreground = self.resolve(fg, self.foreground);
                cell.background = self.resolve(bg, self.background);
                grid.set_cell(col, row, cell);
            }
        }
        grid
    }
}
