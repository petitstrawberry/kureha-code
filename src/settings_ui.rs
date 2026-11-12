use super::preferences::Change;
use super::*;
use std::sync::OnceLock;

#[derive(Clone)]
pub(super) struct Settings {
    page: State<usize>,
    word_wrap: State<bool>,
    wrap_mode: State<usize>,
    format_on_save: State<bool>,
    license: State<usize>,
    query: State<String>,
    rows: State<Vec<usize>>,
    selected_row: State<Option<usize>>,
    last_query: Arc<Mutex<Option<String>>>,
    row_inputs: ui::RowInputs,
    license_text: State<String>,
    document: State<Arc<kureha_code::markdown::Document>>,
    position: Arc<Mutex<crate::markdown_view::PreviewPosition>>,
}
impl Settings {
    pub fn new() -> Self {
        Self {
            page: state(0),
            word_wrap: state(true),
            wrap_mode: state(0),
            format_on_save: state(false),
            license: state(0),
            query: state(String::new()),
            rows: state(Vec::new()),
            selected_row: state(None),
            last_query: Default::default(),
            row_inputs: Default::default(),
            license_text: state(String::new()),
            document: state(Arc::new(Default::default())),
            position: Arc::new(Mutex::new(Default::default())),
        }
    }
    pub fn listenables(&self) -> Vec<&dyn Listenable> {
        vec![&self.page, &self.license, &self.document]
    }
    pub(super) fn sync_preferences(&self, preferences: &preferences::Preferences) {
        if self.word_wrap.get() != preferences.word_wrap {
            self.word_wrap.set(preferences.word_wrap);
        }
        if self.format_on_save.get() != preferences.format_on_save {
            self.format_on_save.set(preferences.format_on_save);
        }
        let mode = usize::from(!preferences.wrap_at_words);
        if self.wrap_mode.get() != mode {
            self.wrap_mode.set(mode);
        }
    }
    pub(super) fn refresh_license_list(&self) {
        let query = self.query.get();
        let mut previous = self.last_query.lock().unwrap();
        if previous.as_ref() == Some(&query) {
            return;
        }
        let needle = query.trim().to_lowercase();
        let rows: Vec<_> = acknowledgements()
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.0.to_lowercase().contains(&needle))
            .map(|(index, _)| index)
            .collect();
        self.selected_row
            .set(rows.iter().position(|id| *id == self.license.get()));
        self.row_inputs
            .retain("license:", rows.iter().map(ToString::to_string));
        self.rows.set(rows);
        *previous = Some(query);
    }
    pub(super) fn show_license(&self, index: usize) {
        if let Some(entry) = acknowledgements().get(index) {
            self.license.set(index);
            self.selected_row
                .set(self.rows.get().iter().position(|id| *id == index));
            if self.license_text.get() == entry.1 {
                return;
            }
            self.license_text.set(entry.1.clone());
            use kureha_code::markdown::{Block, Document, Span};
            let blocks = entry
                .1
                .split("\n\n")
                .enumerate()
                .map(|(line, text)| Block {
                    cells: vec![vec![Span {
                        text: text.to_owned(),
                        style: Default::default(),
                    }]],
                    source_line: line,
                    ..Default::default()
                })
                .collect();
            self.document.set(Arc::new(Document {
                blocks: Arc::new(blocks),
                ..Default::default()
            }));
            *self.position.lock().unwrap() = Default::default();
        }
    }
}
fn state<T: Clone + Send + Sync + 'static>(value: T) -> State<T> {
    State::new(generate_state_id(), value)
}

fn license_row_label(text: String, color: Color) -> impl View + Clone {
    Text::new(text)
        .font_size(13.0)
        .color(color)
        .alignment(Alignment::Leading)
        .padding_insets(EdgeInsets::new(6.0, 0.0, 6.0, 0.0))
        .frame(f32::INFINITY, 32.0)
        .clip()
}

pub(super) fn acknowledgements() -> &'static Vec<(String, String)> {
    static ENTRIES: OnceLock<Vec<(String, String)>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        let catalogue: serde_json::Value =
            serde_json::from_str(include_str!("../assets/acknowledgements.json"))
                .expect("generated license catalogue");
        let texts = catalogue["texts"].as_array().unwrap();
        catalogue["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                let label = format!(
                    "{} {}",
                    entry["name"].as_str().unwrap(),
                    entry["version"].as_str().unwrap()
                )
                .trim()
                .to_owned();
                let mut body = format!(
                    "{label}\n{}\n{}\n\n",
                    entry["license"].as_str().unwrap(),
                    entry["source"].as_str().unwrap()
                );
                for index in entry["texts"].as_array().unwrap() {
                    body.push_str(texts[index.as_u64().unwrap() as usize].as_str().unwrap());
                    body.push_str("\n\n");
                }
                (label, body)
            })
            .collect()
    })
}
impl App {
    pub(super) fn is_settings_tab(&self, id: TabId) -> bool {
        self.runtime.lock().unwrap().settings_tab == Some(id)
    }
    fn show_settings_tab(&self) {
        if self.blocked() {
            return;
        }
        self.close_navigation();
        self.dismiss_language_panel();
        self.settings_ui.refresh_license_list();
        if self.settings_ui.license_text.get().is_empty() {
            self.settings_ui
                .show_license(self.settings_ui.license.get());
        }
        let (id, existing) = {
            let mut rt = self.runtime.lock().unwrap();
            let id = match rt.settings_tab {
                Some(id) => id,
                None => {
                    let id = rt.workspace.new_tab_id();
                    rt.settings_tab = Some(id);
                    id
                }
            };
            let focused = self.focused_group.get();
            let existing = if rt.group_tabs[focused].contains(&id) {
                Some(focused)
            } else {
                rt.group_tabs.iter().position(|tabs| tabs.contains(&id))
            };
            (id, existing)
        };
        self.focus_group(existing.unwrap_or(self.focused_group.get()));
        self.activate(id);
        self.invalidate_clipboard();
        self.update_position();
        self.changed();
    }
    pub(super) fn open_settings(&self) {
        self.settings_ui.page.set(0);
        self.show_settings_tab();
    }
    pub(super) fn open_acknowledgements(&self) {
        self.settings_ui.page.set(1);
        self.settings_ui
            .show_license(self.settings_ui.license.get());
        self.show_settings_tab();
    }
    pub(super) fn close_settings_tab(&self, id: TabId) -> bool {
        if !self.is_settings_tab(id) {
            return false;
        }
        if self.blocked() {
            return true;
        }
        let group = self.focused_group.get();
        let next = {
            let mut rt = self.runtime.lock().unwrap();
            let tabs = &mut rt.group_tabs[group];
            let Some(index) = tabs.iter().position(|tab| *tab == id) else {
                return true;
            };
            tabs.remove(index);
            tabs.get(index.min(tabs.len().saturating_sub(1))).copied()
        };
        if self.group_id(group) == Some(id) {
            self.panes.update(|panes| panes[group] = next);
            if next.is_none() {
                self.retire_empty_group(group);
            }
            self.focus_group_terminal(self.focused_group.get());
        }
        self.invalidate_clipboard();
        self.update_position();
        self.changed();
        true
    }
    pub(super) fn settings_view(&self, group: usize) -> impl View + Clone + use<> {
        let colors = self.theme.get().colors();
        let appearance = self.clone();
        let licenses = self.clone();
        let focus = self.clone();
        TabView::with_selected_index(
            vec![
                TabItem::new("Appearance", move || appearance.appearance_settings()),
                TabItem::new("Acknowledgements", move || licenses.license_settings()),
            ],
            self.settings_ui.page.clone(),
        )
        .tab_bar_placement(scarlet_ui::views::TabBarPlacement::Top)
        .material(self.theme.get().input_material())
        .style(scarlet_ui::views::TabStyle {
            background_color: colors.background,
            selected_color: colors.selection,
            hover_color: colors.selection.with_opacity(0.35),
            border_color: Color::TRANSPARENT,
            text_color: colors.foreground,
            selected_text_color: colors.foreground,
            indicator_color: Color::TRANSPARENT,
            indicator_height: 0.0,
        })
        .font_size(13.0)
        .padding_insets(EdgeInsets::new(0.0, 16.0, 0.0, 0.0))
        .frame(f32::INFINITY, f32::INFINITY)
        .background(colors.background)
        .on_event_capture(move |event| {
            if matches!(
                event,
                Event::Mouse(scarlet_ui::event::MouseEvent::ButtonPressed { .. })
            ) {
                focus.focus_group(group);
            }
            false
        })
    }
    fn appearance_settings(&self) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        let colors = theme.colors();
        let preferences = self.preferences.get();
        let color_app = self.clone();
        let code_app = self.clone();
        let follow = self.clone();
        let custom = self.clone();
        let smaller = self.clone();
        let larger = self.clone();
        let reset_size = self.clone();
        let minimap_app = self.clone();
        let wrap_app = self.clone();
        let wrap_mode_app = self.clone();
        let format_app = self.clone();
        let file_app = self.clone();
        let custom_app = self.clone();
        let button = |label: String| {
            Button::new(label)
                .header_style()
                .text_color(colors.foreground)
                .background_color(colors.selection)
                .font_size(13.0)
        };
        let label = |text: &str| Text::new(text).font_size(13.0).color(colors.muted);
        let size = theme.editor_font_size();
        ScrollView::new(vstack! {
            vstack! {
                label("Color theme"),
                hstack! {
                button(theme.name().into()).on_click(move || color_app.show_theme_picker(false)),
                Button::new("Use Custom Theme").header_style().text_color(colors.muted).background_color(Color::TRANSPARENT)
                    .enabled(preferences.color_theme.is_some()).on_click(move || custom.change_preferences(Change::Color(None))),
                }.spacing(12.0),
            }.alignment(Alignment::Leading).spacing(6.0),
            vstack! {
                label("Code & terminal"),
                hstack! {
                button(preferences.code_theme.clone().unwrap_or_else(|| "Follow color theme".into()))
                    .on_click(move || code_app.show_theme_picker(true)),
                Button::new("Reset").header_style().text_color(colors.muted).background_color(Color::TRANSPARENT)
                    .enabled(preferences.code_theme.is_some()).on_click(move || follow.change_preferences(Change::Code(None))),
                }.spacing(12.0),
            }.alignment(Alignment::Leading).spacing(6.0),
            vstack! {
                label("Code font size"),
                hstack! {
                button("−".into()).on_click(move || smaller.change_preferences(Change::FontSize(Some((size-1.0).max(8.0))))),
                Text::new(format!("{size} px")).font_size(13.0).color(colors.foreground),
                button("+".into()).on_click(move || larger.change_preferences(Change::FontSize(Some((size+1.0).min(48.0))))),
                Button::new("Reset").header_style().text_color(colors.muted).background_color(Color::TRANSPARENT)
                    .enabled(preferences.font_size.is_some()).on_click(move || reset_size.change_preferences(Change::FontSize(None))),
                }.spacing(12.0),
            }.alignment(Alignment::Leading).spacing(6.0),
            vstack! {
                label("Word wrap"),
                hstack! {
                    Toggle::new(self.settings_ui.word_wrap.clone())
                        .on_change(move |enabled| wrap_app.change_preferences(Change::WordWrap(enabled))),
                    Select::new(vec!["Word boundaries".into(), "Characters".into()], self.settings_ui.wrap_mode.clone())
                        .width(190.0)
                        .enabled(preferences.word_wrap)
                        .on_change(move |mode| wrap_mode_app.change_preferences(Change::WrapAtWords(mode == 0))),
                }.spacing(12.0),
            }.alignment(Alignment::Leading).spacing(6.0),
            vstack! {
                label("Format on save"),
                hstack! {
                Toggle::new(self.settings_ui.format_on_save.clone())
                    .on_change(move |enabled| format_app.change_preferences(Change::FormatOnSave(enabled))),
                }.spacing(12.0),
            }.alignment(Alignment::Leading).spacing(6.0),
            vstack! {
                label("Minimap"),
                hstack! {
                Toggle::new(self.minimap_visible.clone())
                    .on_change(move |visible| minimap_app.change_preferences(Change::Minimap(visible))),
                }.spacing(12.0),
            }.alignment(Alignment::Leading).spacing(6.0),
            hstack! {
                Button::new("Open Settings File…").header_style().text_color(colors.accent).background_color(Color::TRANSPARENT)
                    .on_click(move || file_app.open_preferences_file()),
                Button::new("Edit Custom Theme…").header_style().text_color(colors.accent).background_color(Color::TRANSPARENT)
                    .on_click(move || custom_app.open_theme_settings()),
            }.spacing(8.0),
        }.alignment(Alignment::Leading).spacing(16.0).padding(20.0)).frame(f32::INFINITY, f32::INFINITY)
    }
    fn license_settings(&self) -> impl View + Clone + use<> {
        let colors = self.theme.get().colors();
        let rows = self.settings_ui.clone();
        let submit = self.settings_ui.clone();
        let copy = self.clone();
        let list = ListView::new(
            self.settings_ui.rows.clone(),
            self.settings_ui.selected_row.clone(),
            32.0,
            move |index, id, selected| {
                let select = rows.clone();
                ui::list_row(
                    license_row_label(acknowledgements()[id].0.clone(), colors.foreground),
                    rows.row_inputs.get(format!("license:{id}")),
                    selected == Some(index),
                    true,
                    colors,
                    move || select.show_license(id),
                )
            },
        )
        .item_key(|id| (*id as u64).into())
        .frame(f32::INFINITY, f32::INFINITY);
        let browser = vstack! {
            TextField::new(self.settings_ui.query.clone()).placeholder("Search")
                .font_size(13.0).text_color(colors.foreground).placeholder_color(colors.muted)
                .background_color(colors.elevated).border_color(colors.border).focused_border_color(colors.accent)
                .material(self.theme.get().input_material()).on_submit(move || {
                    submit.refresh_license_list();
                    if let Some(id) = submit.rows.get().first().copied() { submit.show_license(id); }
                }).frame(f32::INFINITY, 32.0),
            list,
        }.spacing(8.0).frame(f32::INFINITY, f32::INFINITY);

        let text = ScrollView::new(crate::markdown_view::MarkdownView {
            document: self.settings_ui.document.get(),
            theme: self.theme.get(),
            font: self.document_font(),
            position: self.settings_ui.position.clone(),
            on_link: Arc::new(|_| {}),
            on_scroll: Arc::new(|_| {}),
        })
        .scrollbar_color(colors.muted)
        .frame(f32::INFINITY, f32::INFINITY)
        .clip()
        .key(format!("license-body-{}", self.settings_ui.license.get()));
        let detail = vstack! {
            hstack! {
                Spacer::new(),
                Button::new("Copy").header_style().text_color(colors.foreground).background_color(colors.selection).on_click(move || {
                    if let Some(service)=copy.clipboard_service() {
                        let handle=service.write_text(copy.settings_ui.license_text.get());
                        copy.runtime.lock().unwrap().clipboard_job=Some(ClipboardJob::TextCopy {handle});
                    }
                }),
            },
            text,
        }.spacing(8.0).frame(f32::INFINITY,f32::INFINITY);
        SplitView::new(browser, detail)
            .fraction(0.32)
            .min_first(140.0)
            .min_second(160.0)
            .divider_thickness(12.0)
            .divider_hit_slop(0.0)
            .divider_colors(Color::TRANSPARENT, colors.selection)
            .padding(16.0)
            .frame(f32::INFINITY, f32::INFINITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scarlet_ui::element::LayoutConstraints;
    use scarlet_ui::event::KeyEvent;
    use scarlet_ui::pipeline::RenderingPipeline;
    use scarlet_ui::renderer::{CpuPaintRenderer, PaintContext};

    #[test]
    fn appearance_switches_update_preferences_and_follow_external_changes() {
        use scarlet_ui::event::{MouseButton, MouseEvent};
        use scarlet_ui::views::ToggleRenderObject;
        fn switches(element: &dyn Element, parent: Point, points: &mut Vec<Point>) {
            let origin = Point::new(
                parent.x + element.position().x,
                parent.y + element.position().y,
            );
            if let Some(render) = element.render_object() {
                if render.as_any().is::<ToggleRenderObject>() {
                    points.push(Point::new(
                        origin.x + render.size().width / 2.0,
                        origin.y + render.size().height / 2.0,
                    ));
                }
            }
            for child in element.children() {
                switches(child.as_ref(), origin, points);
            }
        }
        let app = App::new();
        app.open_settings();
        let mut pipeline = RenderingPipeline::new();
        let window = || {
            Window::new("Settings", app.content())
                .decorated(false)
                .size(Size::new(1100.0, 760.0))
        };
        pipeline.set_root(window().create_element());
        pipeline.layout_initial();
        let mut points = Vec::new();
        switches(
            pipeline.element_tree().root().unwrap(),
            Point::new(0.0, 0.0),
            &mut points,
        );
        // Word wrap, format on save, minimap.
        assert_eq!(points.len(), 3);
        for (index, point) in points.iter().enumerate() {
            let (x, y) = (point.x as i32, point.y as i32);
            pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonPressed {
                button: MouseButton::Left,
                click_count: 1,
                x,
                y,
            }));
            pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonReleased {
                button: MouseButton::Left,
                click_count: 1,
                x,
                y,
            }));
            match index {
                0 => assert!(!app.preferences.get().word_wrap),
                1 => assert!(app.preferences.get().format_on_save),
                _ => assert!(app.preferences.get().hide_minimap),
            }
            pipeline
                .element_tree_mut()
                .root_mut()
                .unwrap()
                .update(&window());
            pipeline.layout_initial();
        }
        app.change_preferences(Change::WordWrap(true));
        app.change_preferences(Change::WrapAtWords(false));
        app.change_preferences(Change::Minimap(true));
        app.change_preferences(Change::FormatOnSave(false));
        assert!(!app.settings_ui.format_on_save.get());
        assert!(app.settings_ui.word_wrap.get());
        assert_eq!(app.settings_ui.wrap_mode.get(), 1);
        assert!(app.minimap_visible.get());
        pipeline.teardown();
    }

    fn label_bounds(element: &dyn Element, parent: Point) -> Rect {
        let origin = Point::new(
            parent.x + element.position().x,
            parent.y + element.position().y,
        );
        if let Some(render) = element.render_object() {
            if render.as_any().is::<scarlet_ui::views::TextRenderObject>() {
                return Rect::new(origin, render.size());
            }
        }
        for child in element.children() {
            let bounds = label_bounds(child.as_ref(), origin);
            if bounds.size.height > 0.0 {
                return bounds;
            }
        }
        Rect::from_xywh(0.0, 0.0, 0.0, 0.0)
    }

    fn paint_label<'a>(element: &'a dyn Element, parent: Point, ctx: &mut PaintContext<'a>) {
        let origin = Point::new(
            parent.x + element.position().x,
            parent.y + element.position().y,
        );
        if let Some(render) = element.render_object() {
            if render.as_any().is::<scarlet_ui::views::TextRenderObject>() {
                render.paint(ctx, origin);
            }
        }
        for child in element.children() {
            paint_label(child.as_ref(), origin, ctx);
        }
    }

    #[test]
    fn license_labels_center_the_measured_line_including_mixed_japanese() {
        for label in [
            "ab_glyph 0.2.32",
            "日本語のライセンス",
            "ab_glyph 日本語 0.2.32",
            "Agjpqy É e\u{301}",
        ] {
            let mut element = license_row_label(label.into(), Color::BLACK).create_element();
            element.layout(LayoutConstraints::tight(300.0, 32.0));
            let bounds = label_bounds(element.as_ref(), Point::ZERO);
            assert!(bounds.size.height > 0.0);
            assert_eq!(bounds.origin.x, 6.0, "{label}");
            assert!(
                (bounds.origin.y + bounds.size.height * 0.5 - 16.0).abs() < 0.01,
                "{label}: {bounds:?}"
            );
        }
    }

    /// Actual Text render objects, with guides derived from their layout.
    /// Set KUREHA_SETTINGS_PREVIEW to retain the before/after images and measurements.
    #[test]
    #[ignore = "manual baseline and mixed-script visual inspection"]
    fn license_alignment_guides() {
        use scarlet_ui::graphics::font_metrics_sized;
        let dir = PathBuf::from(std::env::var_os("KUREHA_SETTINGS_PREVIEW").unwrap());
        std::fs::create_dir_all(&dir).unwrap();
        let mut measurements =
            String::from("version,label,row_center,text_top,text_bottom,baseline\n");
        for fixed in [false, true] {
            let version = if fixed { "after" } else { "before" };
            let mut elements = Vec::new();
            for (index, label) in [
                "ab_glyph 0.2.32",
                "日本語のライセンス",
                "ab_glyph 日本語 0.2.32",
                "Agjpqy É e\u{301}",
            ]
            .iter()
            .enumerate()
            {
                let mut element = if fixed {
                    license_row_label((*label).into(), Color::rgb(80, 60, 74)).create_element()
                } else {
                    Text::new(*label)
                        .font_size(13.0)
                        .color(Color::rgb(80, 60, 74))
                        .padding(6.0)
                        .frame(f32::INFINITY, 32.0)
                        .clip()
                        .create_element()
                };
                element.layout(LayoutConstraints::tight(300.0, 32.0));
                let origin = Point::new(16.0, 24.0 + index as f32 * 48.0);
                let bounds = label_bounds(element.as_ref(), origin);
                measurements.push_str(&format!(
                    "{version},{label},{},{},{},{}\n",
                    origin.y + 16.0,
                    bounds.origin.y,
                    bounds.origin.y + bounds.size.height,
                    bounds.origin.y + font_metrics_sized(13.0).ascent
                ));
                elements.push((element, origin, bounds));
            }
            for guides in [false, true] {
                let mut ctx = PaintContext::new();
                for (element, origin, bounds) in &elements {
                    let row = Rect::new(*origin, Size::new(300.0, 32.0));
                    ctx.fill_rounded_rect(row, 7.0, Color::rgb(238, 196, 217));
                    paint_label(element.as_ref(), *origin, &mut ctx);
                    if guides {
                        // Green: typographic layout box. Blue: row center. Red: baseline.
                        ctx.stroke_rect(*bounds, 0.5, Color::rgb(30, 150, 70));
                        for (y, color) in [
                            (origin.y + 16.0, Color::rgb(0, 100, 220)),
                            (
                                bounds.origin.y + font_metrics_sized(13.0).ascent,
                                Color::rgb(210, 45, 35),
                            ),
                        ] {
                            ctx.draw_line(
                                Point::new(origin.x, y),
                                Point::new(origin.x + 300.0, y),
                                0.5,
                                color,
                            );
                        }
                    }
                }
                for scale in [1000, 2000] {
                    let mut renderer =
                        CpuPaintRenderer::new(Size::new(332.0, 224.0), scale, Color::WHITE);
                    renderer.execute(&ctx);
                    let buffer = renderer.buffer();
                    let rgb: Vec<u8> = buffer
                        .data()
                        .chunks_exact(4)
                        .flat_map(|p| [p[2], p[1], p[0]])
                        .collect();
                    image::save_buffer(
                        dir.join(format!(
                            "alignment-{version}-{}-{scale}.png",
                            if guides { "guides" } else { "plain" }
                        )),
                        &rgb,
                        buffer.width(),
                        buffer.height(),
                        image::ColorType::Rgb8,
                    )
                    .unwrap();
                }
            }
        }
        std::fs::write(dir.join("alignment.csv"), measurements).unwrap();
    }
    #[test]
    fn settings_menu_uses_the_existing_application_menu_contract() {
        let app = App::new();
        for menu in [app.menu()] {
            let application = menu
                .items()
                .iter()
                .find(|item| item.id() == "__app__")
                .unwrap();
            let settings = application
                .children_value()
                .iter()
                .find_map(|entry| match entry {
                    MenuEntry::Item(item) if item.id() == "settings" => Some(item),
                    _ => None,
                })
                .unwrap();
            assert_eq!(settings.title(), "Settings…");
            assert_eq!(
                settings.shortcut_value(),
                Some(if cfg!(target_os = "macos") {
                    "Cmd+,"
                } else {
                    "Ctrl+,"
                })
            );
        }
    }
    #[test]
    fn settings_reuses_a_tab_and_moves_and_closes_independently_of_documents() {
        let app = App::with_document();
        let document = app.document_id();
        app.open_settings();
        let settings = app.group_id(0).unwrap();
        assert_ne!(settings, document);
        assert!(app.active_id().is_none());
        app.open_settings();
        app.open_acknowledgements();
        assert_eq!(
            app.runtime.lock().unwrap().group_tabs[0],
            vec![document, settings]
        );
        assert_eq!(app.runtime.lock().unwrap().workspace.documents.len(), 1);
        app.split_editor(false);
        let right = app.focused_group.get();
        assert_eq!(app.group_id(right), Some(settings));
        app.close_active();
        assert_eq!(app.pane_order(), vec![0]);
        assert_eq!(app.group_id(0), Some(settings));
        app.activate(document);
        app.split_editor(true);
        let bottom = app.focused_group.get();
        app.move_tab_internal(settings, 0, bottom, usize::MAX, false);
        app.focus_group(0);
        app.open_settings();
        assert_eq!(app.focused_group.get(), bottom);
        assert_eq!(app.group_id(bottom), Some(settings));
        app.save(); // Settings is not a text document and never prompts to save.
        assert!(app.runtime.lock().unwrap().dialog.is_none());
        app.close_active();
        assert_eq!(app.group_id(bottom), Some(document));
        app.close_active();
        app.close_active();
        assert!(app.runtime.lock().unwrap().workspace.documents.is_empty());
        app.open_settings();
        assert!(app.runtime.lock().unwrap().workspace.documents.is_empty());
        app.close_active();
        assert!(app.panes.get().iter().all(Option::is_none));
    }
    #[test]
    fn settings_shortcuts_work_without_an_open_document() {
        use scarlet_ui::event::KeyModifiers;
        let app = App::new();
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_root(
            Window::new("Kureha Code", app.content())
                .decorated(false)
                .size(Size::new(1100.0, 760.0))
                .create_element(),
        );
        pipeline.layout_initial();
        let mut modifiers = KeyModifiers::empty();
        if cfg!(target_os = "macos") {
            modifiers.super_key = true;
        } else {
            modifiers.control = true;
        }
        pipeline.handle_event(&Event::Keyboard(KeyEvent::Pressed {
            keycode: KeyCode::Char(','),
            modifiers,
        }));
        assert!(app.group_id(0).is_some_and(|id| app.is_settings_tab(id)));
        pipeline.set_root(
            Window::new("Kureha Code", app.content())
                .decorated(false)
                .size(Size::new(1100.0, 760.0))
                .create_element(),
        );
        pipeline.layout_initial();
        pipeline.handle_event(&Event::Keyboard(KeyEvent::Pressed {
            keycode: KeyCode::Char('w'),
            modifiers,
        }));
        assert_eq!(app.group_id(0), None);
        pipeline.teardown();
    }
    #[test]
    fn inline_license_filter_keeps_notice_identity_when_rows_change() {
        let app = App::new();
        app.open_settings();
        let settings = &app.settings_ui;
        assert!(!settings.document.get().blocks.is_empty());
        settings.query.set("Monokai".into());
        settings.refresh_license_list();
        let rows = settings.rows.get();
        assert_eq!(rows.len(), 1);
        let id = rows[0];
        settings.show_license(id);
        let document = settings.document.get();
        assert_eq!(settings.selected_row.get(), Some(0));
        settings.query.set(String::new());
        settings.refresh_license_list();
        assert_eq!(settings.license.get(), id);
        assert_eq!(
            settings.rows.get()[settings.selected_row.get().unwrap()],
            id
        );
        settings.show_license(id);
        assert!(Arc::ptr_eq(&document, &settings.document.get()));
        settings.query.set("no-such-dependency".into());
        settings.refresh_license_list();
        assert!(settings.rows.get().is_empty());
        assert_eq!(settings.selected_row.get(), None);
        assert!(Arc::ptr_eq(&document, &settings.document.get()));
        assert!(!app.nav.is_open());
    }
    #[test]
    fn license_page_blocks_typing_without_changing_the_editor() {
        use scarlet_ui::event::{MouseButton, MouseEvent};
        let app = App::with_document();
        let id = app.document_id();
        app.open_acknowledgements();
        let original = app.settings_ui.license_text.get();
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_root(
            Window::new("Kureha Code", app.content())
                .decorated(false)
                .size(Size::new(1100.0, 760.0))
                .create_element(),
        );
        pipeline.layout_initial();
        pipeline.render().unwrap();
        pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonPressed {
            button: MouseButton::Left,
            click_count: 1,
            x: 800,
            y: 260,
        }));
        pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonReleased {
            button: MouseButton::Left,
            click_count: 1,
            x: 800,
            y: 260,
        }));
        pipeline.handle_event(&Event::Keyboard(KeyEvent::Char { c: 'X' }));
        pipeline.handle_event(&Event::TextInputCommit {
            context_id: 0,
            serial: 1,
            text: "日本語".into(),
        });
        assert_eq!(app.settings_ui.license_text.get(), original);
        assert!(app.active_id().is_none());
        assert_eq!(
            app.runtime
                .lock()
                .unwrap()
                .workspace
                .get(id)
                .unwrap()
                .editor
                .document
                .as_str(),
            ""
        );
        pipeline.teardown();
    }
    #[test]
    #[ignore = "manual native settings preview"]
    fn settings_preview() {
        let dir = PathBuf::from(std::env::var_os("KUREHA_SETTINGS_PREVIEW").unwrap());
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["kureha", "monokai"] {
            let app = App::new();
            app.set_theme(kureha_code::theme::preset(name).unwrap());
            app.open_settings();
            for page in [0, 1] {
                app.settings_ui.page.set(page);
                app.settings_ui.show_license(0);
                let mut pipeline = RenderingPipeline::new();
                pipeline.set_root(
                    Window::new("Kureha Code", app.content())
                        .decorated(false)
                        .size(Size::new(1100.0, 760.0))
                        .create_element(),
                );
                pipeline.layout_initial();
                let buffer = pipeline.render().unwrap();
                let rgb: Vec<u8> = buffer
                    .data()
                    .chunks_exact(4)
                    .flat_map(|p| [p[2], p[1], p[0]])
                    .collect();
                image::save_buffer(
                    dir.join(format!("settings-{name}-{page}.png")),
                    &rgb,
                    buffer.width(),
                    buffer.height(),
                    image::ColorType::Rgb8,
                )
                .unwrap();
                pipeline.teardown();
            }
        }
    }
}
