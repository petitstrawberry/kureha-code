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
    let mut colors =
        vec![(vt100::Color::Default, vt100::Color::Default); rows as usize * cols as usize];
    for row in 0..rows {
        for col in 0..cols {
            let Some(source) = screen.cell(row, col) else {
                continue;
            };
            colors[row as usize * cols as usize + col as usize] =
                (source.fgcolor(), source.bgcolor());
            let mut cell = TextGridCell::new(
                if source.is_wide_continuation() {
                    '\0'
                } else {
                    // TextGrid stores one scalar per cell; keep the base
                    // character without shifting neighbors for combining marks.
                    source.contents().chars().next().unwrap_or(' ')
                },
                color(source.fgcolor(), foreground()),
                color(source.bgcolor(), background()),
            );
            cell.bold = source.bold();
            cell.faint = source.dim();
            cell.italic = source.italic();
            cell.underline = source.underline();
            cell.inverse = source.inverse();
            grid.set_cell(col as usize, row as usize, cell);
        }
    }
    let (row, col) = screen.cursor_position();
    // VT100 keeps the cursor past the right margin while a wrap is pending.
    // Only normalize the displayed position, leaving the parser state intact.
    let row = row.min(rows.saturating_sub(1));
    let mut col = col.min(cols.saturating_sub(1));
    if col > 0
        && screen
            .cell(row, col)
            .is_some_and(|cell| cell.is_wide_continuation())
    {
        col -= 1;
    }
    let mut cursor = TextGridCursor::new(col as usize, row as usize);
    cursor.visible = !screen.hide_cursor() && screen.scrollback() == 0;
    Snapshot {
        grid,
        colors,
        cursor,
        application_cursor: screen.application_cursor(),
        bracketed_paste: screen.bracketed_paste(),
        status,
    }
}
fn color(value: vt100::Color, fallback: Color) -> Color {
    super::TerminalPalette::default().resolve(value, fallback)
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
    pub cell_size: Size,
    pub content: V,
}
impl<V: View + Clone> View for MeasuredView<V> {
    fn create_element(&self) -> Box<dyn Element> {
        Box::new(RenderElement::with_view_children_and_updater(
            self.clone(),
            |view| Measure {
                terminal: view.terminal.clone(),
                cell_size: view.cell_size,
                size: Size::ZERO,
            },
            |object, view| {
                object.terminal = view.terminal.clone();
                object.cell_size = view.cell_size;
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
    cell_size: Size,
    size: Size,
}
impl ElementRenderObject for Measure {
    fn layout(&mut self, constraints: LayoutConstraints) -> Size {
        let grid = self.terminal.shared.grid.get();
        // Stack intrinsic passes may supply tight infinity via a flexible frame.
        // Never allocate a backing bitmap or resize a PTY from that probe.
        let finite = constraints.max_width.is_finite() && constraints.max_height.is_finite();
        self.size = Size::new(
            if constraints.max_width.is_finite() {
                constraints.max_width.max(0.0)
            } else {
                grid.columns() as f32 * self.cell_size.width
            },
            if constraints.max_height.is_finite() {
                constraints.max_height.max(0.0)
            } else {
                grid.rows() as f32 * self.cell_size.height
            },
        );
        if finite && self.size.width > 0.0 && self.size.height > 0.0 {
            self.terminal.resize(
                (self.size.width / self.cell_size.width) as u16,
                (self.size.height / self.cell_size.height) as u16,
            );
        }
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
