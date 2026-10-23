//! Two editor groups share documents and undo history, not viewport state.
use super::*;
impl App {
    pub(super) fn bindings_for(&self, id: DocumentId, group: usize) -> Option<Bindings> {
        let mut rt = self.runtime.lock().unwrap();
        let first = rt.workspace.get(id)?.bindings.clone();
        if group == 0 {
            return Some(first);
        }
        Some(
            rt.second_bindings
                .entry(id)
                .or_insert_with(|| first.another_view())
                .clone(),
        )
    }
    pub(super) fn group_id(&self, group: usize) -> DocumentId {
        let rt = self.runtime.lock().unwrap();
        if group == 1 {
            if let Some(id) = self
                .second
                .get()
                .filter(|id| rt.workspace.get(*id).is_some())
            {
                return id;
            }
        }
        rt.workspace.documents[self.active.get().min(rt.workspace.documents.len() - 1)].id
    }
    pub(super) fn focus_group(&self, group: usize) {
        if group == 1 && self.second.get().is_none() {
            return;
        }
        if self.focused_group.get() != group {
            self.invalidate_clipboard();
            self.focused_group.set(group);
            self.dismiss_language_panel();
        }
        self.terminal.blur();
    }
    pub(super) fn split_editor(&self, down: bool) {
        if self.blocked() {
            return;
        }
        if self.second.get().is_none() {
            let id = self.active_id();
            self.bindings_for(id, 1);
            self.runtime.lock().unwrap().group_tabs[1] = vec![id];
            self.second.set(Some(id));
        }
        self.split_down.set(down);
        self.focus_group(1);
        self.status.set(
            if down {
                "Editor split below"
            } else {
                "Editor split right"
            }
            .into(),
        );
    }
    pub(super) fn close_split(&self) {
        // Closing a view never closes its document or discards unsaved changes.
        {
            let mut rt = self.runtime.lock().unwrap();
            let second = std::mem::take(&mut rt.group_tabs[1]);
            for id in second {
                if !rt.group_tabs[0].contains(&id) {
                    rt.group_tabs[0].push(id);
                }
            }
        }
        self.focus_group(0);
        self.second.set(None);
        self.status.set("Second editor group closed".into());
    }
    pub(super) fn focus_other_group(&self) {
        if self.second.get().is_some() {
            self.focus_group(1 - self.focused_group.get());
        }
    }
    /// A document still visible in the other group is only detached here.
    pub(super) fn close_shared_tab(&self, id: DocumentId) -> bool {
        let group = self.focused_group.get();
        let active = self.group_id(group);
        let next = {
            let mut rt = self.runtime.lock().unwrap();
            if !rt.group_tabs[1 - group].contains(&id) {
                return false;
            }
            let tabs = &mut rt.group_tabs[group];
            let position = tabs.iter().position(|tab| *tab == id).unwrap_or(0);
            tabs.retain(|tab| *tab != id);
            tabs.get(position.min(tabs.len().saturating_sub(1)))
                .copied()
        };
        if active == id {
            if let Some(next) = next {
                self.activate_unchecked(next);
            } else if group == 1 {
                self.close_split();
            } else {
                self.new_document();
            }
        }
        self.changed();
        true
    }
}
