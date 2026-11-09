//! Explicit navigation remembers a location, not every cursor or scroll event.
use super::*;
const LIMIT: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Location {
    id: DocumentId,
    group: usize,
    selection: TextSelection,
    scroll: TextViewScroll,
}
#[derive(Default)]
pub(super) struct History {
    back: Vec<Location>,
    forward: Vec<Location>,
}
impl History {
    fn push(entries: &mut Vec<Location>, location: Location) {
        if entries.last() == Some(&location) {
            return;
        }
        if entries.len() == LIMIT {
            entries.remove(0);
        }
        entries.push(location);
    }
    pub fn remove(&mut self, id: DocumentId) {
        self.back.retain(|location| location.id != id);
        self.forward.retain(|location| location.id != id);
    }
}
impl App {
    fn location_in(&self, group: usize) -> Option<Location> {
        if self.preview_tab(group).is_some() {
            return None;
        }
        let id = self.group_document(group)?;
        let bindings = self.bindings_for(id, group)?;
        Some(Location {
            id,
            group,
            selection: bindings.selection.get(),
            scroll: bindings.scroll.get(),
        })
    }
    pub(super) fn remember_location_in(&self, group: usize) {
        let Some(location) = self.location_in(group) else {
            return;
        };
        {
            let mut rt = self.runtime.lock().unwrap();
            History::push(&mut rt.history.back, location);
            rt.history.forward.clear();
        }
        self.changed();
    }
    pub(super) fn remember_location(&self) {
        self.remember_location_in(self.focused_group.get());
    }
    pub(super) fn can_go_back(&self) -> bool {
        !self.runtime.lock().unwrap().history.back.is_empty()
    }
    pub(super) fn can_go_forward(&self) -> bool {
        !self.runtime.lock().unwrap().history.forward.is_empty()
    }
    pub(super) fn go_back(&self) {
        self.traverse_history(false);
    }
    pub(super) fn go_forward(&self) {
        self.traverse_history(true);
    }
    fn traverse_history(&self, forward: bool) {
        if self.blocked() || self.runtime.lock().unwrap().job.is_some() {
            return;
        }
        let Some(current) = self.location_in(self.focused_group.get()) else {
            return;
        };
        let target = {
            let mut rt = self.runtime.lock().unwrap();
            let target = loop {
                let next = if forward {
                    rt.history.forward.pop()
                } else {
                    rt.history.back.pop()
                };
                match next {
                    Some(location)
                        if location != current && rt.workspace.get(location.id).is_some() =>
                    {
                        break Some(location);
                    }
                    Some(_) => continue,
                    None => break None,
                }
            };
            if target.is_some() {
                if forward {
                    History::push(&mut rt.history.back, current);
                } else {
                    History::push(&mut rt.history.forward, current);
                }
            }
            target
        };
        if let Some(target) = target {
            let group = if self.group_exists(target.group) {
                target.group
            } else {
                self.pane_order()[0]
            };
            self.focus_group(group);
            self.activate_unchecked(target.id);
            let bindings = self.bindings_for(target.id, group).unwrap();
            let document = bindings.document.get();
            let text = document.as_str();
            let clamp = |position: TextPosition| {
                let mut byte = position.byte.min(text.len());
                while !text.is_char_boundary(byte) {
                    byte -= 1;
                }
                TextPosition::new(byte)
            };
            let selection = TextSelection {
                anchor: clamp(target.selection.anchor),
                caret: clamp(target.selection.caret),
            };
            self.runtime
                .lock()
                .unwrap()
                .workspace
                .get_mut(target.id)
                .unwrap()
                .editor
                .moved(selection);
            bindings.selection.set(selection);
            bindings.scroll.set(target.scroll);
            self.dismiss_language_panel();
            self.close_navigation();
            self.invalidate_clipboard();
            self.update_position();
        }
        self.changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scarlet_ui::pipeline::RenderingPipeline;
    use serde_json::json;

    fn populated() -> App {
        let app = App::with_document();
        let id = app.document_id();
        {
            let mut rt = app.runtime.lock().unwrap();
            let doc = rt.workspace.get_mut(id).unwrap();
            doc.editor = kureha_code::editor::Editor::new(&"line\n".repeat(400));
            doc.path = Some(PathBuf::from("/tmp/history.rs"));
        }
        app.sync_editor(id);
        app
    }

    #[test]
    fn definition_jump_back_and_forward_restore_selection_and_viewport_after_render() {
        let app = populated();
        let id = app.document_id();
        let bindings = app.bindings(id).unwrap();
        let window = || {
            Window::new("History", app.content())
                .decorated(false)
                .size(Size::new(1100.0, 760.0))
        };
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_paint_enabled(false);
        pipeline.set_root(window().create_element());
        pipeline.layout_initial();
        pipeline.render();
        let selection = TextSelection::collapsed(500);
        bindings.selection.set(selection);
        bindings.scroll.set(TextViewScroll { x: 0.0, y: 1600.0 });
        pipeline.render();
        let before = bindings.scroll.get();
        app.language_navigate(
            PathBuf::from("/tmp/history.rs"),
            json!({"start":{"line":300,"character":0},"end":{"line":300,"character":4}}),
        );
        pipeline
            .element_tree_mut()
            .root_mut()
            .unwrap()
            .update(&window());
        pipeline.render();
        let target = bindings.selection.get();
        let target_scroll = bindings.scroll.get();
        assert_ne!(target, selection);
        assert!(app.can_go_back());
        app.go_back();
        pipeline
            .element_tree_mut()
            .root_mut()
            .unwrap()
            .update(&window());
        pipeline.render();
        assert_eq!(bindings.selection.get(), selection);
        assert_eq!(bindings.scroll.get(), before);
        assert!(app.can_go_forward());
        app.go_forward();
        pipeline
            .element_tree_mut()
            .root_mut()
            .unwrap()
            .update(&window());
        pipeline.render();
        assert_eq!(bindings.selection.get(), target);
        assert_eq!(bindings.scroll.get(), target_scroll);
        pipeline.teardown();
    }

    #[test]
    fn tabs_branching_closed_documents_and_history_limit() {
        let app = populated();
        let first = app.document_id();
        app.new_document();
        let second = app.document_id();
        app.go_back();
        assert_eq!(app.document_id(), first);
        app.go_forward();
        assert_eq!(app.document_id(), second);
        app.go_back();
        app.new_document();
        let third = app.document_id();
        assert!(
            !app.can_go_forward(),
            "new navigation discards the forward branch"
        );
        app.remove_document(first);
        assert!(
            !app.can_go_back(),
            "closed documents must not leave dead back entries"
        );
        assert_eq!(app.document_id(), third);
        for i in 0..LIMIT + 20 {
            app.bindings(third).unwrap().scroll.set(TextViewScroll {
                x: 0.0,
                y: i as f32,
            });
            app.remember_location();
        }
        assert_eq!(app.runtime.lock().unwrap().history.back.len(), LIMIT);
    }

    #[test]
    fn history_restores_original_split_and_clamps_positions_after_edits() {
        let app = populated();
        let first = app.document_id();
        app.split_editor(false);
        app.bindings_for(first, 1)
            .unwrap()
            .selection
            .set(TextSelection::collapsed(500));
        app.new_document();
        app.focus_group(0);
        app.go_back();
        assert_eq!(app.focused_group.get(), 1);
        assert_eq!(app.document_id(), first);
        assert_eq!(app.bindings(first).unwrap().selection.get().caret.byte, 500);
        app.go_forward();
        app.bindings_for(first, 1)
            .unwrap()
            .document
            .set(TextDocument::from_str("猫"));
        app.focus_group(1);
        app.close_split();
        app.go_back();
        assert_eq!(app.focused_group.get(), 0);
        assert_eq!(app.document_id(), first);
        assert!(
            app.bindings(first)
                .unwrap()
                .document
                .get()
                .as_str()
                .is_char_boundary(app.bindings(first).unwrap().selection.get().caret.byte)
        );
    }
}
