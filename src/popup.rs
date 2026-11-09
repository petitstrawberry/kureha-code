//! An editor popover occupies no document space and only receives input inside its panel.
use scarlet_ui::element::{
    Element, ElementRenderObject, LayoutConstraints, RenderElement, UpdateResult,
};
use scarlet_ui::prelude::*;
use std::any::Any;
#[derive(Clone)]
pub struct Anchored<V: View + Clone> {
    content: V,
    anchor: Option<State<Point>>,
    centered: bool,
    max_size: Size,
}
impl<V: View + Clone> Anchored<V> {
    pub fn new(content: V, anchor: State<Point>) -> Self {
        Self {
            content,
            anchor: Some(anchor),
            centered: false,
            max_size: Size::new(440.0, 260.0),
        }
    }
    pub fn top_right(content: V) -> Self {
        Self {
            content,
            anchor: None,
            centered: false,
            max_size: Size::new(440.0, 260.0),
        }
    }
    pub fn top_center(content: V) -> Self {
        Self {
            content,
            anchor: None,
            centered: true,
            max_size: Size::new(620.0, 380.0),
        }
    }
    pub fn max_size(mut self, size: Size) -> Self {
        self.max_size = size;
        self
    }
}
impl<V: View + Clone + 'static> View for Anchored<V> {
    fn create_element(&self) -> Box<dyn Element> {
        Box::new(RenderElement::with_view_children_and_updater(
            self.clone(),
            |v| Popover {
                anchor: v.anchor.as_ref().map(State::get),
                centered: v.centered,
                max_size: v.max_size,
                size: Size::ZERO,
                panel: Rect::zero(),
            },
            |r, v| {
                r.anchor = v.anchor.as_ref().map(State::get);
                r.centered = v.centered;
                r.max_size = v.max_size;
                UpdateResult::Updated
            },
            |v| vec![v.content.clone_view()],
        ))
    }
    fn listenables(&self) -> Vec<&dyn Listenable> {
        let mut v: Vec<&dyn Listenable> =
            self.anchor.iter().map(|s| s as &dyn Listenable).collect();
        v.extend(self.content.listenables());
        v
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
struct Popover {
    anchor: Option<Point>,
    centered: bool,
    max_size: Size,
    size: Size,
    panel: Rect,
}
impl ElementRenderObject for Popover {
    fn update_needs_layout(&self) -> bool {
        true
    }
    fn layout(&mut self, c: LayoutConstraints) -> Size {
        self.size = Size::new(c.max_width, c.max_height);
        self.size
    }
    fn layout_with_children(
        &mut self,
        c: LayoutConstraints,
        children: &mut [Box<dyn Element>],
    ) -> Size {
        self.layout(c);
        if let Some(child) = children.first_mut() {
            let size = child.layout(LayoutConstraints::new(
                0.0,
                c.max_width.min(self.max_size.width),
                0.0,
                c.max_height.min(self.max_size.height),
            ));
            self.panel = match self.anchor {
                Some(anchor) => place(anchor, self.size, size),
                None => Rect::new(
                    Point::new(
                        (if self.centered {
                            (self.size.width - size.width) / 2.0
                        } else {
                            self.size.width - size.width - 8.0
                        })
                        .max(0.0),
                        8.0_f32.min((self.size.height - size.height).max(0.0)),
                    ),
                    size,
                ),
            };
            child.set_position(self.panel.origin);
        }
        self.size
    }
    fn size(&self) -> Size {
        self.size
    }
    fn hit_test(&self, p: Point) -> bool {
        self.panel.contains(p)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn render(&mut self) {}
}
fn place(anchor: Point, viewport: Size, panel: Size) -> Rect {
    let x = anchor.x.clamp(0.0, (viewport.width - panel.width).max(0.0));
    let y = if anchor.y + panel.height > viewport.height {
        anchor.y - panel.height - 22.0
    } else {
        anchor.y
    };
    Rect::new(
        Point::new(x, y.clamp(0.0, (viewport.height - panel.height).max(0.0))),
        panel,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn popovers_stay_inside_the_editor_and_flip_above_its_bottom() {
        let r = place(
            Point::new(790.0, 590.0),
            Size::new(800.0, 600.0),
            Size::new(400.0, 200.0),
        );
        assert_eq!(r.origin.x, 400.0);
        assert!(r.origin.y < 590.0);
        assert!(r.origin.x + r.size.width <= 800.0 && r.origin.y + r.size.height <= 600.0);
    }
}
