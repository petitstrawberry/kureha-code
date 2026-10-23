//! The workbench uses ScarletUI controls; the window menu is the app menu.
use super::*;
use kureha_code::{file_icons, syntax};
use scarlet_ui::views::containers::ViewTuple;

#[derive(Clone)]
pub(super) struct ViewCollection<V: View + Clone>(pub(super) Vec<V>);
impl<V: View + Clone> ViewTuple for ViewCollection<V> {
    fn create_elements(&self) -> Vec<Box<dyn Element>> {
        self.0.iter().map(|view| view.create_element()).collect()
    }
    fn clone_views(&self) -> Vec<Box<dyn View>> {
        self.0.iter().map(|view| view.clone_view()).collect()
    }
    fn collect_listenables<'a>(&'a self, target: &mut Vec<&'a dyn Listenable>) {
        for view in &self.0 {
            target.extend(view.listenables());
        }
    }
}
impl App {
    pub(super) fn content(&self) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        let keys = self.clone();
        ZStack::new((vstack! {
            SplitView::new(if_view!(self.project.is_open(), self.project_search_view(), self.explorer_view()), self.editor_area())
                .fraction(0.22).min_first(170.0).min_second(420.0).divider_colors(theme.colors().border,theme.colors().accent)
                .frame(f32::INFINITY, f32::INFINITY),
            hstack! {
                Text::from_state(self.status.clone()).font_size(11.0).color(theme.colors().foreground),
                Spacer::new(),
                Text::from_state(self.language_status.clone()).font_size(11.0).color(theme.colors().muted),
                Text::from_state(self.position.clone()).font_size(11.0).color(theme.colors().muted),
            }.spacing(12.0).padding(6.0).background(theme.colors().surface),
        }
        .spacing(0.0)
        .frame(f32::INFINITY, f32::INFINITY)
        .background(theme.colors().background), self.navigation_overlay())).alignment(Alignment::TopLeading)
        .frame(f32::INFINITY,f32::INFINITY)
        .on_event_capture(move |event| {
            let Event::Keyboard(KeyEvent::Pressed { keycode, modifiers }) = event else {
                return false;
            };
            if keys.navigation_key(*keycode,*modifiers) { return true; }
            if keys.nav.is_open() { return false; }
            if modifiers.primary() && modifiers.shift && matches!(keycode, KeyCode::Char('f'|'F')) {
                keys.show_project_search(); return true;
            }
            if modifiers.control && matches!(keycode, KeyCode::Char('`')) {
                keys.toggle_terminal();
                return true;
            }
            if keys.terminal_visible.get() && keys.terminal.focused() {
                if modifiers.primary() && matches!(keycode,KeyCode::Char('v'|'V')) && (cfg!(target_os="macos") || modifiers.shift) {keys.paste_terminal();return true;}
                if modifiers.primary() && matches!(keycode, KeyCode::Char('j' | 'J')) {
                    keys.toggle_terminal();
                    return true;
                }
                return false;
            }
            if keys.language_key(*keycode, *modifiers) {
                return true;
            }
            if keys.language_rename_open() {return false;}
            if *keycode == KeyCode::Escape && keys.dismiss_language_panel() {
                return true;
            }
            if *keycode == KeyCode::F(12) {
                keys.language_request("textDocument/definition");
                return true;
            }
            if *keycode == KeyCode::Space && modifiers.control {
                keys.language_request("textDocument/completion");
                return true;
            }
            if *keycode == KeyCode::F(3) {
                keys.find(modifiers.shift);
                return true;
            }
            if *keycode == KeyCode::Escape && keys.find_open.get() {
                keys.find_open.set(false);
                return true;
            }
            if *keycode == KeyCode::Tab && modifiers.control && !modifiers.alt {
                keys.next_tab(modifiers.shift);
                return true;
            }
            if !modifiers.primary() {
                return false;
            }
            match keycode {
                KeyCode::Char('\\' | '|') => keys.split_editor(modifiers.shift),
                KeyCode::Char('1') => keys.focus_group(0),
                KeyCode::Char('2') => { if keys.second.get().is_none() { keys.split_editor(false); } else { keys.focus_group(1); } },
                KeyCode::Char('j' | 'J') => keys.toggle_terminal(),
                KeyCode::Char('n' | 'N') => keys.new_document(),
                KeyCode::Char('o' | 'O') if modifiers.shift => keys.show_folder(),
                KeyCode::Char('o' | 'O') => keys.picker(DialogPurpose::Open),
                KeyCode::Char('s' | 'S') if modifiers.shift => {
                    keys.picker(DialogPurpose::Save(keys.active_id()))
                }
                KeyCode::Char('s' | 'S') if modifiers.alt => keys.save_all(),
                KeyCode::Char('s' | 'S') => keys.save(),
                KeyCode::Char('w' | 'W') if modifiers.shift => keys.request_close(None),
                KeyCode::Char('w' | 'W') => keys.request_close(Some(keys.active_id())),
                KeyCode::Char('f' | 'F') => keys.show_find(),
                KeyCode::Char('h' | 'H') if !cfg!(target_os = "macos") => keys.show_find(),
                KeyCode::Char('g' | 'G') => keys.find(modifiers.shift),
                KeyCode::Tab => keys.next_tab(modifiers.shift),
                _ => return false,
            }
            true
        })
    }
    fn editor_area(&self) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        vstack! {
            SplitView::new(self.editor_groups(),self.bottom_panel()).axis(scarlet_ui::views::SplitAxis::Vertical).divider_colors(theme.colors().border,theme.colors().accent)
                .fraction(if self.terminal_visible.get() {0.70} else {1.0})
                .min_first(120.0).min_second(if self.terminal_visible.get() {100.0} else {0.0})
                .frame(f32::INFINITY,f32::INFINITY),
            self.language_panel(),
        }
        .spacing(0.0)
        .frame(f32::INFINITY, f32::INFINITY)
    }
    fn editor_groups(&self) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        SplitView::new(
            self.editor_group(0),
            if_view!(
                self.second.get().is_some(),
                self.editor_group(1),
                VStack::new(())
            ),
        )
        .axis(if self.split_down.get() {
            scarlet_ui::views::SplitAxis::Vertical
        } else {
            scarlet_ui::views::SplitAxis::Horizontal
        })
        .fraction(if self.second.get().is_some() {
            0.5
        } else {
            1.0
        })
        .min_first(if self.split_down.get() { 100.0 } else { 180.0 })
        .min_second(if self.second.get().is_some() {
            if self.split_down.get() { 100.0 } else { 180.0 }
        } else {
            0.0
        })
        .divider_colors(theme.colors().border, theme.colors().accent)
        .frame(f32::INFINITY, f32::INFINITY)
    }
    fn editor_group(&self, group: usize) -> impl View + Clone + use<> {
        vstack! { self.tabs_view(group), self.editor_view(self.group_id(group), group), }
            .spacing(0.0)
            .frame(f32::INFINITY, f32::INFINITY)
            .key(format!("editor-group-{group}"))
    }
    fn bottom_panel(&self) -> impl View + Clone + use<> {
        let close = self.clone();
        let restart = self.clone();
        let theme = self.theme.get();
        if_view!(self.terminal_visible.get(),vstack! {
            hstack! {
                Text::new("TERMINAL").font_size(11.0).color(theme.colors().muted),
                Spacer::new(),
                Button::new("New Session").header_style().font_size(12.0).text_color(theme.colors().foreground).background_color(theme.colors().surface).on_click(move || {restart.terminal.close();restart.show_terminal();}),
                Button::new("×").header_style().text_color(theme.colors().foreground).background_color(theme.colors().surface).on_click(move ||close.toggle_terminal()),
            }.spacing(8.0).padding(5.0).background(theme.colors().surface),
            self.terminal.view_with_font(self.editor_font.clone()),
        }.spacing(0.0).frame(f32::INFINITY,f32::INFINITY),VStack::new(()))
    }
    fn tabs_view(&self, group: usize) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        let active = self.group_id(group);
        let summaries = {
            let rt = self.runtime.lock().unwrap();
            rt.workspace.documents.iter().filter(|doc| rt.group_tabs[group].contains(&doc.id)).map(|doc| {
                let saving = matches!(rt.job.as_ref().map(|job| &job.operation), Some(FileOperation::Save { id, .. }) if *id == doc.id);
                (doc.id, doc.path.clone(), format!("{}{}{}", if doc.editor.dirty() { "● " } else { "" }, doc.name(), if saving { " …" } else { "" }))
            }).collect::<Vec<_>>()
        };
        let mut left = 0.0;
        let mut selected_range = (0.0, 0.0);
        let tabs = summaries.into_iter().map(|(id, path, label)| {
            let width = (label.chars().count() as f32 * 7.0 + 70.0).clamp(120.0, 240.0);
            if id == active { selected_range = (left, left + width); }
            left += width;
            let select = self.clone();
            let close = self.clone();
            let background = if id == active { theme.colors().background } else { theme.colors().surface };
            let icon = file_icons::for_path(path.as_deref());
            hstack! {
                Button::new(label).header_style().text_color(theme.colors().foreground).background_color(theme.colors().surface).font_size(13.0).padding(7.0)
                    .icon(icon.icon).icon_color(icon.color).icon_size(IconSize::Small)
                    .background_color(background).on_click(move || {select.focus_group(group); select.activate(id);}).frame(width - 28.0, 32.0),
                Button::new("×").header_style().text_color(theme.colors().foreground).background_color(theme.colors().surface).padding(4.0).background_color(background)
                    .on_click(move || {close.focus_group(group); close.request_close(Some(id));}).frame(28.0, 32.0),
            }.spacing(0.0).frame(width, 32.0).background(background).key(id)
        }).collect::<Vec<_>>();
        ScrollView::new(HStack::new(ViewCollection(tabs)).spacing(0.0))
            .horizontal()
            .content_size(left, 32.0)
            .scroll_to_horizontal_range(selected_range.0, selected_range.1)
            .frame(f32::INFINITY, 34.0)
            .background(theme.colors().surface)
    }
    fn editor_view(&self, id: DocumentId, group: usize) -> impl View + Clone + use<> {
        let bindings = self.bindings_for(id, group).unwrap();
        let edited = self.clone();
        let keys = self.clone();
        let selection = bindings.selection.clone();
        let hovering = self.clone();
        let definition = self.clone();
        let theme = self.theme.get();
        let (styles, fallback, minimap) = {
            let rt = self.runtime.lock().unwrap();
            let doc = rt.workspace.get(id).unwrap();
            let revision = doc.editor.revision();
            // Materialize the immutable document only when its revision/path
            // changes; TextDocument caches as_str for subsequent paints.
            let styles = syntax::spans(
                id,
                revision,
                doc.path.as_deref(),
                &doc.editor.document.as_str(),
            );
            (
                styles,
                syntax::fallback_reason(id, revision, doc.path.as_deref()),
                (doc.editor.document.clone(), doc.path.clone(), revision),
            )
        };
        let snapshot = self.runtime.lock().unwrap().minimap.snapshot(
            id,
            minimap.2,
            minimap.1.as_deref(),
            &minimap.0,
        );
        let minimap_scroll = bindings.scroll.clone();
        let editor = TextView::with_document(bindings.document, bindings.selection)
            .text_spans(styles)
            .diagnostic_spans(self.language_diagnostics(id))
            .caret_rect_state(if self.focused_group.get() == group {
                self.caret_rect.clone()
            } else {
                self.inactive_caret_rect.clone()
            })
            .on_hover(move |hit| {
                if hovering.focused_group.get() == group {
                    hovering.pointer_hover(id, hit.map(|hit| (hit.byte_offset, hit.position)))
                }
            })
            .on_modified_click(move |hit| {
                definition.focus_group(group);
                definition.definition_at(id, hit.byte_offset);
            })
            .autofocus(
                self.focused_group.get() == group
                    && !self.find_open.get()
                    && !self.nav.is_open()
                    && !self.project.wants_focus()
                    && !self.language_rename_open(),
            )
            .scroll_state(bindings.scroll)
            .wrap_mode(WrapMode::None)
            .line_numbers(true)
            .current_line_highlight(true)
            .tab_mode(TabMode::Spaces(4))
            .font_size(14.0)
            .padding(10.0)
            .reveal_selection_on_change(true)
            .border_color(Color::TRANSPARENT)
            .focused_border_color(Color::TRANSPARENT)
            .on_text_change(move |delta| {
                edited.edit(id, delta);
                edited.schedule_completion(id, delta);
                // Every revision needs fresh syntax/diagnostic decorations,
                // including typing after the dirty flag is already set.
                edited.changed();
            });
        let editor = theme
            .style_text_view(editor)
            .borderless()
            .border_color(Color::TRANSPARENT)
            .focused_border_color(Color::TRANSPARENT);
        let editor = if let Some(stack) = &self.editor_font {
            editor.font_stack(stack.clone())
        } else {
            editor
        };
        let editor = editor
            .frame(f32::INFINITY, f32::INFINITY)
            .on_event_capture(move |event| {
                if matches!(
                    event,
                    Event::Mouse(scarlet_ui::event::MouseEvent::ButtonPressed { .. })
                ) {
                    keys.focus_group(group);
                }
                if !keys.editable(id)
                    && matches!(
                        event,
                        Event::Keyboard(_)
                            | Event::TextInputPreedit { .. }
                            | Event::TextInputCommit { .. }
                            | Event::TextInputDeleteSurroundingText { .. }
                    )
                {
                    return true;
                }
                if let Some(doc) = keys.runtime.lock().unwrap().workspace.get_mut(id) {
                    doc.editor.moved(selection.get());
                }
                if let Event::Keyboard(KeyEvent::Pressed { keycode, modifiers }) = event {
                    if keys.code_edit_key(*keycode, *modifiers) {
                        return true;
                    }
                    if modifiers.primary() {
                        match keycode {
                            KeyCode::Char('z' | 'Z') => keys.undo(modifiers.shift),
                            KeyCode::Char('y' | 'Y') => keys.undo(true),
                            KeyCode::Char('x' | 'X') => keys.copy(true),
                            KeyCode::Char('c' | 'C') => keys.copy(false),
                            KeyCode::Char('v' | 'V') => keys.paste(),
                            KeyCode::Char('a' | 'A') => keys.select_all(),
                            _ => return false,
                        }
                        return true;
                    }
                }
                false
            })
            .key(id);
        vstack! {
            if_view!(fallback.is_some(),
                Text::new(fallback.unwrap_or("")).font_size(11.0)
                    .color(theme.colors().muted).padding(6.0),
                VStack::new(())),
            hstack! {
                ZStack::new((editor,if_view!(self.focused_group.get()==group,self.language_popup(),VStack::new(())),if_view!(self.find_open.get() && self.focused_group.get()==group,super::popup::Anchored::top_right(self.find_view()),VStack::new(())))).alignment(Alignment::TopLeading)
                    .frame(f32::INFINITY,f32::INFINITY),
                kureha_code::minimap::Minimap::new(snapshot,minimap_scroll,theme).width(if self.second.get().is_some() {54.0} else {100.0}).enabled(self.minimap_visible.get()).line_height(self.editor_font.as_ref().map_or_else(||scarlet_ui::graphics::line_height_sized(14.0),|font|scarlet_ui::graphics::line_height_sized_with_font_stack(14.0,font)) as f32).view(),
            }.spacing(0.0).frame(f32::INFINITY,f32::INFINITY),
        }
        .spacing(0.0)
        .frame(f32::INFINITY, f32::INFINITY)
    }
    fn explorer_view(&self) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        let open = self.clone();
        let refresh = self.clone();
        let rows = self.clone();
        let selected_color = theme.colors().selection;
        let has_root = self.runtime.lock().unwrap().explorer.root.is_some();
        vstack! {
            hstack! {
                Text::new("EXPLORER").font_size(11.0).color(theme.colors().muted),
                Spacer::new(),
                Button::new("Open Folder").header_style().text_color(theme.colors().foreground).background_color(theme.colors().surface).font_size(11.0).padding(5.0).on_click(move || open.show_folder()),
                Button::icon_only(Icon::Refresh).header_style().text_color(theme.colors().foreground).background_color(theme.colors().surface).padding(5.0).enabled(has_root).on_click(move || {
                    let root = refresh.runtime.lock().unwrap().explorer.root.clone();
                    if let Some(root) = root { refresh.open_folder(root); }
                }),
            }.spacing(4.0).padding(8.0),
            if_view!(has_root,
                ListView::new(self.tree_rows.clone(), self.tree_selected.clone(), 27.0, move |index, row: Row, selected| {
                    let app = rows.clone();
                    let enabled = row.path.is_some();
                    let icon = if row.directory { file_icons::folder(row.expanded) }
                        else { file_icons::for_path(row.path.as_deref()) };
                    hstack! {
                        Spacer::new().frame(row.depth as f32 * 14.0, 1.0),
                        Button::new(row.label.clone()).header_style().text_color(theme.colors().foreground).background_color(theme.colors().surface).font_size(13.0).padding(4.0).enabled(enabled)
                            .icon(icon.icon).icon_color(icon.color).icon_size(IconSize::Small)
                            .on_click(move || app.open_tree_row(row.clone(), index)),
                        Spacer::new(),
                    }.spacing(0.0).frame(f32::INFINITY, 27.0)
                        .background(if selected == Some(index) { selected_color } else { Color::TRANSPARENT })
                }).item_key(|row| row.key.clone().into())
                  .frame(f32::INFINITY, f32::INFINITY),
                vstack! {
                    Text::new("Open a folder to browse files.").font_size(12.0).color(theme.colors().muted),
                    Spacer::new(),
                }.padding(12.0).frame(f32::INFINITY, f32::INFINITY)
            ),
        }.spacing(0.0).background(theme.colors().surface).frame(f32::INFINITY, f32::INFINITY)
    }
    fn find_view(&self) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        let submit = self.clone();
        let previous = self.clone();
        let next = self.clone();
        let replace = self.clone();
        let all = self.clone();
        let close = self.clone();
        let case = self.clone();
        let word = self.clone();
        let regex = self.clone();
        let options = self.find_options.get();
        vstack! {
            hstack! {
                TextField::new(self.query.clone()).background_color(theme.colors().elevated).text_color(theme.colors().foreground).placeholder_color(theme.colors().muted).border_color(theme.colors().border).placeholder("Find").autofocus(true)
                    .on_submit(move || submit.find(false)).frame_width(f32::INFINITY),
                Button::new("Previous").header_style().text_color(theme.colors().foreground).background_color(theme.colors().surface).on_click(move || previous.find(true)),
                Button::new("Next").header_style().text_color(theme.colors().foreground).background_color(theme.colors().surface).on_click(move || next.find(false)),
                Button::new("×").header_style().text_color(theme.colors().foreground).background_color(theme.colors().surface).on_click(move || close.find_open.set(false)),
            }.spacing(6.0),
            hstack! {
                TextField::new(self.replacement.clone()).background_color(theme.colors().elevated).text_color(theme.colors().foreground).placeholder_color(theme.colors().muted).border_color(theme.colors().border).placeholder("Replace with").frame_width(f32::INFINITY),
                Button::new("Replace").header_style().text_color(theme.colors().foreground).background_color(theme.colors().surface).on_click(move || replace.replace_match()),
                Button::new("Replace All").header_style().text_color(theme.colors().foreground).background_color(theme.colors().surface).on_click(move || all.replace_all()),
            }.spacing(6.0),
            hstack! {
                Button::new("Aa").header_style().font_size(12.0).text_color(theme.colors().foreground).background_color(if options.match_case {theme.colors().selection} else {theme.colors().surface}).on_click(move ||case.find_options.update(|o| o.match_case = !o.match_case)),
                Button::new("Whole Word").header_style().font_size(12.0).text_color(theme.colors().foreground).background_color(if options.whole_word {theme.colors().selection} else {theme.colors().surface}).on_click(move ||word.find_options.update(|o| o.whole_word = !o.whole_word)),
                Button::new(".*").header_style().font_size(12.0).text_color(theme.colors().foreground).background_color(if options.regex {theme.colors().selection} else {theme.colors().surface}).on_click(move ||regex.find_options.update(|o| o.regex = !o.regex)),
                Spacer::new(),
            }.spacing(4.0),
            Text::from_state(self.search_status.clone()).font_size(11.0).color(theme.colors().muted),
        }.alignment(Alignment::Leading).spacing(4.0).padding(8.0).background(theme.colors().surface)
    }
    pub(super) fn confirmation_view(&self) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        let save = self.clone();
        let discard = self.clone();
        let cancel = self.clone();
        vstack! {
            Text::from_state(self.confirmation.clone()).font_size(18.0),
            Text::new("Your changes will be lost if you discard them.").font_size(13.0),
            Text::from_state(self.status.clone()).font_size(11.0).color(theme.colors().foreground),
            Spacer::new(),
            hstack! {
                Button::new("Cancel").on_click(move || cancel.cancel_close()),
                Spacer::new(),
                Button::new("Discard").on_click(move || discard.discard_close()),
                Button::new("Save").on_click(move || save.save()),
            }.spacing(10.0),
        }
        .alignment(Alignment::Leading)
        .spacing(14.0)
        .padding(24.0)
        .frame(f32::INFINITY, f32::INFINITY)
    }
    fn item(&self, id: &str, label: &str, shortcut: &str, action: fn(&App)) -> MenuEntry {
        let app = self.clone();
        let shortcut = if cfg!(target_os = "macos") && !matches!(id, "completion" | "go-to-line") {
            shortcut.replacen("Ctrl+", "Cmd+", 1)
        } else {
            shortcut.into()
        };
        MenuEntry::Item(
            MenuItemModel::new(id, label)
                .shortcut(shortcut)
                .on_activate(Arc::new(move || {
                    app.runtime.lock().unwrap().menu_actions.push(action);
                })),
        )
    }
    pub(super) fn menu(&self) -> MenuBarModel {
        MenuBarModel::new(vec![
            MenuItemModel::new("file", "File").children(vec![
                self.item("new", "New File", "Ctrl+N", App::new_document),
                self.item("open", "Open File…", "Ctrl+O", |a| {
                    a.picker(DialogPurpose::Open)
                }),
                self.item(
                    "open-folder",
                    "Open Folder…",
                    "Ctrl+Shift+O",
                    App::show_folder,
                ),
                MenuEntry::Separator,
                self.item("save", "Save", "Ctrl+S", App::save),
                self.item("save-all", "Save All", "Ctrl+Alt+S", App::save_all),
                self.item("save-as", "Save As…", "Ctrl+Shift+S", |a| {
                    a.picker(DialogPurpose::Save(a.active_id()))
                }),
                MenuEntry::Separator,
                self.item("close", "Close Tab", "Ctrl+W", |a| {
                    a.request_close(Some(a.active_id()))
                }),
                self.item("close-window", "Close Window", "Ctrl+Shift+W", |a| {
                    a.request_close(None)
                }),
            ]),
            MenuItemModel::new("edit", "Edit").children(vec![
                self.item("undo", "Undo", "Ctrl+Z", |a| a.undo(false)),
                self.item("redo", "Redo", "Ctrl+Shift+Z", |a| a.undo(true)),
                MenuEntry::Separator,
                self.item("cut", "Cut", "Ctrl+X", |a| a.copy(true)),
                self.item("copy", "Copy", "Ctrl+C", |a| a.copy(false)),
                self.item("paste", "Paste", "Ctrl+V", App::paste),
                self.item("select-all", "Select All", "Ctrl+A", App::select_all),
                MenuEntry::Separator,
                self.item("toggle-comment", "Toggle Line Comment", "Ctrl+/", |a| {
                    a.edit_command(super::editing_ui::Command::Comment)
                }),
                self.item("indent", "Indent Line", "Ctrl+]", |a| {
                    a.edit_command(super::editing_ui::Command::Indent)
                }),
                self.item("outdent", "Outdent Line", "Ctrl+[", |a| {
                    a.edit_command(super::editing_ui::Command::Outdent)
                }),
                self.item("move-line-up", "Move Line Up", "Alt+Up", |a| {
                    a.edit_command(super::editing_ui::Command::MoveUp)
                }),
                self.item("move-line-down", "Move Line Down", "Alt+Down", |a| {
                    a.edit_command(super::editing_ui::Command::MoveDown)
                }),
                self.item("duplicate-line-up", "Copy Line Up", "Alt+Shift+Up", |a| {
                    a.edit_command(super::editing_ui::Command::DuplicateUp)
                }),
                self.item(
                    "duplicate-line-down",
                    "Copy Line Down",
                    "Alt+Shift+Down",
                    |a| a.edit_command(super::editing_ui::Command::DuplicateDown),
                ),
                MenuEntry::Separator,
                self.item("find", "Find…", "Ctrl+F", App::show_find),
                self.item(
                    "search-project",
                    "Find in Files…",
                    "Ctrl+Shift+F",
                    App::show_project_search,
                ),
                self.item(
                    "replace",
                    "Replace…",
                    if cfg!(target_os = "macos") {
                        "Cmd+Alt+F"
                    } else {
                        "Ctrl+H"
                    },
                    App::show_find,
                ),
                self.item("find-next", "Find Next", "F3", |a| a.find(false)),
                self.item("find-previous", "Find Previous", "Shift+F3", |a| {
                    a.find(true)
                }),
            ]),
            MenuItemModel::new("view", "View").children(vec![
                self.item("quick-open", "Quick Open…", "Ctrl+P", App::show_quick_open),
                self.item(
                    "command-palette",
                    "Command Palette…",
                    "Ctrl+Shift+P",
                    App::show_commands,
                ),
                self.item("go-to-line", "Go to Line…", "Ctrl+G", App::show_go_to_line),
                MenuEntry::Separator,
                self.item("split-right", "Split Editor Right", "Ctrl+\\", |a| {
                    a.split_editor(false)
                }),
                self.item("split-down", "Split Editor Down", "Ctrl+Shift+\\", |a| {
                    a.split_editor(true)
                }),
                self.item(
                    "focus-other-group",
                    "Focus Other Editor Group",
                    "",
                    App::focus_other_group,
                ),
                self.item(
                    "close-group",
                    "Close Second Editor Group",
                    "",
                    App::close_split,
                ),
                MenuEntry::Separator,
                self.item("theme-light", "Color Theme: Light", "", |a| {
                    a.set_theme(kureha_code::theme::Theme::Light)
                }),
                self.item("theme-dark", "Color Theme: Dark", "", |a| {
                    a.set_theme(kureha_code::theme::Theme::Dark)
                }),
                MenuEntry::Separator,
                self.item(
                    "minimap",
                    if self.minimap_visible.get() {
                        "Hide Minimap"
                    } else {
                        "Show Minimap"
                    },
                    "",
                    App::toggle_minimap,
                ),
                self.item(
                    "terminal",
                    "Toggle Terminal",
                    "Ctrl+J",
                    App::toggle_terminal,
                ),
            ]),
            MenuItemModel::new("code", "Code").children(vec![
                self.item(
                    "format",
                    "Format Document",
                    "Alt+Shift+F",
                    App::format_document,
                ),
                self.item(
                    "rename",
                    "Rename Symbol in Current File…",
                    "F2",
                    App::show_rename,
                ),
                self.item(
                    "references",
                    "Find References",
                    "Shift+F12",
                    App::find_references,
                ),
                MenuEntry::Separator,
                self.item("completion", "Completion", "Ctrl+Space", |a| {
                    a.language_request("textDocument/completion")
                }),
                self.item("definition", "Go to Definition", "F12", |a| {
                    a.language_request("textDocument/definition")
                }),
                self.item("hover", "Show Hover", "", |a| {
                    a.language_request("textDocument/hover")
                }),
                self.item("problems", "Problems", "", App::show_problems),
                MenuEntry::Separator,
                self.item(
                    "restart-language-server",
                    "Restart Language Server",
                    "",
                    App::restart_language_server,
                ),
            ]),
        ])
    }
}
