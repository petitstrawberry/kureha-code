use super::{Snapshot, Terminal, TerminalStatus};
use scarlet_ui::element::UpdateResult;
use scarlet_ui::{
    Color, Element, ElementRenderObject, LayoutConstraints, Listenable, RenderElement, Size,
    TextGridBuffer, TextGridCell, TextGridCursor, View,
};
use std::any::Any;

pub(super) fn background() -> Color {
    Color::rgb(12, 14, 18)
}
fn foreground() -> Color {
    Color::rgb(230, 232, 235)
}
pub(super) fn blank((cols, rows): (u16, u16)) -> TextGridBuffer {
    TextGridBuffer::new(
        cols as usize,
        rows as usize,
        TextGridCell::blank(foreground(), background()),
    )
}
pub(super) fn snapshot(screen: &vt100::Screen, status: TerminalStatus) -> Snapshot {
    let (rows, cols) = screen.size();
    let mut grid = blank((cols, rows));
    for row in 0..rows {
        for col in 0..cols {
            let Some(source) = screen.cell(row, col) else {
                continue;
            };
            let mut cell = TextGridCell::new(
                if source.is_wide_continuation() {
                    '\0'
                } else {
                    source.contents().chars().next().unwrap_or(' ')
                },
                color(source.fgcolor(), foreground()),
                color(source.bgcolor(), background()),
            );
            cell.bold = source.bold();
            cell.italic = source.italic();
            cell.underline = source.underline();
            cell.inverse = source.inverse();
            grid.set_cell(col as usize, row as usize, cell);
        }
    }
    let (row, col) = screen.cursor_position();
    let mut cursor = TextGridCursor::new(col as usize, row as usize);
    cursor.visible = !screen.hide_cursor() && screen.scrollback() == 0;
    Snapshot {
        grid,
        cursor,
        application_cursor: screen.application_cursor(),
        bracketed_paste: screen.bracketed_paste(),
        status,
    }
}
fn color(value: vt100::Color, fallback: Color) -> Color {
    match value {
        vt100::Color::Default => fallback,
        vt100::Color::Rgb(r, g, b) => Color::rgb(r, g, b),
        vt100::Color::Idx(index) => {
            const ANSI: [(u8, u8, u8); 16] = [
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
            ];
            let (r, g, b) = match index {
                0..=15 => ANSI[index as usize],
                16..=231 => {
                    let n = index - 16;
                    let c = |v| if v == 0 { 0 } else { 55 + v * 40 };
                    (c(n / 36), c(n / 6 % 6), c(n % 6))
                }
                _ => {
                    let level = 8 + (index - 232) * 10;
                    (level, level, level)
                }
            };
            Color::rgb(r, g, b)
        }
    }
}

/// A bounded CSI query recognizer supplements vt100's screen parser, which
/// intentionally does not perform terminal I/O. OSC/DCS payloads are ignored.
#[derive(Default)]
pub(super) struct Queries {
    state: u8,
    sequence: Vec<u8>,
}
impl Queries {
    pub fn feed(&mut self, byte: u8, screen: &vt100::Screen) -> Option<Vec<u8>> {
        match self.state {
            0 if byte == 0x1b => self.state = 1,
            1 => match byte {
                b'[' => {
                    self.sequence.clear();
                    self.state = 2;
                }
                b']' | b'P' | b'_' | b'^' => self.state = 3,
                _ => self.state = 0,
            },
            2 => {
                if (0x40..=0x7e).contains(&byte) {
                    self.state = 0;
                    let reply = match (self.sequence.as_slice(), byte) {
                        (b"6", b'n') => {
                            let (row, col) = screen.cursor_position();
                            format!("\x1b[{};{}R", row + 1, col + 1).into_bytes()
                        }
                        (b"5", b'n') => b"\x1b[0n".to_vec(),
                        (b"" | b"0", b'c') => b"\x1b[?1;2c".to_vec(),
                        (b">" | b">0", b'c') => b"\x1b[>0;1;0c".to_vec(),
                        _ => return None,
                    };
                    return Some(reply);
                }
                if self.sequence.len() < 32 {
                    self.sequence.push(byte);
                } else {
                    self.state = 0;
                    self.sequence.clear();
                }
            }
            3 if byte == 7 => self.state = 0,
            3 if byte == 0x1b => self.state = 4,
            4 => self.state = if byte == b'\\' { 0 } else { 3 },
            _ => {}
        }
        None
    }
}

/// Measure the actual pane allocation, including SplitView divider drags.
#[derive(Clone)]
pub(super) struct MeasuredView<V: View + Clone> {
    pub terminal: Terminal,
    pub content: V,
}
impl<V: View + Clone> View for MeasuredView<V> {
    fn create_element(&self) -> Box<dyn Element> {
        Box::new(RenderElement::with_view_children_and_updater(
            self.clone(),
            |view| Measure {
                terminal: view.terminal.clone(),
                size: Size::ZERO,
            },
            |object, view| {
                object.terminal = view.terminal.clone();
                UpdateResult::Updated
            },
            |view| vec![view.content.clone_view()],
        ))
    }
    fn listenables(&self) -> Vec<&dyn Listenable> {
        self.content.listenables()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
struct Measure {
    terminal: Terminal,
    size: Size,
}
impl ElementRenderObject for Measure {
    fn layout(&mut self, constraints: LayoutConstraints) -> Size {
        let grid = self.terminal.shared.grid.get();
        self.size = constraints.constrain(Size::new(
            if constraints.max_width.is_finite() {
                constraints.max_width
            } else {
                grid.columns() as f32 * super::CELL_WIDTH
            },
            if constraints.max_height.is_finite() {
                constraints.max_height
            } else {
                grid.rows() as f32 * super::CELL_HEIGHT
            },
        ));
        self.terminal
            .resize_pixels(self.size.width, self.size.height);
        self.size
    }
    fn size(&self) -> Size {
        self.size
    }
    fn render(&mut self) {}
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
