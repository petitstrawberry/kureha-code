//! Code-sized, monospaced completion labels with bounded secondary information.
use scarlet_ui::{
    element::{RenderElement, UpdateResult},
    graphics::{self, FontStack},
    prelude::*,
    renderer::PaintContext,
};
use std::{any::Any, sync::Arc};

#[derive(Clone)]
pub(super) struct Label {
    pub text: String,
    pub detail: String,
    pub query: String,
    pub font: Option<Arc<FontStack>>,
    pub font_size: f32,
    pub foreground: Color,
    pub accent: Color,
}

impl Label {
    fn measure(&self, text: &str) -> f32 {
        self.font.as_ref().map_or_else(
            || graphics::measure_text_sized(text, self.font_size).0,
            |font| graphics::measure_text_sized_with_font_stack(text, self.font_size, font).0,
        ) as f32
    }

    fn fit(&self, text: &str, width: f32) -> String {
        if self.measure(text) <= width {
            return text.to_owned();
        }
        let width = width - self.measure("…");
        if width <= 0.0 {
            return String::new();
        }
        let boundaries: Vec<_> = text.char_indices().map(|(i, _)| i).collect();
        let count = boundaries.partition_point(|&end| self.measure(&text[..end]) <= width);
        format!("{}…", &text[..boundaries[count.saturating_sub(1)]])
    }
}

impl View for Label {
    fn create_element(&self) -> Box<dyn Element> {
        Box::new(RenderElement::with_view_children_and_updater(
            self.clone(),
            |view| Renderer {
                label: view.clone(),
                size: Size::ZERO,
                runs: Vec::new(),
            },
            |render, view| {
                render.label = view.clone();
                UpdateResult::Updated
            },
            |_| Vec::new(),
        ))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct Renderer {
    label: Label,
    size: Size,
    runs: Vec<(f32, String, Color)>,
}

impl ElementRenderObject for Renderer {
    fn layout(&mut self, c: LayoutConstraints) -> Size {
        let width = c.max_width.min(516.0).max(0.0);
        let label = &self.label;
        self.size = Size::new(width, label.font_size + 12.0);
        self.runs.clear();
        // The symbol has priority. Hide detail in narrow windows rather than
        // squeezing both columns into unreadable slivers.
        let detail_width = if width >= 300.0 {
            label.measure(&label.detail).min(width * 0.40)
        } else {
            0.0
        };
        let gap = if detail_width > 0.0 { 20.0 } else { 0.0 };
        let text = label.fit(&label.text, (width - detail_width - gap).max(0.0));
        let mut query = label.query.chars().peekable();
        let mut x = 0.0;
        let mut run = String::new();
        let mut matched_run = false;
        for ch in text.chars() {
            let matched = query
                .peek()
                .is_some_and(|q| q.to_lowercase().eq(ch.to_lowercase()));
            if matched {
                query.next();
            }
            if matched != matched_run && !run.is_empty() {
                let advance = label.measure(&run);
                self.runs.push((
                    x,
                    std::mem::take(&mut run),
                    if matched_run {
                        label.accent
                    } else {
                        label.foreground
                    },
                ));
                x += advance;
            }
            matched_run = matched;
            run.push(ch);
        }
        if !run.is_empty() {
            self.runs.push((
                x,
                run,
                if matched_run {
                    label.accent
                } else {
                    label.foreground
                },
            ));
        }
        if detail_width > 0.0 {
            let detail = label.fit(&label.detail, detail_width);
            self.runs.push((
                width - label.measure(&detail),
                detail,
                label.foreground.with_opacity(0.78),
            ));
        }
        self.size
    }
    fn update_needs_layout(&self) -> bool {
        true
    }
    fn size(&self) -> Size {
        self.size
    }
    fn hit_test(&self, p: Point) -> bool {
        Rect::new(Point::ZERO, self.size).contains(p)
    }
    fn render(&mut self) {}
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn paint<'a>(&'a self, ctx: &mut PaintContext<'a>, origin: Point) -> bool {
        ctx.push_clip(Rect::new(origin, self.size));
        for (x, text, color) in &self.runs {
            let point = Point::new(origin.x + x, origin.y + 5.0);
            if let Some(font) = &self.label.font {
                ctx.draw_text_with_font_stack(
                    point,
                    text.clone(),
                    *color,
                    self.label.font_size,
                    font.clone(),
                );
            } else {
                ctx.draw_text(point, text.clone(), *color, self.label.font_size);
            }
        }
        ctx.pop_clip();
        true
    }
}
