//! Pane IDs remain stable when neighbours close, so async opens cannot target a
//! newly-created pane accidentally. Each pane owns its caret and viewport.
use super::*;
#[derive(Clone, Debug, PartialEq)]
pub(super) enum PaneLayout {
    Leaf(usize),
    Split {
        id: usize,
        down: bool,
        fraction: f32,
        first: Box<PaneLayout>,
        second: Box<PaneLayout>,
    },
}
impl PaneLayout {
    pub fn leaves(&self) -> Vec<usize> {
        match self {
            Self::Leaf(group) => vec![*group],
            Self::Split { first, second, .. } => {
                let mut groups = first.leaves();
                groups.extend(second.leaves());
                groups
            }
        }
    }
    pub fn split(&mut self, beside: usize, group: usize, edge: tab_drag::Edge) {
        match self {
            Self::Leaf(id) if *id == beside => {
                self.wrap(group, edge);
            }
            Self::Split { first, second, .. } => {
                first.split(beside, group, edge);
                second.split(beside, group, edge);
            }
            _ => {}
        }
    }
    pub fn wrap(&mut self, group: usize, edge: tab_drag::Edge) {
        let old = Box::new(self.clone());
        let new = Box::new(Self::Leaf(group));
        let (first, second) = if matches!(edge, tab_drag::Edge::Left | tab_drag::Edge::Top) {
            (new, old)
        } else {
            (old, new)
        };
        *self = Self::Split {
            id: group,
            down: matches!(edge, tab_drag::Edge::Top | tab_drag::Edge::Bottom),
            fraction: 0.5,
            first,
            second,
        };
    }
    pub fn retain(&self, keep: &impl Fn(usize) -> bool) -> Option<Self> {
        match self {
            Self::Leaf(group) => keep(*group).then(|| self.clone()),
            Self::Split {
                id,
                down,
                fraction,
                first,
                second,
            } => match (first.retain(keep), second.retain(keep)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    id: *id,
                    down: *down,
                    fraction: *fraction,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (one, other) => one.or(other),
            },
        }
    }
}
impl App {
    pub(super) fn pane_order(&self) -> Vec<usize> {
        self.pane_layout
            .get()
            .leaves()
            .into_iter()
            .filter(|g| self.group_exists(*g))
            .collect()
    }

    pub(super) fn bindings_for(&self, id: DocumentId, group: usize) -> Option<Bindings> {
        let mut rt = self.runtime.lock().unwrap();
        let first = rt.workspace.get(id)?.bindings.clone();
        if group == 0 {
            return Some(first);
        }
        Some(
            rt.pane_bindings
                .entry((id, group))
                .or_insert_with(|| first.another_view())
                .clone(),
        )
    }
    pub(super) fn group_exists(&self, group: usize) -> bool {
        self.panes.get().get(group).is_some_and(Option::is_some)
    }
    pub(super) fn group_id(&self, group: usize) -> DocumentId {
        let panes = self.panes.get();
        panes
            .get(group)
            .copied()
            .flatten()
            .or_else(|| panes.into_iter().flatten().next())
            .unwrap()
    }
    pub(super) fn set_group_id(&self, group: usize, id: DocumentId) {
        self.panes.update(|panes| panes[group] = Some(id));
    }
    pub(super) fn focus_group(&self, group: usize) {
        if !self.group_exists(group) {
            return;
        }
        self.project.blur();
        if self.focused_group.get() != group {
            self.invalidate_clipboard();
            self.focused_group.set(group);
            self.dismiss_language_panel();
        }
        self.blur_terminals();
    }
    pub(super) fn allocate_group(&self, id: DocumentId) -> usize {
        let group = self.panes.get().len();
        self.runtime.lock().unwrap().group_tabs.push(vec![id]);
        self.panes.update(|panes| panes.push(Some(id)));
        group
    }
    pub(super) fn add_group(&self, id: DocumentId, beside: usize, edge: tab_drag::Edge) -> usize {
        self.remember_pane_sizes();
        let group = self.allocate_group(id);
        self.pane_layout
            .update(|layout| layout.split(beside, group, edge));
        group
    }
    pub(super) fn split_editor(&self, down: bool) {
        if self.blocked() {
            return;
        }
        let source = self.focused_group.get();
        let id = self.active_id();
        let group = self.add_group(
            id,
            source,
            if down {
                tab_drag::Edge::Bottom
            } else {
                tab_drag::Edge::Right
            },
        );
        if let (Some(from), Some(to)) =
            (self.bindings_for(id, source), self.bindings_for(id, group))
        {
            to.selection.set(from.selection.get());
            to.scroll.set(from.scroll.get());
        }
        self.focus_group(group);
        self.status.set(
            if down {
                "Pane split below"
            } else {
                "Pane split right"
            }
            .into(),
        );
    }
    /// Remove only a view; tabs and PTYs migrate to its nearest neighbour.
    pub(super) fn close_split(&self) {
        let order = self.pane_order();
        if order.len() < 2 {
            return;
        }
        let source = self.focused_group.get();
        let position = order.iter().position(|g| *g == source).unwrap();
        let target = order[if position == 0 { 1 } else { position - 1 }];
        let active = self.group_id(target);
        let tabs = self.runtime.lock().unwrap().group_tabs[source].clone();
        for id in tabs {
            let shared = self.runtime.lock().unwrap().group_tabs[target].contains(&id);
            if shared {
                self.runtime.lock().unwrap().group_tabs[source].retain(|tab| *tab != id);
            } else {
                self.move_tab_internal(id, source, target, usize::MAX, false);
            }
        }
        self.retire_empty_group(source);
        self.focus_group(target);
        self.set_group_id(target, active);
        self.status
            .set("Pane closed; tabs moved to neighbour".into());
    }
    pub(super) fn retire_empty_group(&self, group: usize) {
        if self.pane_order().len() < 2 {
            return;
        }
        if !self.runtime.lock().unwrap().group_tabs[group].is_empty() {
            return;
        }
        self.panes.update(|panes| panes[group] = None);
        if let Some(layout) = self.pane_layout.get().retain(&|g| g != group) {
            self.pane_layout.set(layout);
        }
        let mut rt = self.runtime.lock().unwrap();
        rt.pane_bindings.retain(|(_, g), _| *g != group);
        rt.image_zoom.retain(|(_, g), _| *g != group);
        drop(rt);
        if self.focused_group.get() == group {
            self.focus_group(self.pane_order()[0]);
        }
    }
    pub(super) fn focus_other_group(&self) {
        let order = self.pane_order();
        let index = order
            .iter()
            .position(|g| *g == self.focused_group.get())
            .unwrap_or(0);
        self.focus_group(order[(index + 1) % order.len()]);
    }
    pub(super) fn focus_pane_number(&self, number: usize) {
        if let Some(group) = self.pane_order().get(number).copied() {
            self.focus_group(group);
        } else {
            self.split_editor(false);
        }
    }
    pub(super) fn close_shared_tab(&self, id: DocumentId) -> bool {
        let group = self.focused_group.get();
        let active = self.group_id(group);
        let next = {
            let mut rt = self.runtime.lock().unwrap();
            if !rt
                .group_tabs
                .iter()
                .enumerate()
                .any(|(g, tabs)| g != group && tabs.contains(&id))
            {
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
            } else {
                self.retire_empty_group(group);
            }
        }
        self.changed();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn three_views_share_edits_but_keep_viewports_and_transform_every_selection() {
        let app = App::new();
        let id = app.active_id();
        app.edit(
            id,
            &EditDelta {
                replaced_range: 0..0,
                inserted: "abc\ndef".into(),
            },
        );
        app.split_editor(false);
        app.split_editor(false);
        for group in 0..3 {
            let view = app.bindings_for(id, group).unwrap();
            view.selection.set(TextSelection::collapsed(5 + group));
            view.scroll.set(TextViewScroll {
                x: 0.0,
                y: group as f32 * 20.0,
            });
        }
        app.edit(
            id,
            &EditDelta {
                replaced_range: 0..0,
                inserted: "XX".into(),
            },
        );
        for group in 0..2 {
            let view = app.bindings_for(id, group).unwrap();
            assert_eq!(view.selection.get().caret.byte, 7 + group);
            assert_eq!(view.scroll.get().y, group as f32 * 20.0);
            assert_eq!(view.document.get().as_str(), "XXabc\ndef");
        }
        app.focus_group(1);
        app.close_split();
        assert_eq!(app.pane_order(), vec![0, 2]);
        assert_eq!(app.bindings_for(id, 2).unwrap().scroll.get().y, 40.0);
    }
    #[test]
    fn async_open_cannot_target_a_new_pane_after_its_origin_closes() {
        let app = App::new();
        app.split_editor(false);
        app.split_editor(false);
        app.focus_group(1);
        app.close_split();
        app.split_editor(false);
        assert_eq!(app.pane_order(), vec![0, 3, 2]);
        let active = app.active_id();
        app.complete_open(1, PathBuf::from("delayed.txt"), "result".into(), None);
        assert_eq!(app.focused_group.get(), 3);
        assert_eq!(app.active_id(), active);
        let rt = app.runtime.lock().unwrap();
        let opened = rt.workspace.find_path(Path::new("delayed.txt")).unwrap();
        assert!(rt.group_tabs[0].contains(&opened));
        assert!(!rt.group_tabs[3].contains(&opened));
    }
}
