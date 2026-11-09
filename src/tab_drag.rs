//! Workbench tab gestures retain their identity across rebuilds. Hit regions use
//! layout coordinates, independent of CPU/GPU paint origins and cached layers.
use super::*;
use scarlet_ui::{
    element::{ElementRenderObject, LayoutConstraints, RenderElement, UpdateResult},
    event::{MouseButton, MouseEvent},
    renderer::PaintContext,
};
use std::any::Any;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum RegionId {
    Root,
    Group(usize),
    Tab(usize, DocumentId),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum Destination {
    Insert(usize, usize),
    Split(usize, Edge),
}
#[derive(Clone, Copy)]
struct Gesture {
    group: usize,
    id: DocumentId,
    start: Point,
    dragging: bool,
    target: Option<Destination>,
}
#[derive(Default)]
struct Model {
    geometry: BTreeMap<RegionId, Rect>,
    gesture: Option<Gesture>,
    press: Option<Point>,
}
#[derive(Clone, Default)]
pub(super) struct DragState(Arc<Mutex<Model>>);

impl App {
    pub(super) fn tab_region<V: View + Clone>(
        &self,
        child: V,
        id: RegionId,
    ) -> impl View + Clone + use<V> {
        if id == RegionId::Root {
            let tabs = self.runtime.lock().unwrap().group_tabs.clone();
            self.tab_drag
                .0
                .lock()
                .unwrap()
                .geometry
                .retain(|key, _| match *key {
                    RegionId::Tab(group, id) => tabs[group].contains(&id),
                    RegionId::Group(1) => self.second.get().is_some(),
                    _ => true,
                });
        }
        Region {
            child,
            id,
            state: self.tab_drag.clone(),
            accent: self.theme.get().colors().accent,
        }
    }
    pub(super) fn tab_drag_event(&self, event: &Event) -> bool {
        let root = self
            .tab_drag
            .0
            .lock()
            .unwrap()
            .geometry
            .get(&RegionId::Root)
            .copied()
            .unwrap_or(Rect::zero());
        let point = |x, y| Point::new(x as f32 + root.origin.x, y as f32 + root.origin.y);
        match event {
            Event::Mouse(MouseEvent::ButtonPressed {
                button: MouseButton::Left,
                x,
                y,
                ..
            }) => {
                let allowed = !self.blocked()
                    && !self.nav.is_open()
                    && self.runtime.lock().unwrap().job.is_none();
                self.tab_drag.0.lock().unwrap().press = allowed.then(|| point(*x, *y));
                false
            }
            Event::Mouse(MouseEvent::Moved { x, y }) => {
                let p = point(*x, *y);
                let tabs = self.runtime.lock().unwrap().group_tabs.clone();
                let mut model = self.tab_drag.0.lock().unwrap();
                let Some(mut drag) = model.gesture else {
                    return false;
                };
                drag.dragging |= (p.x - drag.start.x).hypot(p.y - drag.start.y) >= 6.0;
                if drag.dragging {
                    drag.target =
                        destination(&model.geometry, &tabs, self.second.get().is_some(), p);
                }
                let changed = model.gesture.is_none_or(|previous| {
                    previous.dragging != drag.dragging || previous.target != drag.target
                });
                model.gesture = Some(drag);
                drop(model);
                if changed {
                    self.changed();
                }
                true
            }
            Event::Mouse(MouseEvent::ButtonReleased {
                button: MouseButton::Left,
                x,
                y,
                ..
            }) => {
                let p = point(*x, *y);
                let drag = self.tab_drag.0.lock().unwrap().gesture.take();
                let Some(drag) = drag else {
                    return false;
                };
                if drag.dragging {
                    let tabs = self.runtime.lock().unwrap().group_tabs.clone();
                    let target = destination(
                        &self.tab_drag.0.lock().unwrap().geometry,
                        &tabs,
                        self.second.get().is_some(),
                        p,
                    );
                    match target {
                        Some(Destination::Insert(group, index)) => {
                            self.move_tab(drag.id, drag.group, group, index)
                        }
                        Some(Destination::Split(_, edge)) => {
                            self.split_tab(drag.id, drag.group, edge)
                        }
                        None => {}
                    }
                } else {
                    self.focus_group(drag.group);
                    self.activate(drag.id);
                }
                self.changed();
                true
            }
            Event::Mouse(MouseEvent::ButtonCancelled {
                button: MouseButton::Left,
                ..
            })
            | Event::Keyboard(KeyEvent::Pressed {
                keycode: KeyCode::Escape,
                ..
            }) => {
                let had = self.tab_drag.0.lock().unwrap().gesture.take().is_some();
                if had {
                    self.changed();
                }
                had
            }
            _ => false,
        }
    }

    pub(super) fn move_tab(&self, id: DocumentId, source: usize, target: usize, mut index: usize) {
        if self.blocked() || target > 1 || source > 1 || target == 1 && self.second.get().is_none()
        {
            return;
        }
        let source_active = self.group_id(source);
        let view = self.bindings_for(id, source);
        {
            let rt = self.runtime.lock().unwrap();
            if !rt.group_tabs[source].contains(&id) {
                return;
            }
        }
        if source != target {
            self.remember_location_in(source);
        }
        let (next, empty) = {
            let mut rt = self.runtime.lock().unwrap();
            let old = rt.group_tabs[source]
                .iter()
                .position(|tab| *tab == id)
                .unwrap();
            if source == target && old < index {
                index = index.saturating_sub(1);
            }
            if source != target
                && rt.group_tabs[target]
                    .iter()
                    .position(|tab| *tab == id)
                    .is_some_and(|old| old < index)
            {
                index = index.saturating_sub(1);
            }
            rt.group_tabs[source].retain(|tab| *tab != id);
            let next = rt.group_tabs[source]
                .get(old.min(rt.group_tabs[source].len().saturating_sub(1)))
                .copied();
            rt.group_tabs[target].retain(|tab| *tab != id);
            index = index.min(rt.group_tabs[target].len());
            rt.group_tabs[target].insert(index, id);
            if source != target {
                if let Some(zoom) = rt.image_zoom.remove(&(id, source)) {
                    rt.image_zoom.insert((id, target), zoom);
                }
            }
            (next, rt.group_tabs[source].is_empty())
        };
        if source != target {
            if let (Some(from), Some(to)) = (view, self.bindings_for(id, target)) {
                to.selection.set(from.selection.get());
                to.scroll.set(from.scroll.get());
            }
            if source_active == id {
                if let Some(next) = next {
                    self.focus_group(source);
                    self.activate_unchecked(next);
                }
            }
        }
        self.focus_group(target);
        self.activate_unchecked(id);
        if empty && source != target {
            if source == 1 {
                self.close_split();
            } else {
                self.promote_second_group(id);
            }
        }
        self.dismiss_language_panel();
        self.changed();
    }

    fn promote_second_group(&self, active: DocumentId) {
        let index = {
            let mut rt = self.runtime.lock().unwrap();
            rt.group_tabs[0] = std::mem::take(&mut rt.group_tabs[1]);
            let ids = rt.group_tabs[0].clone();
            for id in ids {
                if let Some(bindings) = rt.second_bindings.remove(&id) {
                    rt.workspace.get_mut(id).unwrap().bindings = bindings;
                }
                if let Some(zoom) = rt.image_zoom.remove(&(id, 1)) {
                    rt.image_zoom.insert((id, 0), zoom);
                }
            }
            rt.workspace.index(active).unwrap()
        };
        self.active.set(index);
        self.focus_group(0);
        self.second.set(None);
        self.split_reversed.set(false);
    }

    pub(super) fn split_tab(&self, id: DocumentId, source: usize, edge: Edge) {
        if self.blocked() {
            return;
        }
        if self.second.get().is_some() {
            self.move_tab(id, source, 1 - source, usize::MAX);
            return;
        }
        if !self.runtime.lock().unwrap().group_tabs[0].contains(&id) {
            return;
        }
        self.remember_location_in(0);
        if let (Some(from), Some(to)) = (self.bindings_for(id, 0), self.bindings_for(id, 1)) {
            to.selection.set(from.selection.get());
            to.scroll.set(from.scroll.get());
        }
        let first = {
            let mut rt = self.runtime.lock().unwrap();
            let active = rt.workspace.documents[self.active.get()].id;
            if rt.group_tabs[0].len() > 1 {
                rt.group_tabs[0].retain(|tab| *tab != id);
            }
            rt.group_tabs[1] = vec![id];
            if let Some(zoom) = rt.image_zoom.get(&(id, 0)).copied() {
                rt.image_zoom.insert((id, 1), zoom);
            }
            if rt.group_tabs[0].contains(&active) {
                active
            } else {
                rt.group_tabs[0][0]
            }
        };
        self.focus_group(0);
        self.activate_unchecked(first);
        self.second.set(Some(id));
        self.split_down
            .set(matches!(edge, Edge::Top | Edge::Bottom));
        self.split_reversed
            .set(matches!(edge, Edge::Left | Edge::Top));
        self.focus_group(1);
        self.dismiss_language_panel();
        self.changed();
    }
}

fn destination(
    geometry: &BTreeMap<RegionId, Rect>,
    tabs: &[Vec<DocumentId>; 2],
    split: bool,
    p: Point,
) -> Option<Destination> {
    for group in 0..if split { 2 } else { 1 } {
        let pane = geometry.get(&RegionId::Group(group))?;
        if !pane.contains(p) {
            continue;
        }
        if p.y < pane.origin.y + 50.0 {
            let mut index = tabs[group].len();
            for (i, id) in tabs[group].iter().enumerate() {
                if let Some(rect) = geometry.get(&RegionId::Tab(group, *id)) {
                    if p.x < rect.origin.x + rect.size.width / 2.0 {
                        index = i;
                        break;
                    }
                }
            }
            return Some(Destination::Insert(group, index));
        }
        if !split {
            let x = (p.x - pane.origin.x) / pane.size.width;
            let y = (p.y - pane.origin.y) / pane.size.height;
            let edge = if x < 0.22 {
                Some(Edge::Left)
            } else if x > 0.78 {
                Some(Edge::Right)
            } else if y < 0.25 {
                Some(Edge::Top)
            } else if y > 0.75 {
                Some(Edge::Bottom)
            } else {
                None
            };
            if let Some(edge) = edge {
                return Some(Destination::Split(group, edge));
            }
        }
        return Some(Destination::Insert(group, tabs[group].len()));
    }
    None
}

#[derive(Clone)]
struct Region<V> {
    child: V,
    id: RegionId,
    state: DragState,
    accent: Color,
}
impl<V: View + Clone> View for Region<V> {
    fn create_element(&self) -> Box<dyn Element> {
        let element = Box::new(RenderElement::with_view_children_and_updater(
            self.clone(),
            |v| RegionRenderer {
                id: v.id,
                state: v.state.clone(),
                accent: v.accent,
                size: Size::ZERO,
            },
            |r, v| {
                r.id = v.id;
                r.state = v.state.clone();
                r.accent = v.accent;
                UpdateResult::Updated
            },
            |v| vec![v.child.clone_view()],
        ));
        if self.id == RegionId::Root {
            Box::new(DragRoot {
                inner: element,
                state: self.state.clone(),
            })
        } else {
            element
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
// Collect immediately before dispatch, including positions changed by incremental
// layout or tab-strip scrolling. Retained GPU layers may paint at local origins.
struct DragRoot {
    inner: Box<dyn Element>,
    state: DragState,
}
impl DragRoot {
    fn refresh_geometry(&self) {
        let mut geometry = BTreeMap::new();
        geometry.insert(
            RegionId::Root,
            Rect::new(Point::ZERO, self.inner.bounds().size),
        );
        for child in self.inner.children() {
            collect_geometry(child.as_ref(), Point::ZERO, &mut geometry);
        }
        self.state.0.lock().unwrap().geometry = geometry;
    }
}
impl Element for DragRoot {
    fn id(&self) -> scarlet_ui::element::ElementId {
        self.inner.id()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn children(&self) -> &[Box<dyn Element>] {
        self.inner.children()
    }
    fn children_mut(&mut self) -> &mut [Box<dyn Element>] {
        self.inner.children_mut()
    }
    fn update(&mut self, view: &dyn View) -> UpdateResult {
        self.inner.update(view)
    }
    fn rebuild(&mut self) -> UpdateResult {
        self.inner.rebuild()
    }
    fn mount(&mut self, ctx: &scarlet_ui::pipeline::MountContext) {
        self.inner.mount(ctx);
    }
    fn unmount(&mut self) {
        self.inner.unmount();
    }
    fn layout(&mut self, c: LayoutConstraints) -> Size {
        let size = self.inner.layout(c);
        self.refresh_geometry();
        size
    }
    fn last_layout_constraints(&self) -> Option<LayoutConstraints> {
        self.inner.last_layout_constraints()
    }
    fn set_last_layout_constraints(&mut self, c: LayoutConstraints) {
        self.inner.set_last_layout_constraints(c);
    }
    fn set_safe_area_insets(&mut self, insets: EdgeInsets) {
        self.inner.set_safe_area_insets(insets);
    }
    fn position(&self) -> Point {
        self.inner.position()
    }
    fn set_position(&mut self, p: Point) {
        self.inner.set_position(p);
    }
    fn bounds(&self) -> Rect {
        self.inner.bounds()
    }
    fn fill_width(&self) -> bool {
        self.inner.fill_width()
    }
    fn fill_height(&self) -> bool {
        self.inner.fill_height()
    }
    fn flex_factor(&self) -> u32 {
        self.inner.flex_factor()
    }
    fn handle_event(&mut self, event: &Event, phase: scarlet_ui::event::Phase) -> bool {
        if phase == scarlet_ui::event::Phase::Capture && matches!(event, Event::Mouse(_)) {
            self.refresh_geometry();
        }
        self.inner.handle_event(event, phase)
    }
}

struct RegionRenderer {
    id: RegionId,
    state: DragState,
    accent: Color,
    size: Size,
}
fn collect_geometry(element: &dyn Element, parent: Point, geometry: &mut BTreeMap<RegionId, Rect>) {
    let origin = Point::new(
        parent.x + element.position().x,
        parent.y + element.position().y,
    );
    if let Some(region) = element
        .render_object()
        .and_then(|r| r.as_any().downcast_ref::<RegionRenderer>())
    {
        geometry.insert(region.id, Rect::new(origin, element.bounds().size));
    }
    for child in element.children() {
        collect_geometry(child.as_ref(), origin, geometry);
    }
}

impl ElementRenderObject for RegionRenderer {
    fn layout(&mut self, _: LayoutConstraints) -> Size {
        self.size
    }
    fn layout_with_children(
        &mut self,
        c: LayoutConstraints,
        children: &mut [Box<dyn Element>],
    ) -> Size {
        self.size = children[0].layout(c);
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
    fn handle_event(&mut self, event: &Event, phase: scarlet_ui::event::Phase) -> bool {
        if !matches!(
            phase,
            scarlet_ui::event::Phase::Capture | scarlet_ui::event::Phase::Target
        ) {
            return false;
        }
        let RegionId::Tab(group, id) = self.id else {
            return false;
        };
        if let Event::Mouse(MouseEvent::ButtonPressed {
            button: MouseButton::Left,
            x,
            y,
            ..
        }) = event
        {
            if *x >= 0
                && (*x as f32) < self.size.width - 28.0
                && *y >= 0
                && (*y as f32) < self.size.height
            {
                let mut model = self.state.0.lock().unwrap();
                if let Some(start) = model.press.take() {
                    model.gesture = Some(Gesture {
                        group,
                        id,
                        start,
                        dragging: false,
                        target: None,
                    });
                    return true;
                }
            }
        }
        false
    }
    fn paint_overlay(&self, ctx: &mut PaintContext<'_>, origin: Point) -> bool {
        let RegionId::Group(group) = self.id else {
            return false;
        };
        let model = self.state.0.lock().unwrap();
        let Some(drag) = model.gesture.filter(|d| d.dragging) else {
            return false;
        };
        let mut rect = Rect::new(origin, self.size);
        match drag.target {
            Some(Destination::Split(g, edge)) if g == group => match edge {
                Edge::Left => rect.size.width /= 2.0,
                Edge::Right => {
                    rect.size.width /= 2.0;
                    rect.origin.x += rect.size.width;
                }
                Edge::Top => rect.size.height /= 2.0,
                Edge::Bottom => {
                    rect.size.height /= 2.0;
                    rect.origin.y += rect.size.height;
                }
            },
            Some(Destination::Insert(g, index)) if g == group => {
                ctx.fill_rect(rect, self.accent.with_opacity(0.08));
                let mut tabs: Vec<_> = model
                    .geometry
                    .iter()
                    .filter_map(|(key, rect)| match key {
                        RegionId::Tab(g, _) if *g == group => Some(*rect),
                        _ => None,
                    })
                    .collect();
                tabs.sort_by(|a, b| a.origin.x.total_cmp(&b.origin.x));
                let x = tabs
                    .get(index)
                    .map(|r| r.origin.x)
                    .or_else(|| tabs.last().map(|r| r.origin.x + r.size.width))
                    .map(|x| x - model.geometry[&self.id].origin.x + origin.x)
                    .unwrap_or(origin.x + 8.0);
                ctx.push_clip(rect);
                ctx.fill_rect(
                    Rect::from_xywh(x - 2.0, origin.y + 10.0, 3.0, 32.0),
                    self.accent,
                );
                ctx.pop_clip();
                return true;
            }
            _ => return false,
        }
        ctx.fill_rect(rect, self.accent.with_opacity(0.20));
        ctx.draw_text(
            Point::new(rect.origin.x + 16.0, rect.origin.y + 58.0),
            "Split editor",
            self.accent,
            14.0,
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scarlet_ui::{event::KeyModifiers, pipeline::RenderingPipeline};
    #[test]
    fn moving_dirty_tabs_keeps_document_history_and_collapses_empty_groups() {
        let app = App::new();
        let first = app.active_id();
        app.new_document();
        let second = app.active_id();
        app.edit(
            second,
            &EditDelta {
                replaced_range: 0..0,
                inserted: "猫\ntext".into(),
            },
        );
        let selection = TextSelection::collapsed(3);
        app.bindings(second).unwrap().selection.set(selection);
        app.bindings(second)
            .unwrap()
            .scroll
            .set(TextViewScroll { x: 11.0, y: 20.0 });
        app.split_tab(second, 0, Edge::Left);
        assert!(app.split_reversed.get());
        assert_eq!(
            app.runtime.lock().unwrap().group_tabs,
            [vec![first], vec![second]]
        );
        assert_eq!(app.bindings(second).unwrap().selection.get(), selection);
        app.move_tab(second, 1, 0, 0);
        assert!(app.second.get().is_none());
        assert_eq!(
            app.runtime.lock().unwrap().group_tabs[0],
            vec![second, first]
        );
        assert!(
            app.runtime
                .lock()
                .unwrap()
                .workspace
                .get(second)
                .unwrap()
                .editor
                .dirty()
        );
        app.undo(false);
        assert_eq!(app.bindings(second).unwrap().document.get().as_str(), "");
        app.split_tab(second, 0, Edge::Bottom);
        app.move_tab(first, 0, 1, usize::MAX);
        assert!(app.second.get().is_none());
        assert_eq!(app.focused_group.get(), 0);
        assert_eq!(app.active_id(), first);
        assert_eq!(
            app.runtime.lock().unwrap().group_tabs[0],
            vec![second, first]
        );
    }

    #[test]
    fn mounted_drag_splits_survives_rebuild_and_moves_between_panes() {
        let app = App::new();
        let first = app.active_id();
        app.new_document();
        let second = app.active_id();
        let window = || {
            Window::new("Tab drag", app.content())
                .decorated(false)
                .size(Size::new(1000.0, 600.0))
        };
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_root(window().create_element());
        pipeline.layout_initial();
        pipeline.render();
        let rect = |pipeline: &RenderingPipeline, key| {
            let mut geometry = BTreeMap::new();
            collect_geometry(
                pipeline.element_tree().root().unwrap(),
                Point::ZERO,
                &mut geometry,
            );
            geometry[&key]
        };
        let mouse = |pressed, p: Point| {
            Event::Mouse(if pressed {
                MouseEvent::ButtonPressed {
                    button: MouseButton::Left,
                    x: p.x as i32,
                    y: p.y as i32,
                    click_count: 1,
                }
            } else {
                MouseEvent::ButtonReleased {
                    button: MouseButton::Left,
                    x: p.x as i32,
                    y: p.y as i32,
                    click_count: 1,
                }
            })
        };
        // GPU records may paint each retained layer at a local origin. Painting
        // must never replace layout-space hit regions with those local positions.
        fn paint_locally(element: &dyn Element) {
            if let Some(region) = element
                .render_object()
                .and_then(|r| r.as_any().downcast_ref::<RegionRenderer>())
            {
                region.paint(&mut PaintContext::new(), Point::ZERO);
            }
            for child in element.children() {
                paint_locally(child.as_ref());
            }
        }
        let before = app.tab_drag.0.lock().unwrap().geometry.clone();
        paint_locally(pipeline.element_tree().root().unwrap());
        assert_eq!(app.tab_drag.0.lock().unwrap().geometry, before);
        let tab = rect(&pipeline, RegionId::Tab(0, second));
        let start = Point::new(tab.origin.x + 30.0, tab.origin.y + 15.0);
        assert!(pipeline.handle_event(&mouse(true, start)));
        assert!(app.tab_drag.0.lock().unwrap().gesture.is_some());
        let group = rect(&pipeline, RegionId::Group(0));
        let drop = Point::new(
            group.origin.x + group.size.width - 30.0,
            group.origin.y + 180.0,
        );
        assert!(pipeline.handle_event(&Event::Mouse(MouseEvent::Moved {
            x: drop.x as i32,
            y: drop.y as i32
        })));
        pipeline
            .element_tree_mut()
            .root_mut()
            .unwrap()
            .update(&window());
        pipeline.render();
        assert!(pipeline.handle_event(&mouse(false, drop)));
        assert_eq!(app.second.get(), Some(second));
        assert_eq!(
            app.runtime.lock().unwrap().group_tabs,
            [vec![first], vec![second]]
        );
        pipeline
            .element_tree_mut()
            .root_mut()
            .unwrap()
            .update(&window());
        pipeline.render();
        let tab = rect(&pipeline, RegionId::Tab(1, second));
        assert!(pipeline.handle_event(&mouse(
            true,
            Point::new(tab.origin.x + 30.0, tab.origin.y + 15.0)
        )));
        let group = rect(&pipeline, RegionId::Group(0));
        let drop = Point::new(group.origin.x + 80.0, group.origin.y + 160.0);
        pipeline.handle_event(&Event::Mouse(MouseEvent::Moved {
            x: drop.x as i32,
            y: drop.y as i32,
        }));
        assert!(pipeline.handle_event(&mouse(false, drop)));
        assert!(app.second.get().is_none());
        assert_eq!(
            app.runtime.lock().unwrap().group_tabs[0],
            vec![first, second]
        );
        // Cancelling or releasing outside any editor must leave both tab sets intact.
        pipeline
            .element_tree_mut()
            .root_mut()
            .unwrap()
            .update(&window());
        pipeline.render();
        let before = app.runtime.lock().unwrap().group_tabs.clone();
        let tab = rect(&pipeline, RegionId::Tab(0, second));
        let start = Point::new(tab.origin.x + 30.0, tab.origin.y + 15.0);
        pipeline.handle_event(&mouse(true, start));
        pipeline.handle_event(&Event::Mouse(MouseEvent::Moved { x: 2, y: 200 }));
        pipeline.handle_event(&mouse(false, Point::new(2.0, 200.0)));
        assert_eq!(app.runtime.lock().unwrap().group_tabs, before);
        pipeline.handle_event(&mouse(true, start));
        pipeline.handle_event(&Event::Keyboard(KeyEvent::Pressed {
            keycode: KeyCode::Escape,
            modifiers: KeyModifiers::default(),
        }));
        assert!(app.tab_drag.0.lock().unwrap().gesture.is_none());
        assert_eq!(app.runtime.lock().unwrap().group_tabs, before);
        pipeline.teardown();
    }
}
