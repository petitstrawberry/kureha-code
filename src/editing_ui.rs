use super::*;
use kureha_code::editing::{self, Direction};

#[derive(Clone, Copy)]
pub(super) enum Command {
    Comment,
    Indent,
    Outdent,
    DuplicateUp,
    DuplicateDown,
    MoveUp,
    MoveDown,
    Newline,
    BackspaceIndent,
}

impl App {
    pub(super) fn edit_command(&self, command: Command) {
        let Some(id) = self.active_id() else {
            return;
        };
        if !self.editable(id) {
            return;
        }
        let Some(bindings) = self.bindings(id) else {
            return;
        };
        let selection = bindings.selection.get();
        let preferences = self.preferences.get();
        {
            let mut rt = self.runtime.lock().unwrap();
            let Some(doc) = rt.workspace.get_mut(id) else {
                return;
            };
            let text = doc.editor.document.as_str();
            let plan = match command {
                Command::Comment => editing::toggle_line_comment(
                    &text,
                    selection,
                    doc.path
                        .as_ref()
                        .and_then(|p| p.extension())
                        .and_then(|s| s.to_str())
                        .unwrap_or(""),
                ),
                Command::Indent => editing::indent_with(
                    &text,
                    selection,
                    preferences.insert_spaces,
                    preferences.tab_size,
                ),
                Command::Outdent => editing::outdent_with(&text, selection, preferences.tab_size),
                Command::DuplicateUp => editing::duplicate_lines(&text, selection, Direction::Up),
                Command::DuplicateDown => {
                    editing::duplicate_lines(&text, selection, Direction::Down)
                }
                Command::MoveUp => editing::move_lines(&text, selection, Direction::Up),
                Command::MoveDown => editing::move_lines(&text, selection, Direction::Down),
                Command::BackspaceIndent => {
                    editing::backspace_indent(&text, selection, preferences.tab_size)
                }
                Command::Newline => editing::newline_with(
                    &text,
                    selection,
                    preferences.insert_spaces,
                    preferences.tab_size,
                ),
            };
            let Some(plan) = plan else {
                return;
            };
            if text
                .len()
                .saturating_sub(plan.edit.replaced_range.len())
                .saturating_add(plan.edit.inserted.len())
                > files::MAX_BYTES as usize
            {
                drop(rt);
                self.status
                    .set("Edit would exceed the document size limit".into());
                return;
            }
            doc.editor.moved(selection);
            doc.editor.edit_plan(&plan);
        };
        self.sync_editor(id);
        self.dismiss_language_panel();
        self.changed();
    }

    pub(super) fn code_edit_key(
        &self,
        key: KeyCode,
        modifiers: scarlet_ui::event::KeyModifiers,
    ) -> bool {
        let command = if modifiers.primary() && !modifiers.alt {
            match key {
                KeyCode::Char('/') => Some(Command::Comment),
                KeyCode::Char(']') => Some(Command::Indent),
                KeyCode::Char('[') => Some(Command::Outdent),
                _ => None,
            }
        } else if modifiers.alt && !modifiers.control && !modifiers.super_key {
            match (key, modifiers.shift) {
                (KeyCode::Up, true) => Some(Command::DuplicateUp),
                (KeyCode::Down, true) => Some(Command::DuplicateDown),
                (KeyCode::Up, false) => Some(Command::MoveUp),
                (KeyCode::Down, false) => Some(Command::MoveDown),
                _ => None,
            }
        } else if !modifiers.alt && !modifiers.control && !modifiers.super_key {
            match key {
                KeyCode::Backspace
                    if !modifiers.shift
                        && self
                            .active_id()
                            .and_then(|id| self.bindings(id))
                            .is_some_and(|b| {
                                editing::backspace_indent(
                                    &b.document.get().as_str(),
                                    b.selection.get(),
                                    self.preferences.get().tab_size,
                                )
                                .is_some()
                            }) =>
                {
                    Some(Command::BackspaceIndent)
                }
                KeyCode::Enter if !modifiers.shift => Some(Command::Newline),
                KeyCode::Tab if modifiers.shift => Some(Command::Outdent),
                KeyCode::Tab
                    if self
                        .active_id()
                        .and_then(|id| self.bindings(id))
                        .is_some_and(|b| b.selection.get().normalized_range().is_some()) =>
                {
                    Some(Command::Indent)
                }
                _ => None,
            }
        } else {
            None
        };
        if let Some(command) = command {
            self.edit_command(command);
            true
        } else {
            false
        }
    }
}
