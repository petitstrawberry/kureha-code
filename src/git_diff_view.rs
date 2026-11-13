//! The lines of a Git change, drawn directly: old and new line numbers, then
//! the text in the editor font, on a green or red band for added and removed
//! lines. Only the lines in view are painted.
use super::*;
use kureha_code::{
    git::diff::{FileDiff, Kind},
    theme::Theme,
};
use scarlet_ui::{
    element::{RenderElement, UpdateResult},
    event::Phase,
    graphics,
    renderer::PaintContext,
};
use std::any::Any;

#[derive(Clone)]
pub(super) struct DiffLines {
    pub diff: Arc<FileDiff>,
    pub theme: Theme,
    pub font: Option<FontStack>,
}

impl View for DiffLines {
    fn create_element(&self) -> Box<dyn Element> {
        Box::new(RenderElement::with_view_children_and_updater(
            self.clone(),
            |view| Renderer::new(view.clone()),
            |render, view| render.update(view),
            |_| Vec::new(),
        ))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Space around the lines and between the columns.
const PAD: f32 = 10.0;

struct Renderer {
    view: DiffLines,
    size: Size,
    viewport: Option<Rect>,
    line_height: f32,
    digit_width: f32,
}

impl Renderer {
    fn new(view: DiffLines) -> Self {
        let mut renderer = Self {
            view,
            size: Size::new(0.0, 0.0),
            viewport: None,
            line_height: 0.0,
            digit_width: 0.0,
        };
        renderer.measure();
        renderer
    }
    fn font_size(&self) -> f32 {
        self.view.theme.editor_font_size() - 1.0
    }
    fn measure(&mut self) {
        let size = self.font_size();
        let (height, width) = match &self.view.font {
            Some(font) => (
                graphics::line_height_sized_with_font_stack(size, font) as f32,
                graphics::measure_text_sized_with_font_stack("0000000000", size, font).0 as f32,
            ),
            None => (
                size * 1.4,
                graphics::measure_text_sized("0000000000", size).0 as f32,
            ),
        };
        self.line_height = height.max(size) + 2.0;
        self.digit_width = width / 10.0;
    }
    fn update(&mut self, view: &DiffLines) -> UpdateResult {
        let same = Arc::ptr_eq(&self.view.diff, &view.diff)
            && self.view.theme == view.theme
            && self.view.font.as_ref().map(FontStack::cache_id)
                == view.font.as_ref().map(FontStack::cache_id);
        self.view = view.clone();
        if same {
            UpdateResult::NoChange
        } else {
            self.measure();
            UpdateResult::Updated
        }
    }
    /// Width of one line-number column.
    fn number_width(&self) -> f32 {
        let widest = self
            .view
            .diff
            .lines
            .iter()
            .flat_map(|line| [line.old, line.new])
            .flatten()
            .max()
            .unwrap_or(1);
        (widest.to_string().len().max(2) as f32) * self.digit_width
    }
}

impl ElementRenderObject for Renderer {
    fn layout(&mut self, c: LayoutConstraints) -> Size {
        let width = if c.max_width.is_finite() {
            c.max_width
        } else {
            800.0
        };
        let height = self.view.diff.lines.len() as f32 * self.line_height + PAD * 2.0;
        self.size = Size::new(width, height);
        self.size
    }
    fn depends_on_viewport(&self) -> bool {
        true
    }
    fn set_viewport_hint(&mut self, viewport: Rect) -> bool {
        if self.viewport == Some(viewport) {
            return false;
        }
        self.viewport = Some(viewport);
        true
    }
    fn update_needs_layout(&self) -> bool {
        false
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
    fn hit_test(&self, p: Point) -> bool {
        Rect::new(Point::ZERO, self.size).contains(p)
    }
    fn handle_event(&mut self, _event: &Event, _phase: Phase) -> bool {
        false
    }
    fn paint<'a>(&'a self, ctx: &mut PaintContext<'a>, origin: Point) -> bool {
        let theme = &self.view.theme;
        let editor = theme.editor_colors();
        let colors = theme.colors();
        let ansi = theme.terminal_palette().ansi;
        let (added, removed) = (ansi[2], ansi[1]);
        let visible = self.viewport.unwrap_or(Rect::new(Point::ZERO, self.size));
        let first = ((visible.origin.y - PAD) / self.line_height)
            .floor()
            .max(0.0) as usize;
        let count = (visible.size.height / self.line_height).ceil() as usize + 2;
        let number = self.number_width();
        let text_x = PAD + number * 2.0 + PAD * 2.0;
        let size = self.font_size();
        ctx.push_clip(Rect::new(origin, self.size));
        for (index, line) in self
            .view
            .diff
            .lines
            .iter()
            .enumerate()
            .skip(first)
            .take(count)
        {
            let y = origin.y + PAD + index as f32 * self.line_height;
            let band = match line.kind {
                Kind::Added => Some(added.with_opacity(0.16)),
                Kind::Removed => Some(removed.with_opacity(0.16)),
                Kind::Hunk => Some(colors.current_line),
                Kind::Context => None,
            };
            if let Some(band) = band {
                ctx.fill_rect(
                    Rect::from_xywh(origin.x, y, self.size.width, self.line_height),
                    band,
                );
            }
            let mut draw = |x: f32, text: String, color: Color| {
                let point = Point::new(origin.x + x, y + 1.0);
                match &self.view.font {
                    Some(font) => ctx.draw_text_with_font_stack(
                        point,
                        text,
                        color,
                        size,
                        Arc::new(font.clone()),
                    ),
                    None => ctx.draw_text(point, text, color, size),
                }
            };
            for (column, value) in [line.old, line.new].into_iter().enumerate() {
                if let Some(value) = value {
                    let text = value.to_string();
                    let right = PAD + number * (column + 1) as f32 + PAD * column as f32;
                    let x = right - text.len() as f32 * self.digit_width;
                    draw(x, text, colors.muted);
                }
            }
            let (mark, color) = match line.kind {
                Kind::Hunk => ("", colors.muted),
                Kind::Context => (" ", editor.foreground),
                Kind::Removed => ("-", editor.foreground),
                Kind::Added => ("+", editor.foreground),
            };
            let text = format!("{mark}{}", line.text.replace('\t', "    "));
            draw(text_x, text, color);
        }
        ctx.pop_clip();
        true
    }
}
