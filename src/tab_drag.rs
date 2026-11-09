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
    EditorArea,
    Panel,
    Session(u64),
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
    Outer(Edge),
}
#[derive(Clone, Copy)]
struct Gesture {
    group: usize,
    id: DocumentId,
    start: Point,
    dragging: bool,
    target: Option<Destination>,
}
#[derive(Clone, Copy)]
struct PanelGesture {
    start: Point,
    target: Option<terminal_ui::PanelPosition>,
    session: u64,
    dragging: bool,
}
#[derive(Default)]
struct Model {
    geometry: BTreeMap<RegionId, Rect>,
    gesture: Option<Gesture>,
    press: Option<Point>,
    panel_drag: Option<PanelGesture>,
}
#[derive(Clone, Default)]
pub(super) struct DragState(Arc<Mutex<Model>>);

impl App {
    pub(super) fn remember_panel_size(&self) {
        if !self.panel_visible.get() {
            return;
        }
        let geometry = self.tab_drag.0.lock().unwrap().geometry.clone();
        let Some(panel) = geometry.get(&RegionId::Panel) else {
            return;
        };
        let side = self.panel_position.get() == terminal_ui::PanelPosition::Right;
        let panes = self
            .pane_order()
            .iter()
            .filter_map(|g| geometry.get(&RegionId::Group(*g)))
            .collect::<Vec<_>>();
        let start = panes
            .iter()
            .map(|r| if side { r.origin.x } else { r.origin.y })
            .fold(f32::INFINITY, f32::min);
        let end = panes
            .iter()
            .map(|r| {
                if side {
                    r.origin.x + r.size.width
                } else {
                    r.origin.y + r.size.height
                }
            })
            .fold(0.0, f32::max);
        let extent = if side {
            panel.size.width
        } else {
            panel.size.height
        };
        if end > start && extent > 0.0 {
            self.panel_fractions.update(|ratios| {
                ratios[usize::from(side)] = (end - start) / (end - start + extent)
            });
        }
    }
    pub(super) fn remember_pane_sizes(&self) {
        fn measure(
            layout: &mut groups::PaneLayout,
            geometry: &BTreeMap<RegionId, Rect>,
        ) -> Option<Rect> {
            match layout {
                groups::PaneLayout::Leaf(group) => geometry.get(&RegionId::Group(*group)).copied(),
                groups::PaneLayout::Split {
                    down,
                    fraction,
                    first,
                    second,
                    ..
                } => match (measure(first, geometry), measure(second, geometry)) {
                    (Some(a), Some(b)) => {
                        let (start, end) = if *down {
                            (a.size.height, b.size.height)
                        } else {
                            (a.size.width, b.size.width)
                        };
                        if start + end > 0.0 {
                            *fraction = start / (start + end);
                        }
                        let x = a.origin.x.min(b.origin.x);
                        let y = a.origin.y.min(b.origin.y);
                        Some(Rect::from_xywh(
                            x,
                            y,
                            (a.origin.x + a.size.width).max(b.origin.x + b.size.width) - x,
                            (a.origin.y + a.size.height).max(b.origin.y + b.size.height) - y,
                        ))
                    }
                    (one, other) => one.or(other),
                },
            }
        }
        let mut geometry = self.tab_drag.0.lock().unwrap().geometry.clone();
        geometry.retain(|id, _| match id {
            RegionId::Group(g) => self.group_exists(*g),
            _ => true,
        });
        let mut layout = self.pane_layout.get();
        measure(&mut layout, &geometry);
        if layout != self.pane_layout.get() {
            self.pane_layout.set(layout);
        }
    }
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
                    RegionId::Tab(group, id) => {
                        tabs.get(group).is_some_and(|tabs| tabs.contains(&id))
                    }
                    RegionId::Group(group) => self.group_exists(group),
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
        let panel_drag = self.tab_drag.0.lock().unwrap().panel_drag;
        if let Some(mut drag) = panel_drag {
            let start = drag.start;
            let previous = drag.target;
            match event {
                Event::Mouse(MouseEvent::Moved { x, y }) => {
                    let p = point(*x, *y);
                    let target = if (p.x - start.x).hypot(p.y - start.y) < 6.0 || !root.contains(p)
                    {
                        None
                    } else if p.x > root.size.width * 0.80 {
                        Some(terminal_ui::PanelPosition::Right)
                    } else if p.y > root.size.height * 0.70 {
                        Some(terminal_ui::PanelPosition::Bottom)
                    } else {
                        None
                    };
                    drag.dragging |= (p.x - start.x).hypot(p.y - start.y) >= 6.0;
                    drag.target = target;
                    self.tab_drag.0.lock().unwrap().panel_drag = Some(drag);
                    if previous != target {
                        self.changed();
                    }
                    return true;
                }
                Event::Mouse(MouseEvent::ButtonReleased {
                    button: MouseButton::Left,
                    x,
                    y,
                    ..
                }) => {
                    self.tab_drag.0.lock().unwrap().panel_drag = None;
                    if !root.contains(point(*x, *y)) {
                        self.changed();
                        return true;
                    }
                    if !drag.dragging || previous.is_some() {
                        self.terminal.select(drag.session);
                    }
                    if let Some(position) = previous {
                        self.remember_panel_size();
                        self.panel_position.set(position);
                    }
                    self.changed();
                    return true;
                }
                Event::Mouse(MouseEvent::ButtonCancelled { .. })
                | Event::Keyboard(KeyEvent::Pressed {
                    keycode: KeyCode::Escape,
                    ..
                }) => {
                    self.tab_drag.0.lock().unwrap().panel_drag = None;
                    self.changed();
                    return true;
                }
                _ => {}
            }
        }
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
                    drag.target = destination(&model.geometry, &tabs, &self.pane_order(), p);
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
                        &self.pane_order(),
                        p,
                    );
                    match target {
                        Some(Destination::Insert(group, index)) => {
                            self.move_tab(drag.id, drag.group, group, index)
                        }
                        Some(Destination::Split(target, edge)) => {
                            self.split_tab_beside(drag.id, drag.group, target, edge)
                        }
                        Some(Destination::Outer(edge)) => {
                            self.split_tab_outer(drag.id, drag.group, edge)
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

    pub(super) fn move_tab(&self, id: DocumentId, source: usize, target: usize, index: usize) {
        self.move_tab_internal(id, source, target, index, true);
    }
    pub(super) fn move_tab_internal(
        &self,
        id: DocumentId,
        source: usize,
        target: usize,
        mut index: usize,
        remember: bool,
    ) {
        if self.blocked() || !self.group_exists(target) || !self.group_exists(source) {
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
        if source != target && remember {
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
            self.retire_empty_group(source);
        }
        self.dismiss_language_panel();
        self.changed();
    }

    #[cfg(test)]
    pub(super) fn split_tab(&self, id: DocumentId, source: usize, edge: Edge) {
        self.split_tab_beside(id, source, source, edge);
    }
    pub(super) fn split_tab_beside(
        &self,
        id: DocumentId,
        source: usize,
        beside: usize,
        edge: Edge,
    ) {
        self.split_tab_at(id, source, Some(beside), edge);
    }
    pub(super) fn split_tab_outer(&self, id: DocumentId, source: usize, edge: Edge) {
        self.split_tab_at(id, source, None, edge);
    }
    fn split_tab_at(&self, id: DocumentId, source: usize, beside: Option<usize>, edge: Edge) {
        if self.blocked()
            || !self.group_exists(source)
            || beside.is_some_and(|g| !self.group_exists(g))
        {
            return;
        }
        if !self.runtime.lock().unwrap().group_tabs[source].contains(&id) {
            return;
        }
        self.remember_location_in(source);
        let from = self.bindings_for(id, source).unwrap();
        let detach = beside.map_or(self.pane_order().len() > 1, |target| target != source);
        let group = if let Some(beside) = beside {
            self.add_group(id, beside, edge)
        } else {
            self.remember_pane_sizes();
            let group = self.allocate_group(id);
            self.pane_layout.update(|layout| layout.wrap(group, edge));
            group
        };
        let to = self.bindings_for(id, group).unwrap();
        to.selection.set(from.selection.get());
        to.scroll.set(from.scroll.get());
        let next = {
            let mut rt = self.runtime.lock().unwrap();
            if rt.group_tabs[source].len() > 1 || detach {
                rt.group_tabs[source].retain(|tab| *tab != id);
            }
            if let Some(zoom) = rt.image_zoom.get(&(id, source)).copied() {
                rt.image_zoom.insert((id, group), zoom);
            }
            if rt.group_tabs[source].contains(&self.group_id(source)) {
                Some(self.group_id(source))
            } else {
                rt.group_tabs[source].first().copied()
            }
        };
        if let Some(next) = next {
            self.set_group_id(source, next);
        }
        self.focus_group(group);
        if next.is_none() {
            self.retire_empty_group(source);
        }
        self.dismiss_language_panel();
        self.changed();
    }
}

fn destination(
    geometry: &BTreeMap<RegionId, Rect>,
    tabs: &[Vec<DocumentId>],
    order: &[usize],
    p: Point,
) -> Option<Destination> {
    // The outer band targets the whole editor tree, excluding sidebar/panel.
    // Further inside, the same edge targets only the pane under the pointer.
    if order.len() > 1 {
        if let Some(area) = geometry
            .get(&RegionId::EditorArea)
            .filter(|area| area.contains(p))
        {
            let edges = [
                (p.x - area.origin.x, Edge::Left),
                (area.origin.x + area.size.width - p.x, Edge::Right),
                (p.y - area.origin.y, Edge::Top),
                (area.origin.y + area.size.height - p.y, Edge::Bottom),
            ];
            if let Some((distance, edge)) = edges.into_iter().min_by(|a, b| a.0.total_cmp(&b.0)) {
                if distance < 16.0 {
                    return Some(Destination::Outer(edge));
                }
            }
        }
    }
    for &group in order {
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
        {
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
        if let RegionId::Session(session) = self.id {
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
                        model.panel_drag = Some(PanelGesture {
                            start,
                            target: None,
                            session,
                            dragging: false,
                        });
                        return true;
                    }
                }
            }
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
        if self.id == RegionId::Root {
            if let Some(PanelGesture {
                target: Some(position),
                ..
            }) = self.state.0.lock().unwrap().panel_drag
            {
                let rect = match position {
                    terminal_ui::PanelPosition::Right => Rect::from_xywh(
                        origin.x + self.size.width * 0.8,
                        origin.y,
                        self.size.width * 0.2,
                        self.size.height,
                    ),
                    terminal_ui::PanelPosition::Bottom => Rect::from_xywh(
                        origin.x,
                        origin.y + self.size.height * 0.7,
                        self.size.width,
                        self.size.height * 0.3,
                    ),
                };
                ctx.fill_rect(rect, self.accent.with_opacity(0.16));
                return true;
            }
        }
        if self.id == RegionId::EditorArea {
            let model = self.state.0.lock().unwrap();
            if let Some(Gesture {
                dragging: true,
                target: Some(Destination::Outer(edge)),
                ..
            }) = model.gesture
            {
                let mut rect = Rect::new(origin, self.size);
                match edge {
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
                }
                ctx.fill_rect(rect, self.accent.with_opacity(0.20));
                ctx.draw_text(
                    Point::new(rect.origin.x + 16.0, rect.origin.y + 32.0),
                    "Entire editor area",
                    self.accent,
                    14.0,
                );
                return true;
            }
        }
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
            "Split pane",
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
        assert_eq!(app.pane_order(), vec![1, 0]);
        assert_eq!(
            app.runtime.lock().unwrap().group_tabs,
            [vec![first], vec![second]]
        );
        assert_eq!(app.bindings(second).unwrap().selection.get(), selection);
        app.move_tab(second, 1, 0, 0);
        assert!(app.panes.get().get(1).copied().flatten().is_none());
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
        app.move_tab(first, 0, 2, usize::MAX);
        assert_eq!(app.pane_order(), vec![2]);
        assert_eq!(app.focused_group.get(), 2);
        assert_eq!(app.active_id(), first);
        assert_eq!(
            app.runtime.lock().unwrap().group_tabs[2],
            vec![second, first]
        );
    }

    #[test]
    fn outer_edge_drop_spans_entire_editor_and_keeps_existing_stack_on_every_side() {
        for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            let app = App::new();
            app.split_editor(true);
            app.new_document();
            let id = app.active_id();
            let before = app.pane_layout.get();
            let selection = TextSelection::collapsed(0);
            app.bindings(id).unwrap().selection.set(selection);
            let window = || {
                Window::new("Outer", app.content())
                    .decorated(false)
                    .size(Size::new(1200.0, 800.0))
            };
            let mut pipeline = RenderingPipeline::new();
            pipeline.set_paint_enabled(false);
            pipeline.set_root(window().create_element());
            pipeline.layout_initial();
            pipeline.render();
            let mut geometry = BTreeMap::new();
            collect_geometry(
                pipeline.element_tree().root().unwrap(),
                Point::ZERO,
                &mut geometry,
            );
            let area = geometry[&RegionId::EditorArea];
            let point = match edge {
                Edge::Left => {
                    Point::new(area.origin.x + 5.0, area.origin.y + area.size.height * 0.75)
                }
                Edge::Right => Point::new(
                    area.origin.x + area.size.width - 5.0,
                    area.origin.y + area.size.height * 0.75,
                ),
                Edge::Top => Point::new(area.origin.x + area.size.width * 0.5, area.origin.y + 5.0),
                Edge::Bottom => Point::new(
                    area.origin.x + area.size.width * 0.5,
                    area.origin.y + area.size.height - 5.0,
                ),
            };
            assert_eq!(
                destination(
                    &geometry,
                    &app.runtime.lock().unwrap().group_tabs,
                    &app.pane_order(),
                    point
                ),
                Some(Destination::Outer(edge))
            );
            let pane = geometry[&RegionId::Group(1)];
            let inner = Point::new(
                pane.origin.x + pane.size.width - 30.0,
                pane.origin.y + pane.size.height * 0.5,
            );
            assert_eq!(
                destination(
                    &geometry,
                    &app.runtime.lock().unwrap().group_tabs,
                    &app.pane_order(),
                    inner
                ),
                Some(Destination::Split(1, Edge::Right))
            );
            let tab = geometry[&RegionId::Tab(1, id)];
            pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonPressed {
                button: MouseButton::Left,
                x: (tab.origin.x + 25.0) as i32,
                y: (tab.origin.y + 15.0) as i32,
                click_count: 1,
            }));
            pipeline.handle_event(&Event::Mouse(MouseEvent::Moved {
                x: point.x as i32,
                y: point.y as i32,
            }));
            pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonReleased {
                button: MouseButton::Left,
                x: point.x as i32,
                y: point.y as i32,
                click_count: 1,
            }));
            assert_eq!(app.active_id(), id);
            let groups::PaneLayout::Split {
                first,
                second,
                down,
                ..
            } = app.pane_layout.get()
            else {
                panic!("outer split missing");
            };
            assert_eq!(down, matches!(edge, Edge::Top | Edge::Bottom));
            let old = if matches!(edge, Edge::Left | Edge::Top) {
                second
            } else {
                first
            };
            assert_eq!(*old, before);
            pipeline
                .element_tree_mut()
                .root_mut()
                .unwrap()
                .update(&window());
            pipeline.render();
            geometry.clear();
            collect_geometry(
                pipeline.element_tree().root().unwrap(),
                Point::ZERO,
                &mut geometry,
            );
            let outer = geometry[&RegionId::Group(2)];
            if down {
                assert!((outer.size.width - area.size.width).abs() < 1.0);
            } else {
                assert!((outer.size.height - area.size.height).abs() < 1.0);
            }
            assert_eq!(app.bindings(id).unwrap().selection.get(), selection);
            pipeline.teardown();
        }
    }

    #[test]
    fn panel_drag_docks_independently_of_nested_editors_and_toggle_preserves_sessions() {
        let app = App::new();
        app.split_editor(true);
        app.focus_group(0);
        app.split_editor(false);
        let layout = app.pane_layout.get();
        app.new_terminal();
        let session = app.terminal.active().unwrap();
        let generation = session.terminal.generation();
        let window = || {
            Window::new("Panel", app.content())
                .decorated(false)
                .size(Size::new(1100.0, 760.0))
        };
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_paint_enabled(false);
        pipeline.set_root(window().create_element());
        pipeline.layout_initial();
        pipeline.render();
        let mut geometry = BTreeMap::new();
        collect_geometry(
            pipeline.element_tree().root().unwrap(),
            Point::ZERO,
            &mut geometry,
        );
        let tab = geometry[&RegionId::Session(session.id)];
        pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonPressed {
            button: MouseButton::Left,
            x: (tab.origin.x + 25.0) as i32,
            y: (tab.origin.y + 15.0) as i32,
            click_count: 1,
        }));
        assert!(app.tab_drag.0.lock().unwrap().panel_drag.is_some());
        pipeline.handle_event(&Event::Mouse(MouseEvent::Moved { x: 1050, y: 350 }));
        pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonReleased {
            button: MouseButton::Left,
            x: 1050,
            y: 350,
            click_count: 1,
        }));
        assert_eq!(app.panel_position.get(), terminal_ui::PanelPosition::Right);
        assert_eq!(app.pane_layout.get(), layout);
        pipeline
            .element_tree_mut()
            .root_mut()
            .unwrap()
            .update(&window());
        pipeline.render();
        let mut geometry = BTreeMap::new();
        collect_geometry(
            pipeline.element_tree().root().unwrap(),
            Point::ZERO,
            &mut geometry,
        );
        assert!(geometry[&RegionId::Panel].origin.x > geometry[&RegionId::Group(2)].origin.x);
        app.focus_group(0);
        app.toggle_panel();
        assert!(!app.panel_visible.get());
        app.toggle_panel();
        assert!(app.panel_visible.get());
        assert_eq!(app.terminal.active().unwrap().id, session.id);
        assert_eq!(session.terminal.generation(), generation);
        assert_eq!(app.pane_layout.get(), layout);
        session.terminal.close();
        pipeline.teardown();
    }

    #[test]
    fn nested_splits_keep_bottom_full_width_and_can_become_three_columns() {
        let app = App::new();
        app.split_editor(true); // bottom 1
        app.focus_group(0);
        app.split_editor(false); // upper right 2
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_paint_enabled(false);
        for size in [Size::new(1400.0, 900.0), Size::new(720.0, 400.0)] {
            pipeline.set_root(
                Window::new("Nested", app.content())
                    .decorated(false)
                    .size(size)
                    .create_element(),
            );
            pipeline.layout_initial();
            pipeline.render();
            let mut geometry = BTreeMap::new();
            collect_geometry(
                pipeline.element_tree().root().unwrap(),
                Point::ZERO,
                &mut geometry,
            );
            let left = geometry[&RegionId::Group(0)];
            let right = geometry[&RegionId::Group(2)];
            let bottom = geometry[&RegionId::Group(1)];
            assert!((left.origin.y - right.origin.y).abs() < 1.0);
            assert!(right.origin.x >= left.origin.x + left.size.width);
            assert!(bottom.origin.y >= left.origin.y + left.size.height);
            assert!((bottom.origin.x - left.origin.x).abs() < 1.0);
            assert!((bottom.size.width - left.size.width - right.size.width - 10.0).abs() < 1.0);
            for pane in [left, right, bottom] {
                assert!(pane.size.width > 0.0 && pane.size.height > 0.0);
                assert!(pane.origin.x + pane.size.width <= size.width + 1.0);
                assert!(pane.origin.y + pane.size.height <= size.height + 1.0);
            }
        }
        pipeline.teardown();
        // A unique tab dragged out of the bottom pane removes that old pane.
        app.focus_group(1);
        app.new_document();
        let moved = app.active_id();
        let shared = app.group_id(0);
        app.request_close(Some(shared));
        app.split_tab_beside(moved, 1, 2, Edge::Right);
        assert_eq!(app.pane_order(), vec![0, 2, 3]);
        assert!(!app.group_exists(1));
        assert!(matches!(
            app.pane_layout.get(),
            groups::PaneLayout::Split { down: false, .. }
        ));
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
        assert_eq!(app.panes.get().get(1).copied().flatten(), Some(second));
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
        let drop = Point::new(
            group.origin.x + group.size.width / 2.0,
            group.origin.y + 160.0,
        );
        pipeline.handle_event(&Event::Mouse(MouseEvent::Moved {
            x: drop.x as i32,
            y: drop.y as i32,
        }));
        assert!(pipeline.handle_event(&mouse(false, drop)));
        assert!(app.panes.get().get(1).copied().flatten().is_none());
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
