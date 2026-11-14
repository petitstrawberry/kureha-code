//! Observe modifier-only input at the actual text-input target. ScarletUI sends
//! ModifiersChanged directly to that target, bypassing ancestor event capture.
//! Delegate the complete Element contract so editing, IME, focus and rendering
//! retain the native TextView behavior.
use scarlet_ui::{element::*, pipeline::MountContext, prelude::*, view::ViewKey};
use std::{any::Any, sync::Arc};

#[derive(Clone)]
pub(super) struct ObservedEditor {
    pub editor: TextView,
    pub observer: Arc<dyn Fn(&Event)>,
}

impl View for ObservedEditor {
    fn create_element(&self) -> Box<dyn Element> {
        Box::new(ObservedElement {
            inner: self.editor.create_element(),
            observer: self.observer.clone(),
        })
    }

    fn listenables(&self) -> Vec<&dyn Listenable> {
        self.editor.listenables()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct ObservedElement {
    inner: Box<dyn Element>,
    observer: Arc<dyn Fn(&Event)>,
}

#[allow(deprecated)] // Forward the legacy hooks too; the app uses paint commands.
impl Element for ObservedElement {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn update(&mut self, view: &dyn View) -> UpdateResult {
        let Some(view) = view.as_any().downcast_ref::<ObservedEditor>() else {
            return UpdateResult::Replaced;
        };
        self.observer = view.observer.clone();
        self.inner.update(&view.editor)
    }

    fn handle_event(&mut self, event: &Event, phase: scarlet_ui::event::Phase) -> bool {
        (self.observer)(event);
        self.inner.handle_event(event, phase)
    }

    fn advance_visual_animation(&mut self, _elapsed: core::time::Duration) -> bool {
        self.inner.advance_visual_animation(_elapsed)
    }

    fn finish_visual_animation(&mut self) -> bool {
        self.inner.finish_visual_animation()
    }

    fn set_safe_area_insets(&mut self, insets: scarlet_ui::geometry::EdgeInsets) {
        self.inner.set_safe_area_insets(insets)
    }

    fn id(&self) -> ElementId {
        self.inner.id()
    }

    fn type_name(&self) -> &str {
        self.inner.type_name()
    }

    fn view_key(&self) -> Option<&ViewKey> {
        self.inner.view_key()
    }

    fn type_name_debug(&self) -> String {
        self.inner.type_name_debug()
    }

    fn retains_virtual_items(&self) -> bool {
        self.inner.retains_virtual_items()
    }

    fn has_scroll_dependencies(&self) -> bool {
        self.inner.has_scroll_dependencies()
    }

    fn scroll_anchor(&self, viewport: Rect) -> Option<ScrollAnchor> {
        self.inner.scroll_anchor(viewport)
    }

    fn resolve_scroll_anchor(&self, key: &ViewKey) -> Option<Point> {
        self.inner.resolve_scroll_anchor(key)
    }

    fn children(&self) -> &[Box<dyn Element>] {
        self.inner.children()
    }

    fn children_mut(&mut self) -> &mut [Box<dyn Element>] {
        self.inner.children_mut()
    }

    fn rebuild(&mut self) -> UpdateResult {
        self.inner.rebuild()
    }

    fn mount(&mut self, _ctx: &MountContext) {
        self.inner.mount(_ctx)
    }

    fn unmount(&mut self) {
        self.inner.unmount()
    }

    fn layout(&mut self, constraints: LayoutConstraints) -> Size {
        self.inner.layout(constraints)
    }

    fn last_layout_constraints(&self) -> Option<LayoutConstraints> {
        self.inner.last_layout_constraints()
    }

    fn set_last_layout_constraints(&mut self, _constraints: LayoutConstraints) {
        self.inner.set_last_layout_constraints(_constraints)
    }

    fn position(&self) -> Point {
        self.inner.position()
    }

    fn set_position(&mut self, _position: Point) {
        self.inner.set_position(_position)
    }

    fn set_viewport_hint(&mut self, _viewport: Rect) -> bool {
        self.inner.set_viewport_hint(_viewport)
    }

    fn bounds(&self) -> Rect {
        self.inner.bounds()
    }

    fn hit_test(&self, point: Point) -> bool {
        self.inner.hit_test(point)
    }

    fn render(&mut self) {
        self.inner.render()
    }

    fn get_buffer(&self) -> Option<&scarlet_ui::buffer::Buffer> {
        self.inner.get_buffer()
    }

    fn render_object(&self) -> Option<&dyn scarlet_ui::element::ElementRenderObject> {
        self.inner.render_object()
    }

    fn render_object_mut(&mut self) -> Option<&mut dyn scarlet_ui::element::ElementRenderObject> {
        self.inner.render_object_mut()
    }

    fn press_repeat_timing(&self) -> Option<(core::time::Duration, core::time::Duration)> {
        self.inner.press_repeat_timing()
    }

    fn invoke_press_repeat(&self) -> bool {
        self.inner.invoke_press_repeat()
    }

    fn take_window_action(&mut self) -> Option<scarlet_ui::event::WindowEvent> {
        self.inner.take_window_action()
    }

    fn flex_factor(&self) -> u32 {
        self.inner.flex_factor()
    }

    fn pointer_drag_needs_animation(&self) -> bool {
        self.inner.pointer_drag_needs_animation()
    }

    fn advance_pointer_drag(&mut self, _elapsed: core::time::Duration) -> bool {
        self.inner.advance_pointer_drag(_elapsed)
    }

    fn get_window_info(&self) -> Option<scarlet_ui::views::WindowInfo> {
        self.inner.get_window_info()
    }

    fn get_window_size_limits(&self) -> Option<WindowSizeLimits> {
        self.inner.get_window_size_limits()
    }

    fn reveal_text_input(&mut self, _request: Option<(Rect, Rect)>) -> bool {
        self.inner.reveal_text_input(_request)
    }

    fn text_edit_state(&self) -> Option<scarlet_ui::clipboard::TextEditState> {
        self.inner.text_edit_state()
    }

    fn apply_text_edit(
        &mut self,
        _action: scarlet_ui::clipboard::TextEditAction,
        _text: Option<&str>,
    ) -> bool {
        self.inner.apply_text_edit(_action, _text)
    }

    fn text_input_state(&self) -> Option<TextInputElementState> {
        self.inner.text_input_state()
    }

    fn avoid_input_occlusion(
        &mut self,
        _occlusion: Option<Rect>,
        _caret: Option<Rect>,
        _reveal: bool,
    ) -> bool {
        self.inner
            .avoid_input_occlusion(_occlusion, _caret, _reveal)
    }

    fn clear_buffers(&mut self) {
        self.inner.clear_buffers()
    }

    fn fill_width(&self) -> bool {
        self.inner.fill_width()
    }

    fn fill_height(&self) -> bool {
        self.inner.fill_height()
    }

    fn wants_keyboard_focus(&self) -> bool {
        self.inner.wants_keyboard_focus()
    }

    fn accepts_keyboard_focus(&self) -> bool {
        self.inner.accepts_keyboard_focus()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scarlet_ui::{
        KeyCode, event::KeyModifiers, generate_state_id, pipeline::RenderingPipeline,
    };
    use std::sync::Mutex;

    #[test]
    fn modifier_only_events_reach_the_editor_observer_without_losing_text_input() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        let selection = State::new(generate_state_id(), TextSelection::collapsed(0));
        let editor = ObservedEditor {
            editor: TextView::new(
                State::new(generate_state_id(), "hello".into()),
                selection.clone(),
            )
            .autofocus(true),
            observer: Arc::new(move |event| {
                if let Event::ModifiersChanged(m) = event {
                    captured.lock().unwrap().push(m.primary());
                }
            }),
        };
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_root(
            Window::new("Modifier QA", editor)
                .size(Size::new(400.0, 200.0))
                .create_element(),
        );
        pipeline.layout_initial();
        pipeline.render();
        let mut modifiers = KeyModifiers::empty();
        if cfg!(target_os = "macos") {
            modifiers.super_key = true;
        } else {
            modifiers.control = true;
        }
        pipeline.handle_event(&Event::ModifiersChanged(modifiers));
        pipeline.handle_event(&Event::ModifiersChanged(KeyModifiers::empty()));
        assert_eq!(*seen.lock().unwrap(), vec![true, false]);
        pipeline.handle_event(&Event::Keyboard(scarlet_ui::event::KeyEvent::Pressed {
            keycode: KeyCode::Right,
            modifiers: KeyModifiers::empty(),
        }));
        assert_eq!(selection.get().caret.byte, 1);
    }
}
