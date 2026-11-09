//! Retained native Markdown layout; rebuilding or scrolling never reparses text.
use super::*;
use kureha_code::{
    markdown::{Block, Document, Kind, Span, Style},
    theme::Theme,
};
use scarlet_ui::{
    element::{RenderElement, UpdateResult},
    event::{MouseButton, MouseEvent, Phase},
    graphics,
    renderer::PaintContext,
};
use std::any::Any;

#[derive(Clone)]
pub(super) struct MarkdownView {
    pub document: Arc<Document>,
    pub theme: Theme,
    pub font: Option<FontStack>,
    pub on_link: Arc<dyn Fn(String) + Send + Sync>,
    pub scroll_target: State<Option<usize>>,
}
#[derive(Clone)]
struct Run {
    text: String,
    rect: Rect,
    size: f32,
    mono: bool,
    bold: bool,
    style: Style,
}
#[derive(Default)]
struct Layout {
    size: Size,
    runs: Vec<Run>,
    panels: Vec<(Rect, Color)>,
    advances: BTreeMap<(bool, u32, char), f32>,
    anchors: BTreeMap<String, usize>,
    images: Vec<(Rect, Arc<scarlet_ui::buffer::Buffer>, String)>,
}
fn measure(text: &str, size: f32, font: Option<&FontStack>) -> f32 {
    font.map_or_else(
        || graphics::measure_text_sized(text, size).0,
        |font| graphics::measure_text_sized_with_font_stack(text, size, font).0,
    ) as f32
}
impl MarkdownView {
    fn layout(&self, width: f32) -> Layout {
        let width = width.max(1.0);
        let page = (width - 40.0).clamp(1.0, 840.0);
        let left = ((width - page) * 0.5).max(0.0);
        let mut result = Layout {
            size: Size::new(width, 40.0),
            ..Layout::default()
        };
        let mut top = 22.0;
        let colors = self.theme.editor_colors();
        for block in &self.document.blocks {
            let indent = (block.indent.min(8) as f32 * 20.0 + block.quote.min(8) as f32 * 14.0)
                .min(page * 0.4);
            let x = left + indent;
            let available = (page - indent).max(1.0);
            let start = top;
            if matches!(block.kind, Kind::Heading(_)) {
                let slug = block
                    .text()
                    .to_lowercase()
                    .chars()
                    .filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '-' || *c == '_')
                    .map(|c| if c.is_whitespace() { '-' } else { c })
                    .collect::<String>();
                let mut key = slug.clone();
                let mut duplicate = 1;
                while result.anchors.contains_key(&key) {
                    key = format!("{slug}-{duplicate}");
                    duplicate += 1;
                }
                result.anchors.insert(key, top as usize);
            }
            if block.kind == Kind::Rule {
                result
                    .panels
                    .push((Rect::from_xywh(x, top + 8.0, available, 1.0), colors.border));
                top += 25.0;
                continue;
            }
            let code = matches!(block.kind, Kind::Code(_));
            let table = matches!(block.kind, Kind::TableRow(_));
            let size = match block.kind {
                Kind::Heading(1) => 28.0,
                Kind::Heading(2) => 23.0,
                Kind::Heading(_) => 18.0,
                Kind::Code(_) => 13.0,
                _ => 15.0,
            };
            let inset = if code || table { 10.0 } else { 0.0 };
            if code {
                top += 10.0;
            }
            if !block.marker.is_empty() {
                result.runs.push(Run {
                    text: block.marker.clone(),
                    rect: Rect::from_xywh(x - 20.0, top, 20.0, size + 8.0),
                    size,
                    mono: false,
                    bold: false,
                    style: Style::default(),
                });
            }
            let columns = block.cells.len().max(1);
            let column_width = available / columns as f32;
            let mut bottom = top + size + 8.0;
            for (index, cell) in block.cells.iter().enumerate() {
                bottom = bottom.max(self.wrap(
                    cell,
                    block,
                    x + index as f32 * column_width + inset,
                    top,
                    (column_width - inset * 2.0).max(1.0),
                    size,
                    &mut result,
                ));
            }
            top = bottom + if code || table { 10.0 } else { 0.0 };
            if code || table {
                let fill = if code {
                    colors.current_line
                } else if block.kind == Kind::TableRow(true) {
                    colors.selection.with_opacity(0.35)
                } else {
                    colors.current_line.with_opacity(0.5)
                };
                result
                    .panels
                    .push((Rect::from_xywh(x, start, available, top - start), fill));
                if table {
                    result
                        .panels
                        .push((Rect::from_xywh(x, top - 1.0, available, 1.0), colors.border));
                    for col in 1..columns {
                        result.panels.push((
                            Rect::from_xywh(x + col as f32 * column_width, start, 1.0, top - start),
                            colors.border,
                        ));
                    }
                }
            }
            if block.quote > 0 {
                result.panels.push((
                    Rect::from_xywh(x - 10.0, start, 3.0, top - start),
                    self.theme.colors().accent.with_opacity(0.5),
                ));
            }
            top += if table { 0.0 } else { 14.0 };
        }
        result.size.height = top + 24.0;
        result
    }
    fn wrap(
        &self,
        spans: &[Span],
        block: &Block,
        left: f32,
        top: f32,
        width: f32,
        size: f32,
        layout: &mut Layout,
    ) -> f32 {
        let code = matches!(block.kind, Kind::Code(_));
        let mut x = 0.0;
        let mut y = top;
        let mono_height = self.font.as_ref().map_or(0.0, |f| {
            graphics::line_height_sized_with_font_stack(size, f) as f32
        });
        let line_height = (graphics::line_height_sized(size) as f32).max(mono_height) + 6.0;
        for span in spans {
            if let Some(destination) = &span.style.image {
                if x > 0.0 {
                    y += line_height;
                }
                x = 0.0;
                if let Some(Ok(buffer)) = self.document.images.get(destination).map(|r| &r.result) {
                    let scale = (width / buffer.width() as f32).min(1.0);
                    let rect = Rect::from_xywh(
                        left,
                        y,
                        buffer.width() as f32 * scale,
                        buffer.height() as f32 * scale,
                    );
                    layout.images.push((
                        rect,
                        buffer.clone(),
                        span.style
                            .link
                            .clone()
                            .unwrap_or_else(|| destination.clone()),
                    ));
                    y += rect.size.height + 10.0;
                } else {
                    let reason = self
                        .document
                        .images
                        .get(destination)
                        .and_then(|r| r.result.as_ref().err())
                        .map(String::as_str)
                        .unwrap_or("Loading image…");
                    let fallback = vec![Span {
                        text: format!("{} — {}", span.text, reason),
                        style: Style {
                            link: Some(destination.clone()),
                            ..Style::default()
                        },
                    }];
                    y = self.wrap(&fallback, block, left, y, width, size, layout) + 8.0;
                }
                continue;
            }
            let mono = code || span.style.code;
            let font = if mono { self.font.as_ref() } else { None };
            // Word tokens keep normal prose wrapping; long identifiers break at UTF-8 boundaries.
            for token in span.text.split_inclusive(|c: char| c.is_whitespace()) {
                let newline = token.ends_with('\n');
                let token = token.trim_end_matches('\n');
                let expanded;
                let mut rest = if token.contains('\t') {
                    expanded = token.replace('\t', "    ");
                    expanded.as_str()
                } else {
                    token
                };
                let sample_end = rest.char_indices().nth(128).map_or(rest.len(), |(i, _)| i);
                if x > 0.0 && x + measure(&rest[..sample_end], size, font) > width {
                    x = 0.0;
                    y += line_height;
                }
                while !rest.is_empty() {
                    if !code && x == 0.0 {
                        rest = rest.trim_start();
                        if rest.is_empty() {
                            break;
                        }
                    }
                    let mut end = 0;
                    let mut advance = 0.0;
                    for (i, ch) in rest.char_indices() {
                        let width_of_char = *layout
                            .advances
                            .entry((mono, size.to_bits(), ch))
                            .or_insert_with(|| {
                                let mut bytes = [0u8; 4];
                                measure(ch.encode_utf8(&mut bytes), size, font)
                            });
                        if end > 0 && advance + width_of_char > width - x {
                            break;
                        }
                        advance += width_of_char;
                        end = i + ch.len_utf8();
                    }
                    let text = &rest[..end];
                    let w = measure(text, size, font);
                    layout.runs.push(Run {
                        text: text.into(),
                        rect: Rect::from_xywh(left + x, y, w, line_height),
                        size,
                        mono,
                        bold: span.style.strong
                            || matches!(block.kind, Kind::Heading(_) | Kind::TableRow(true)),
                        style: span.style.clone(),
                    });
                    x += w;
                    rest = &rest[end..];
                    if !rest.is_empty() {
                        x = 0.0;
                        y += line_height;
                    }
                }
                if newline {
                    x = 0.0;
                    y += line_height;
                }
            }
        }
        y + line_height
    }
}
impl View for MarkdownView {
    fn create_element(&self) -> Box<dyn Element> {
        Box::new(RenderElement::with_view_children_and_updater(
            self.clone(),
            |view| Renderer {
                view: view.clone(),
                layout: Layout::default(),
                valid: false,
                pressed: None,
            },
            |render, view| {
                let same = Arc::ptr_eq(&render.view.document, &view.document)
                    && render.view.theme == view.theme
                    && render.view.font.as_ref().map(FontStack::cache_id)
                        == view.font.as_ref().map(FontStack::cache_id);
                render.view = view.clone();
                if !same {
                    render.valid = false;
                    render.pressed = None;
                }
                UpdateResult::Updated
            },
            |_| Vec::new(),
        ))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
struct Renderer {
    view: MarkdownView,
    layout: Layout,
    valid: bool,
    pressed: Option<String>,
}
impl Renderer {
    fn link(&self, x: i32, y: i32) -> Option<String> {
        if let Some((_, _, link)) = self
            .layout
            .images
            .iter()
            .find(|(rect, _, _)| rect.contains(Point::new(x as f32, y as f32)))
        {
            return Some(link.clone());
        }
        self.layout
            .runs
            .iter()
            .find(|run| {
                run.rect.contains(Point::new(x as f32, y as f32)) && run.style.link.is_some()
            })
            .and_then(|r| r.style.link.clone())
    }
}
impl ElementRenderObject for Renderer {
    fn layout(&mut self, c: LayoutConstraints) -> Size {
        let width = if c.max_width.is_finite() {
            c.max_width
        } else {
            840.0
        };
        if !self.valid || self.layout.size.width != width {
            self.layout = self.view.layout(width);
            self.valid = true;
        }
        self.layout.size
    }
    fn update_needs_layout(&self) -> bool {
        !self.valid
    }
    fn size(&self) -> Size {
        self.layout.size
    }
    fn render(&mut self) {}
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn hit_test(&self, p: Point) -> bool {
        Rect::new(Point::ZERO, self.size()).contains(p)
    }
    fn handle_event(&mut self, event: &Event, phase: Phase) -> bool {
        if phase != Phase::Target {
            return false;
        }
        match event {
            Event::Mouse(MouseEvent::ButtonPressed {
                button: MouseButton::Left,
                x,
                y,
                ..
            }) => {
                self.pressed = self.link(*x, *y);
                self.pressed.is_some()
            }
            Event::Mouse(MouseEvent::ButtonReleased {
                button: MouseButton::Left,
                x,
                y,
                ..
            }) => {
                if let Some(link) = self
                    .pressed
                    .take()
                    .filter(|link| Some(link.clone()) == self.link(*x, *y))
                {
                    if let Some(anchor) = link
                        .strip_prefix('#')
                        .and_then(|a| kureha_code::markdown_resources::decode_path(a).ok())
                        .and_then(|a| self.layout.anchors.get(&a))
                    {
                        self.view.scroll_target.set(Some(*anchor));
                    } else {
                        (self.view.on_link)(link);
                    }
                    return true;
                }
                false
            }
            Event::Mouse(MouseEvent::Wheel { .. }) => {
                self.pressed = None;
                false
            }
            _ => false,
        }
    }
    fn paint<'a>(&'a self, ctx: &mut PaintContext<'a>, origin: Point) -> bool {
        let colors = self.view.theme.editor_colors();
        ctx.push_clip(Rect::new(origin, self.size()));
        for (rect, color) in &self.layout.panels {
            ctx.fill_rect(
                Rect::from_xywh(
                    origin.x + rect.origin.x,
                    origin.y + rect.origin.y,
                    rect.size.width,
                    rect.size.height,
                ),
                *color,
            );
        }
        for (rect, buffer, _) in &self.layout.images {
            ctx.draw_buffer_rect_shared(
                Rect::from_xywh(
                    origin.x + rect.origin.x,
                    origin.y + rect.origin.y,
                    rect.size.width,
                    rect.size.height,
                ),
                Rect::from_xywh(0.0, 0.0, buffer.width() as f32, buffer.height() as f32),
                buffer.clone(),
                1.0,
            );
        }
        for run in &self.layout.runs {
            let point = Point::new(origin.x + run.rect.origin.x, origin.y + run.rect.origin.y);
            let color = run.style.color.unwrap_or_else(|| {
                if run.style.link.is_some() || run.style.emphasis {
                    self.view.theme.colors().accent
                } else {
                    colors.foreground
                }
            });
            if run.style.code && run.style.color.is_none() {
                ctx.fill_rect(Rect::new(point, run.rect.size), colors.current_line);
            }
            let mut draw = |point| {
                if let Some(font) = self.view.font.as_ref().filter(|_| run.mono) {
                    ctx.draw_text_with_font_stack(
                        point,
                        run.text.clone(),
                        color,
                        run.size,
                        Arc::new(font.clone()),
                    );
                } else {
                    ctx.draw_text(point, run.text.clone(), color, run.size);
                }
            };
            draw(point);
            if run.bold {
                draw(Point::new(point.x + 0.5, point.y));
            }
            if run.style.link.is_some() || run.style.strike {
                let y = point.y
                    + if run.style.strike {
                        run.rect.size.height * 0.45
                    } else {
                        run.rect.size.height - 4.0
                    };
                ctx.fill_rect(
                    Rect::from_xywh(point.x, y, run.rect.size.width, 1.0),
                    color.with_opacity(0.6),
                );
            }
        }
        ctx.pop_clip();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scarlet_ui::{
        event::{ScrollSource, WheelPhase},
        pipeline::RenderingPipeline,
    };
    fn view(source: &str) -> MarkdownView {
        MarkdownView {
            document: Arc::new(kureha_code::markdown::parse(source, &Theme::Light)),
            theme: Theme::Light,
            font: kureha_code::fonts::editor_stack(),
            on_link: Arc::new(|_| {}),
            scroll_target: State::new(generate_state_id(), None),
        }
    }
    #[test]
    fn native_layout_wraps_unicode_tables_and_images_at_narrow_widths() {
        let mut view = view(
            "# Native preview\n\n日本語と **bold**、*emphasis*。\n\n| A | B |\n|---|---|\n| long-word | 日本語 |\n\n![diagram](image.png)\n",
        );
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/screenshots/panel-bottom.png");
        Arc::make_mut(&mut view.document).images.insert(
            "image.png".into(),
            kureha_code::markdown_resources::load(path.to_str().unwrap(), None, None),
        );
        for width in [180.0, 320.0, 840.0] {
            let layout = view.layout(width);
            assert_eq!(layout.images.len(), 1);
            for (rect, _, _) in &layout.images {
                assert!(rect.right() <= width && rect.left() >= 0.0);
            }
            for run in &layout.runs {
                assert!(
                    run.rect.right() <= width + 1.0,
                    "outside preview: {} {:?}",
                    run.text,
                    run.rect
                );
            }
            assert!(layout.size.height.is_finite());
        }
    }
    #[test]
    fn retained_layout_reuses_geometry_and_long_unicode_words_make_progress() {
        let source = "あ".repeat(20000);
        let view = view(&source);
        let started = std::time::Instant::now();
        let layout = view.layout(320.0);
        assert_eq!(
            layout.runs.iter().map(|r| r.text.len()).sum::<usize>(),
            source.len()
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let mut renderer = Renderer {
            view,
            layout,
            valid: true,
            pressed: None,
        };
        let pointer = renderer.layout.runs.as_ptr();
        renderer.layout(LayoutConstraints::new(0.0, 320.0, 0.0, f32::INFINITY));
        assert_eq!(renderer.layout.runs.as_ptr(), pointer);
    }
    #[test]
    fn heading_links_resolve_unicode_and_duplicate_headings() {
        let view = view("[Go](#日本語-1)\n\n# 日本語\n\nFirst\n\n# 日本語\n");
        let layout = view.layout(320.0);
        let target = layout.anchors["日本語-1"];
        assert!(target > layout.anchors["日本語"]);
        let run = layout.runs.iter().find(|r| r.style.link.is_some()).unwrap();
        let point = run.rect.origin;
        let request = view.scroll_target.clone();
        let mut renderer = Renderer {
            view,
            layout,
            valid: true,
            pressed: None,
        };
        let press = Event::Mouse(MouseEvent::ButtonPressed {
            button: MouseButton::Left,
            x: point.x as i32 + 1,
            y: point.y as i32 + 1,
            click_count: 1,
        });
        let release = Event::Mouse(MouseEvent::ButtonReleased {
            button: MouseButton::Left,
            x: point.x as i32 + 1,
            y: point.y as i32 + 1,
            click_count: 1,
        });
        assert!(renderer.handle_event(&press, Phase::Target));
        assert!(renderer.handle_event(&release, Phase::Target));
        assert_eq!(request.get(), Some(target));
    }
    #[test]
    fn preview_scrolls_without_changing_document_or_rebuilding_content() {
        let view = view(&"# Title\n\nParagraph with **emphasis**.\n\n".repeat(50));
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_root(
            Window::new("Markdown", ScrollView::new(view).frame(320.0, 240.0))
                .decorated(false)
                .size(Size::new(320.0, 240.0))
                .create_element(),
        );
        pipeline.layout_initial();
        pipeline.render();
        assert!(pipeline.handle_event(&Event::Mouse(MouseEvent::Wheel {
            delta_x: 0,
            delta_y: -10000,
            x: 100,
            y: 100,
            phase: WheelPhase::Moved,
            source: ScrollSource::Trackpad
        })));
        assert!(pipeline.render().is_some());
        pipeline.teardown();
    }
}
