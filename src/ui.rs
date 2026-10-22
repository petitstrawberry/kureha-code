//! The workbench uses ScarletUI controls; the window menu is the app menu.
use super::*;
use scarlet_ui::views::containers::ViewTuple;

#[derive(Clone)]
struct ViewCollection<V: View + Clone>(Vec<V>);
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
        let palette = ColorPalette::default();
        let keys = self.clone();
        vstack! {
            SplitView::new(self.explorer_view(), self.editor_area())
                .fraction(0.22).min_first(170.0).min_second(420.0)
                .frame(f32::INFINITY, f32::INFINITY),
            hstack! {
                Text::from_state(self.status.clone()).font_size(11.0),
                Spacer::new(),
                Text::from_state(self.position.clone()).font_size(11.0),
            }.spacing(12.0).padding(6.0).background(palette.surface_variant()),
        }
        .spacing(0.0)
        .frame(f32::INFINITY, f32::INFINITY)
        .on_event_capture(move |event| {
            let Event::Keyboard(KeyEvent::Pressed { keycode, modifiers }) = event else {
                return false;
            };
            if *keycode == KeyCode::F(3) {
                keys.find(modifiers.shift);
                return true;
            }
            if *keycode == KeyCode::Escape && keys.find_open.get() {
                keys.find_open.set(false);
                return true;
            }
            if !modifiers.primary() {
                return false;
            }
            match keycode {
                KeyCode::Char('n' | 'N') => keys.new_document(),
                KeyCode::Char('o' | 'O') if modifiers.shift => keys.show_folder(),
                KeyCode::Char('o' | 'O') => keys.picker(DialogPurpose::Open),
                KeyCode::Char('s' | 'S') if modifiers.shift => {
                    keys.picker(DialogPurpose::Save(keys.active_id()))
                }
                KeyCode::Char('s' | 'S') => keys.save(),
                KeyCode::Char('w' | 'W') if modifiers.shift => keys.request_close(None),
                KeyCode::Char('w' | 'W') => keys.request_close(Some(keys.active_id())),
                KeyCode::Char('f' | 'F' | 'h' | 'H') => keys.show_find(),
                KeyCode::Tab => keys.next_tab(modifiers.shift),
                _ => return false,
            }
            true
        })
    }
    fn editor_area(&self) -> impl View + Clone + use<> {
        let id = self.active_id();
        vstack! {
            self.tabs_view(),
            if_view!(self.find_open.get(), self.find_view(), VStack::new(())),
            self.editor_view(id),
        }
        .spacing(0.0)
        .frame(f32::INFINITY, f32::INFINITY)
    }
    fn tabs_view(&self) -> impl View + Clone + use<> {
        let palette = ColorPalette::default();
        let active = self.active_id();
        let summaries = {
            let rt = self.runtime.lock().unwrap();
            rt.workspace.documents.iter().map(|doc| {
                let saving = matches!(rt.job.as_ref().map(|job| &job.operation), Some(FileOperation::Save { id, .. }) if *id == doc.id);
                (doc.id, format!("{}{}{}", if doc.editor.dirty() { "● " } else { "" }, doc.name(), if saving { " …" } else { "" }))
            }).collect::<Vec<_>>()
        };
        let mut left = 0.0;
        let mut selected_range = (0.0, 0.0);
        let tabs = summaries.into_iter().map(|(id, label)| {
            let width = (label.chars().count() as f32 * 7.0 + 50.0).clamp(120.0, 240.0);
            if id == active { selected_range = (left, left + width); }
            left += width;
            let select = self.clone();
            let close = self.clone();
            let background = if id == active { palette.background() } else { palette.surface_variant() };
            hstack! {
                Button::new(label).header_style().font_size(13.0).padding(7.0)
                    .background_color(background).on_click(move || select.activate(id)).frame(width - 28.0, 32.0),
                Button::new("×").header_style().padding(4.0).background_color(background)
                    .on_click(move || close.request_close(Some(id))).frame(28.0, 32.0),
            }.spacing(0.0).frame(width, 32.0).background(background).key(id)
        }).collect::<Vec<_>>();
        ScrollView::new(HStack::new(ViewCollection(tabs)).spacing(0.0))
            .horizontal()
            .content_size(left, 32.0)
            .scroll_to_horizontal_range(selected_range.0, selected_range.1)
            .frame(f32::INFINITY, 34.0)
            .background(palette.surface_variant())
    }
    fn editor_view(&self, id: DocumentId) -> impl View + Clone + use<> {
        let bindings = self.bindings(id).unwrap();
        let edited = self.clone();
        let keys = self.clone();
        let selection = bindings.selection.clone();
        let editor = TextView::with_document(bindings.document, bindings.selection)
            .autofocus(true)
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
            .on_text_change(move |delta| edited.edit(id, delta));
        let editor = if let Some(stack) = &self.editor_font {
            editor.font_stack(stack.clone())
        } else {
            editor
        };
        editor
            .frame(f32::INFINITY, f32::INFINITY)
            .on_event_capture(move |event| {
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
            .key(id)
    }
    fn explorer_view(&self) -> impl View + Clone + use<> {
        let palette = ColorPalette::default();
        let open = self.clone();
        let refresh = self.clone();
        let rows = self.clone();
        let selected_color = palette.menu_hover();
        let has_root = self.runtime.lock().unwrap().explorer.root.is_some();
        vstack! {
            hstack! {
                Text::new("EXPLORER").font_size(11.0).color(palette.text_secondary()),
                Spacer::new(),
                Button::new("Open Folder").header_style().font_size(11.0).padding(5.0).on_click(move || open.show_folder()),
                Button::icon_only(Icon::Refresh).header_style().padding(5.0).enabled(has_root).on_click(move || {
                    let root = refresh.runtime.lock().unwrap().explorer.root.clone();
                    if let Some(root) = root { refresh.open_folder(root); }
                }),
            }.spacing(4.0).padding(8.0),
            if_view!(has_root,
                ListView::new(self.tree_rows.clone(), self.tree_selected.clone(), 27.0, move |index, row: Row, selected| {
                    let app = rows.clone();
                    let enabled = row.path.is_some();
                    let icon = if row.directory { if row.expanded { "▾" } else { "▸" } } else { "·" };
                    let label = format!("{}{} {}", "  ".repeat(row.depth), icon, row.label);
                    hstack! {
                        Button::new(label).header_style().font_size(13.0).padding(4.0).enabled(enabled)
                            .on_click(move || app.open_tree_row(row.clone(), index)),
                        Spacer::new(),
                    }.spacing(0.0).frame(f32::INFINITY, 27.0)
                        .background(if selected == Some(index) { selected_color } else { Color::TRANSPARENT })
                }).item_key(|row| row.key.clone().into())
                  .frame(f32::INFINITY, f32::INFINITY),
                vstack! {
                    Text::new("Open a folder to browse files.").font_size(12.0).color(palette.text_secondary()),
                    Spacer::new(),
                }.padding(12.0).frame(f32::INFINITY, f32::INFINITY)
            ),
        }.spacing(0.0).background(palette.surface_variant()).frame(f32::INFINITY, f32::INFINITY)
    }
    fn find_view(&self) -> impl View + Clone + use<> {
        let submit = self.clone();
        let previous = self.clone();
        let next = self.clone();
        let replace = self.clone();
        let all = self.clone();
        let close = self.clone();
        vstack! {
            hstack! {
                TextField::new(self.query.clone()).placeholder("Find (match case)").autofocus(true)
                    .on_submit(move || submit.find(false)).frame_width(f32::INFINITY),
                Button::new("Previous").header_style().on_click(move || previous.find(true)),
                Button::new("Next").header_style().on_click(move || next.find(false)),
                Button::new("×").header_style().on_click(move || close.find_open.set(false)),
            }.spacing(6.0),
            hstack! {
                TextField::new(self.replacement.clone()).placeholder("Replace with").frame_width(f32::INFINITY),
                Button::new("Replace").header_style().on_click(move || replace.replace_match()),
                Button::new("Replace All").header_style().on_click(move || all.replace_all()),
            }.spacing(6.0),
            Text::from_state(self.search_status.clone()).font_size(11.0),
        }.alignment(Alignment::Leading).spacing(4.0).padding(8.0).background(ColorPalette::default().surface_variant())
    }
    pub(super) fn confirmation_view(&self) -> impl View + Clone + use<> {
        let save = self.clone();
        let discard = self.clone();
        let cancel = self.clone();
        vstack! {
            Text::from_state(self.confirmation.clone()).font_size(18.0),
            Text::new("Your changes will be lost if you discard them.").font_size(13.0),
            Text::from_state(self.status.clone()).font_size(11.0),
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
                self.item("find", "Find…", "Ctrl+F", App::show_find),
                self.item("replace", "Replace…", "Ctrl+H", App::show_find),
                self.item("find-next", "Find Next", "F3", |a| a.find(false)),
                self.item("find-previous", "Find Previous", "Shift+F3", |a| {
                    a.find(true)
                }),
            ]),
        ])
    }
}
