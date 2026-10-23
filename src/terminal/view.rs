//! Rebuild only the terminal when its cursor or focus changes. TextGrid owns
//! the screen buffer subscription; the workbench need not rebuild on PTY I/O.
use super::Terminal;
use scarlet_ui::{ComponentElement, Element, FontStack, Listenable, Size, View, graphics};
use std::any::Any;

pub(super) fn cell_size(font: Option<&FontStack>) -> Size {
    match font {
        Some(font) => Size::new(
            graphics::measure_text_sized_with_font_stack("M", 14.0, font)
                .0
                .max(1) as f32,
            graphics::line_height_sized_with_font_stack(14.0, font).max(1) as f32,
        ),
        None => Size::new(super::CELL_WIDTH, super::CELL_HEIGHT),
    }
}

#[derive(Clone)]
pub(super) struct LiveView {
    pub terminal: Terminal,
    pub font: Option<FontStack>,
}

impl View for LiveView {
    fn create_element(&self) -> Box<dyn Element> {
        Box::new(ComponentElement::new_with_builder(self.clone(), |view| {
            Box::new(view.terminal.content(view.font.clone()))
        }))
    }

    fn listenables(&self) -> Vec<&dyn Listenable> {
        vec![&self.terminal.shared.cursor, &self.terminal.shared.focused]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
