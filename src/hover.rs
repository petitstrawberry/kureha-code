//! Measured, read-only language information. The scroll viewport fits the
//! content, while the body retains its full height for long documentation.
use kureha_code::theme::Theme;
use scarlet_ui::{
    element::{RenderElement, UpdateResult},
    graphics::{self, FontStack},
    prelude::*,
    renderer::PaintContext,
};
use std::{any::Any, ops::Range, path::Path, sync::Arc};

const PADDING: f32 = 14.0;
const GAP: f32 = 10.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Context,
    Code,
    Prose,
}

#[derive(Clone, Debug)]
struct Block {
    text: String,
    kind: Kind,
    spans: Vec<TextStyleSpan>,
}

fn blocks(text: &str) -> Vec<Block> {
    let mut result = Vec::new();
    let mut paragraph = Vec::new();
    for line in text.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if paragraph.is_empty() {
                continue;
            }
            let text = paragraph.join("\n");
            let code = [
                "fn ",
                "pub ",
                "extern ",
                "unsafe ",
                "struct ",
                "enum ",
                "trait ",
                "impl ",
                "type ",
                "const ",
                "static ",
                "let ",
                "def ",
                "class ",
                "function ",
                "interface ",
            ]
            .iter()
            .any(|prefix| text.trim_start().starts_with(prefix));
            result.push(Block {
                text,
                kind: if code { Kind::Code } else { Kind::Prose },
                spans: Vec::new(),
            });
            paragraph.clear();
        } else {
            paragraph.push(line);
        }
    }
    // Plaintext rust-analyzer hovers put the qualified module above the signature.
    if result.len() > 1
        && result[1].kind == Kind::Code
        && result[0]
            .text
            .chars()
            .all(|ch| ch.is_alphanumeric() || "_:.$".contains(ch))
    {
        result[0].kind = Kind::Context;
    }
    for block in &mut result {
        if block.kind == Kind::Prose {
            // LSP soft line breaks should follow the available width. Preserve
            // lists and indented examples rather than merging their rows.
            if !block
                .text
                .lines()
                .any(|line| line.starts_with([' ', '\t', '-', '*', '•']))
            {
                block.text = block.text.split_whitespace().collect::<Vec<_>>().join(" ");
            }
        }
    }
    result
}

fn measure(text: &str, size: f32, font: Option<&FontStack>) -> f32 {
    font.map_or_else(
        || graphics::measure_text_sized(text, size).0,
        |font| graphics::measure_text_sized_with_font_stack(text, size, font).0,
    ) as f32
}

struct WrappedLine {
    text: String,
    source: Range<usize>,
    runs: Vec<ColoredRun>,
}

struct ColoredRun {
    text: String,
    x: f32,
    color: Color,
}

fn color_runs(
    line: &WrappedLine,
    spans: &[TextStyleSpan],
    default: Color,
    font_size: f32,
    font: Option<&FontStack>,
) -> Vec<ColoredRun> {
    let mut ranges = Vec::new();
    let mut byte = 0;
    for span in spans {
        let start = span.range.start.max(line.source.start);
        let end = span.range.end.min(line.source.end);
        if start >= end {
            continue;
        }
        let start = start - line.source.start;
        let end = end - line.source.start;
        if byte < start {
            ranges.push((byte..start, default));
        }
        ranges.push((start..end, span.color));
        byte = end;
    }
    if byte < line.text.len() {
        ranges.push((byte..line.text.len(), default));
    }
    ranges
        .into_iter()
        .map(|(range, color)| ColoredRun {
            x: measure(&line.text[..range.start], font_size, font),
            text: line.text[range].to_owned(),
            color,
        })
        .collect()
}

fn wrap(text: &str, width: f32, size: f32, font: Option<&FontStack>) -> Vec<WrappedLine> {
    let mut result = Vec::new();
    let mut line_start = 0;
    for physical in text.split_inclusive('\n') {
        let line = physical
            .strip_suffix('\n')
            .unwrap_or(physical)
            .trim_end_matches('\r');
        let mut rest = line;
        while !rest.is_empty() {
            let mut end = 0;
            let mut word_end = 0;
            for (index, ch) in rest.char_indices() {
                let next = index + ch.len_utf8();
                if measure(&rest[..next], size, font) > width && end > 0 {
                    break;
                }
                end = next;
                if ch.is_whitespace() || ",;".contains(ch) {
                    word_end = next;
                }
            }
            if end < rest.len() && word_end > 0 {
                end = word_end;
            }
            let value = rest[..end].trim_end();
            let start = line_start + line.len() - rest.len();
            result.push(WrappedLine {
                text: value.to_owned(),
                source: start..start + value.len(),
                runs: Vec::new(),
            });
            rest = rest[end..].trim_start();
        }
        if line.is_empty() {
            result.push(WrappedLine {
                text: String::new(),
                source: line_start..line_start,
                runs: Vec::new(),
            });
        }
        line_start += physical.len();
    }
    result
}

struct Section {
    kind: Kind,
    lines: Vec<WrappedLine>,
    top: f32,
    height: f32,
    font_size: f32,
    line_height: f32,
}

#[derive(Clone)]
pub(super) struct HoverCard {
    blocks: Arc<Vec<Block>>,
    theme: Theme,
    font: Option<Arc<FontStack>>,
}

impl HoverCard {
    pub fn new(text: &str, theme: Theme, font: Option<FontStack>, path: Option<&Path>) -> Self {
        let mut blocks = blocks(text);
        for block in &mut blocks {
            if block.kind == Kind::Code {
                block.spans = kureha_code::syntax::snippet_spans(path, &block.text, theme);
            }
        }
        Self {
            blocks: Arc::new(blocks),
            theme,
            font: font.map(Arc::new),
        }
    }

    fn layout(&self, width: f32) -> (Vec<Section>, Size) {
        let mut top = PADDING;
        let mut sections = Vec::new();
        for block in self.blocks.iter() {
            let font_size = if block.kind == Kind::Context {
                11.0
            } else {
                13.0
            };
            let font = (block.kind == Kind::Code)
                .then_some(self.font.as_deref())
                .flatten();
            let line_height = font.map_or_else(
                || graphics::line_height_sized(font_size),
                |font| graphics::line_height_sized_with_font_stack(font_size, font),
            ) as f32
                + 3.0;
            let mut lines = wrap(
                &block.text,
                (width - PADDING * 2.0).max(1.0),
                font_size,
                font,
            );
            let color = if block.kind == Kind::Context {
                self.theme.colors().muted
            } else {
                self.theme.colors().foreground
            };
            for line in &mut lines {
                line.runs = color_runs(line, &block.spans, color, font_size, font);
            }
            let height = lines.len() as f32 * line_height;
            sections.push(Section {
                kind: block.kind,
                lines,
                top,
                height,
                font_size,
                line_height,
            });
            top += height + GAP;
        }
        (
            sections,
            Size::new(width, (top - GAP + PADDING).max(PADDING * 2.0)),
        )
    }

    fn preferred_width(&self) -> f32 {
        self.blocks
            .iter()
            .flat_map(|block| {
                let font = (block.kind == Kind::Code)
                    .then_some(self.font.as_deref())
                    .flatten();
                block
                    .text
                    .lines()
                    .map(move |line| measure(line, 13.0, font) + PADDING * 2.0)
            })
            .fold(280.0, f32::max)
            .min(520.0)
    }
}

impl View for HoverCard {
    fn create_element(&self) -> Box<dyn Element> {
        Box::new(RenderElement::with_view_children_and_updater(
            self.clone(),
            |card| Viewport {
                card: card.clone(),
                size: Size::ZERO,
            },
            |render, card| {
                render.card = card.clone();
                UpdateResult::Updated
            },
            |card| {
                let colors = card.theme.colors();
                vec![Box::new(
                    Surface::floating(
                        ScrollView::new(Body(card.clone())).scrollbar_color(colors.muted),
                    )
                    .fill(colors.elevated)
                    .border_color(colors.border)
                    .corner_radius(6.0),
                )]
            },
        ))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct Viewport {
    card: HoverCard,
    size: Size,
}
impl ElementRenderObject for Viewport {
    fn layout(&mut self, c: LayoutConstraints) -> Size {
        let width = self.card.preferred_width().min(c.max_width);
        let (_, content) = self.card.layout(width);
        self.size = Size::new(width, content.height.min(c.max_height));
        self.size
    }
    fn layout_with_children(
        &mut self,
        c: LayoutConstraints,
        children: &mut [Box<dyn Element>],
    ) -> Size {
        self.layout(c);
        if let Some(child) = children.first_mut() {
            child.layout(LayoutConstraints::tight(self.size.width, self.size.height));
            child.set_position(Point::ZERO);
        }
        self.size
    }
    fn update_needs_layout(&self) -> bool {
        true
    }
    fn size(&self) -> Size {
        self.size
    }
    fn hit_test(&self, p: Point) -> bool {
        Rect::new(Point::ZERO, self.size).contains(p)
    }
    fn render(&mut self) {}
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[derive(Clone)]
struct Body(HoverCard);
impl View for Body {
    fn create_element(&self) -> Box<dyn Element> {
        Box::new(RenderElement::with_view_children_and_updater(
            self.clone(),
            |view| BodyRenderer {
                card: view.0.clone(),
                sections: Vec::new(),
                size: Size::ZERO,
            },
            |render, view| {
                render.card = view.0.clone();
                UpdateResult::Updated
            },
            |_| Vec::new(),
        ))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct BodyRenderer {
    card: HoverCard,
    sections: Vec<Section>,
    size: Size,
}
impl ElementRenderObject for BodyRenderer {
    fn layout(&mut self, c: LayoutConstraints) -> Size {
        (self.sections, self.size) = self.card.layout(c.max_width);
        self.size
    }
    fn update_needs_layout(&self) -> bool {
        true
    }
    fn size(&self) -> Size {
        self.size
    }
    fn hit_test(&self, p: Point) -> bool {
        Rect::new(Point::ZERO, self.size).contains(p)
    }
    fn render(&mut self) {}
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn paint<'a>(&'a self, ctx: &mut PaintContext<'a>, origin: Point) -> bool {
        let colors = self.card.theme.colors();
        ctx.push_clip(Rect::new(origin, self.size));
        for section in &self.sections {
            if section.kind == Kind::Code {
                ctx.fill_rect(
                    Rect::from_xywh(
                        origin.x + 1.0,
                        origin.y + section.top - GAP / 2.0,
                        (self.size.width - 2.0).max(0.0),
                        section.height + GAP,
                    ),
                    colors.current_line,
                );
            }
            for (index, line) in section.lines.iter().enumerate() {
                let point = Point::new(
                    origin.x + PADDING,
                    origin.y + section.top + index as f32 * section.line_height,
                );
                let font = self
                    .card
                    .font
                    .as_ref()
                    .filter(|_| section.kind == Kind::Code);
                for run in &line.runs {
                    let point = Point::new(point.x + run.x, point.y);
                    if let Some(font) = font {
                        ctx.draw_text_with_font_stack(
                            point,
                            run.text.clone(),
                            run.color,
                            section.font_size,
                            font.clone(),
                        );
                    } else {
                        ctx.draw_text(point, run.text.clone(), run.color, section.font_size);
                    }
                }
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
        event::{MouseEvent, ScrollSource, WheelPhase},
        generate_state_id,
        pipeline::RenderingPipeline,
    };

    const EXAMPLE: &str = "scarlet\n\n\n\npub extern \"C\" fn start_kernel(boot_info: &BootInfo) -> !\n\n\nMain kernel entry point for the boot processor\n\nThis function is called by architecture-specific boot code and performs the complete kernel initialization sequence using information in the BootInfo structure.";

    fn highlighted_with_path(text: &str, theme: Theme, path: &str) -> HoverCard {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let card = HoverCard::new(
                text,
                theme,
                kureha_code::fonts::editor_stack(),
                Some(Path::new(path)),
            );
            if card
                .blocks
                .iter()
                .filter(|block| block.kind == Kind::Code)
                .all(|block| !block.spans.is_empty())
            {
                return card;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "hover syntax worker did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn highlighted(text: &str, theme: Theme) -> HoverCard {
        highlighted_with_path(text, theme, "hover.rs")
    }

    #[test]
    fn hover_highlights_code_in_both_themes_and_preserves_wrapped_unicode_ranges() {
        let text = "pub fn greet() {\n    let message = \"こんにちは🍁日本語の長い文字列を折り返しても色を保つ\";\n}\n\nOrdinary documentation remains uncolored.";
        let mut colors = Vec::new();
        for theme in Theme::ALL {
            let card = highlighted(text, theme);
            let (sections, _) = card.layout(180.0);
            let code = &card.blocks[0];
            let color_at = |byte| {
                code.spans
                    .iter()
                    .find(|span| span.range.contains(&byte))
                    .map(|span| span.color)
            };
            let keyword = color_at(0).expect("pub is highlighted");
            let string =
                color_at(code.text.find("こんにちは").unwrap()).expect("string is highlighted");
            assert_ne!(keyword, string);
            colors.push(keyword);
            let mut string_lines = 0;
            for section in &sections {
                let source = if section.kind == Kind::Code {
                    &card.blocks[0].text
                } else {
                    &card.blocks[1].text
                };
                for line in &section.lines {
                    assert_eq!(&source[line.source.clone()], line.text);
                    assert_eq!(
                        line.runs
                            .iter()
                            .map(|run| run.text.as_str())
                            .collect::<String>(),
                        line.text
                    );
                    if line.runs.iter().any(|run| run.color == string) {
                        string_lines += 1;
                    }
                    if section.kind == Kind::Prose {
                        assert!(
                            line.runs
                                .iter()
                                .all(|run| run.color == theme.colors().foreground)
                        );
                    }
                }
            }
            assert!(
                string_lines > 1,
                "multiline wrapped tokens retain their color"
            );
        }
        assert_ne!(colors[0], colors[1]);
        for (path, source) in [
            ("hover.py", "def greet(name: str):"),
            ("hover.ts", "function greet(name: string): void"),
        ] {
            assert!(
                !highlighted_with_path(source, Theme::Light, path).blocks[0]
                    .spans
                    .is_empty()
            );
        }
    }

    #[test]
    fn hover_wraps_measured_words_and_unicode_without_losing_content() {
        for font in [None, kureha_code::fonts::editor_stack()] {
            for text in [
                "Complete kernel initialization sequence",
                "日本語の説明と🍁Unicode",
                &"VeryLongIdentifier".repeat(30),
            ] {
                let lines = wrap(text, 180.0, 13.0, font.as_ref());
                assert!(
                    lines
                        .iter()
                        .all(|line| measure(&line.text, 13.0, font.as_ref()) <= 180.0)
                );
                let compact = |s: &str| {
                    s.chars()
                        .filter(|ch| !ch.is_whitespace())
                        .collect::<String>()
                };
                assert_eq!(
                    compact(
                        &lines
                            .iter()
                            .map(|line| line.text.as_str())
                            .collect::<String>()
                    ),
                    compact(text)
                );
                for line in &lines {
                    assert_eq!(&text[line.source.clone()], line.text);
                }
            }
        }
        assert!(
            wrap("Complete kernel initialization sequence", 180.0, 13.0, None)
                .iter()
                .any(|line| line.text.contains("initialization"))
        );
    }

    fn mounted(text: &str, theme: Theme) -> RenderingPipeline {
        let card = highlighted(text, theme);
        let overlay = crate::popup::Anchored::new(
            card,
            State::new(generate_state_id(), Point::new(50.0, 30.0)),
        )
        .max_size(Size::new(520.0, 340.0));
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_root(
            Window::new("Hover", overlay.background(theme.colors().background))
                .decorated(false)
                .size(Size::new(620.0, 410.0))
                .create_element(),
        );
        pipeline.layout_initial();
        pipeline
    }

    fn body(element: &dyn Element) -> (Size, Point) {
        fn find(element: &dyn Element) -> Option<(Size, Point)> {
            if element.type_name_debug().contains("ScrollViewRenderObject") {
                let content = &element.children()[0];
                return Some((content.bounds().size, content.position()));
            }
            element
                .children()
                .iter()
                .find_map(|child| find(child.as_ref()))
        }
        find(element).expect("hover body is mounted")
    }

    #[test]
    fn hover_fits_short_content_and_scrolls_to_the_end_of_long_documents() {
        let card = highlighted(EXAMPLE, Theme::Light);
        assert_eq!(
            card.blocks.iter().map(|b| b.kind).collect::<Vec<_>>(),
            [Kind::Context, Kind::Code, Kind::Prose, Kind::Prose]
        );
        let (_, size) = card.layout(520.0);
        assert!(size.height < 260.0, "excessive blank space: {size:?}");
        let short = mounted("A short description.", Theme::Light);
        assert!(body(short.element_tree().root().unwrap()).0.height < 80.0);

        let mut long = mounted(
            &format!(
                "{EXAMPLE}\n\n{}\n\nLast paragraph",
                "More documentation.\n\n".repeat(40)
            ),
            Theme::Dark,
        );
        long.render();
        let (content, initial) = body(long.element_tree().root().unwrap());
        assert!(content.height > 340.0);
        assert_eq!(initial.y, 0.0);
        assert!(long.handle_event(&Event::Mouse(MouseEvent::Wheel {
            delta_x: 0,
            delta_y: -10000,
            x: 150,
            y: 120,
            phase: WheelPhase::Moved,
            source: ScrollSource::Trackpad,
        })));
        long.render();
        let (_, scrolled) = body(long.element_tree().root().unwrap());
        assert!(
            (scrolled.y + content.height - 340.0).abs() < 1.0,
            "last paragraph must be reachable: {scrolled:?}, {content:?}"
        );
    }

    #[test]
    fn hover_renders_both_themes() {
        for theme in Theme::ALL {
            let mut pipeline = mounted(EXAMPLE, theme);
            let buffer = pipeline.render().expect("hover must paint");
            assert!(
                buffer
                    .data()
                    .chunks_exact(4)
                    .any(|pixel| pixel != theme.colors().background.to_bgra().to_le_bytes())
            );
            // Optional output uses the actual ScarletUI renderer for visual QA.
            if let Some(directory) = std::env::var_os("KUREHA_HOVER_PREVIEW") {
                use std::io::Write;
                std::fs::create_dir_all(&directory).unwrap();
                let path = std::path::Path::new(&directory)
                    .join(format!("hover-{}.ppm", theme.name().to_lowercase()));
                let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
                write!(file, "P6\n{} {}\n255\n", buffer.width(), buffer.height()).unwrap();
                for pixel in buffer.data().chunks_exact(4) {
                    file.write_all(&[pixel[2], pixel[1], pixel[0]]).unwrap();
                }
            }
        }
    }
}
