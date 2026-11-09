//! IME composition is local to the terminal view; only committed text reaches
//! the PTY. Shell output is not an editable surrounding-text buffer.
use super::*;
use scarlet_ui::{
    Point, Rect, Size, element::TextInputElementState, graphics, renderer::PaintContext,
};

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Preedit {
    text: String,
    cursor: usize,
    anchor: usize,
}

fn boundary(text: &str, byte: u32) -> usize {
    let mut byte = (byte as usize).min(text.len());
    while !text.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

impl Terminal {
    pub(super) fn clear_preedit(&self) {
        if !self.shared.preedit.get().text.is_empty() {
            self.shared.preedit.set(Preedit::default());
        }
    }

    pub(super) fn composing(&self) -> bool {
        !self.shared.preedit.get().text.is_empty()
    }

    pub(super) fn ime_event(&self, event: &Event) -> bool {
        match event {
            Event::TextInputPreedit {
                text,
                cursor_byte,
                anchor_byte,
                ..
            } => {
                self.shared.preedit.set(Preedit {
                    cursor: boundary(text, *cursor_byte),
                    anchor: boundary(text, *anchor_byte),
                    text: text.clone(),
                });
                true
            }
            Event::TextInputCommit { text, .. } => {
                if text.is_empty() || self.send_input(text.as_bytes()) {
                    self.clear_preedit();
                    true
                } else {
                    false
                }
            }
            // We advertise no surrounding text: never translate a platform
            // deletion/selection request into edits of the shell's output.
            Event::TextInputDeleteSurroundingText { .. }
            | Event::TextInputSelection { .. }
            | Event::TextInputDone { .. } => true,
            _ => false,
        }
    }

    fn preedit_geometry(
        &self,
        size: Size,
        cell: Size,
        font: Option<&FontStack>,
    ) -> (Rect, f32, f32, f32) {
        let preedit = self.shared.preedit.get();
        let measure = |text: &str| match font {
            Some(font) => graphics::measure_text_sized_with_font_stack(text, 14.0, font).0 as f32,
            None => graphics::measure_text_sized(text, 14.0).0 as f32,
        };
        let cursor = self.shared.cursor.get();
        let width = measure(&preedit.text);
        let caret = measure(&preedit.text[..preedit.cursor]);
        let anchor = measure(&preedit.text[..preedit.anchor]);
        let extent = (width + 1.0).max(cell.width);
        let x = (cursor.column as f32 * cell.width).min((size.width - extent).max(0.0));
        let y = (cursor.row as f32 * cell.height).min((size.height - cell.height).max(0.0));
        // Long compositions scroll horizontally inside the available viewport.
        let offset = (caret + 1.0 - (size.width - x)).max(0.0);
        (
            Rect::from_xywh(x, y, extent.min(size.width), cell.height.min(size.height)),
            caret,
            anchor,
            offset,
        )
    }

    pub(super) fn sync_ime(&self, size: Size, cell: Size, font: Option<&FontStack>) {
        if !self.focused() || !self.is_running() {
            self.clear_preedit();
            if self.shared.text_input.get().is_some() {
                self.shared.text_input.set(None);
            }
            return;
        }
        let (rect, caret, _, offset) = self.preedit_geometry(size, cell, font);
        let input_rect = Rect::new(Point::ZERO, size);
        let cursor_rect = Rect::from_xywh(
            rect.origin.x + caret - offset,
            rect.origin.y,
            1.0,
            rect.size.height,
        );
        if self
            .shared
            .text_input
            .get()
            .is_some_and(|old| old.input_rect == input_rect && old.cursor_rect == cursor_rect)
        {
            return;
        }
        self.shared.text_input.set(Some(TextInputElementState {
            editor_id: None,
            input_rect,
            cursor_rect,
            surrounding_text: String::new(),
            cursor_byte: 0,
            anchor_byte: 0,
            automatic_software_keyboard: true,
        }));
    }

    pub(super) fn paint_preedit(
        &self,
        ctx: &mut PaintContext<'_>,
        origin: Point,
        size: Size,
        cell: Size,
        font: Option<&FontStack>,
    ) -> bool {
        let preedit = self.shared.preedit.get();
        if !self.focused() || preedit.text.is_empty() {
            return false;
        }
        let (mut rect, caret, anchor, offset) = self.preedit_geometry(size, cell, font);
        rect.origin.x += origin.x;
        rect.origin.y += origin.y;
        let palette = self.shared.palette.get();
        ctx.push_clip(rect);
        ctx.fill_rect(rect, palette.background);
        let point = Point::new(rect.origin.x - offset, rect.origin.y);
        if caret != anchor {
            ctx.fill_rect(
                Rect::from_xywh(
                    point.x + caret.min(anchor),
                    point.y,
                    (caret - anchor).abs(),
                    rect.size.height,
                ),
                palette.cursor.with_opacity(0.22),
            );
        }
        if let Some(font) = font {
            ctx.draw_text_with_font_stack(
                point,
                preedit.text,
                palette.foreground,
                14.0,
                Arc::new(font.clone()),
            );
        } else {
            ctx.draw_text(point, preedit.text, palette.foreground, 14.0);
        }
        ctx.fill_rect(
            Rect::from_xywh(
                rect.origin.x,
                rect.origin.y + rect.size.height - 1.0,
                rect.size.width,
                1.0,
            ),
            palette.foreground,
        );
        ctx.fill_rect(
            Rect::from_xywh(point.x + caret, point.y, 1.0, rect.size.height),
            palette.cursor,
        );
        ctx.pop_clip();
        true
    }
}
