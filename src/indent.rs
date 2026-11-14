//! Optional indentation decorations using the editor's measured visual rows.
//!
//! ScarletUI currently exposes foreground/underline spans only. This container
//! paints exclusively in leading whitespace, after the normal editor, so text,
//! diagnostics, hit testing, IME, and selection remain owned by TextView.
use kureha_code::theme::WorkbenchColors;
use scarlet_ui::{
    element::{ElementRenderObject, LayoutConstraints, RenderElement, UpdateResult},
    prelude::*,
    renderer::PaintContext,
    views::text_view::{TextViewLayout, TextViewRenderObject},
};
use std::{any::Any, ops::Range, sync::Arc};

// Match TextView's Spaces(4) mode and its fixed four-space tab expansion.
const INDENT: usize = 4;
const PADDING: f32 = 10.0;

#[derive(Clone, Debug)]
struct Line {
    columns: usize,
    blank: bool,
    reference: usize,
    block: Option<usize>,
}

#[derive(Debug, PartialEq)]
struct Block {
    lines: Range<usize>,
    column: usize,
}

#[derive(Default)]
struct Indents {
    lines: Vec<Line>,
    blocks: Vec<Block>,
}

impl Indents {
    /// One linear analysis per document revision; cursor/scroll/theme changes
    /// reuse it. A stack builds block ranges without scanning around the caret.
    fn new(source: &str) -> Self {
        let mut lines: Vec<_> = source
            .split('\n')
            .enumerate()
            .map(|(reference, text)| {
                let text = text.strip_suffix('\r').unwrap_or(text);
                let columns = text
                    .bytes()
                    .take_while(|b| matches!(b, b' ' | b'\t'))
                    .map(|b| if b == b'\t' { INDENT } else { 1 })
                    .sum();
                Line {
                    columns,
                    blank: text.trim_matches([' ', '\t']).is_empty(),
                    reference,
                    block: None,
                }
            })
            .collect();
        // Empty lines inside a block inherit the shallower adjacent indent.
        // Leading/trailing empty lines do not invent a new block.
        let mut previous = None;
        for i in 0..lines.len() {
            if lines[i].blank {
                lines[i].columns = previous.map_or(0, |p: usize| lines[p].columns);
                lines[i].reference = previous.unwrap_or(i);
            } else {
                previous = Some(i);
            }
        }
        let mut next: Option<usize> = None;
        for i in (0..lines.len()).rev() {
            if lines[i].blank {
                if let Some(n) = next {
                    if lines[n].columns < lines[i].columns {
                        lines[i].columns = lines[n].columns;
                        lines[i].reference = n;
                    }
                }
            } else {
                next = Some(i);
            }
        }
        let mut result = Self {
            lines,
            blocks: Vec::new(),
        };
        let mut stack: Vec<(usize, usize)> = Vec::new();
        for i in 0..result.lines.len() {
            let depth = result.lines[i].columns;
            while stack.last().is_some_and(|(d, _)| *d > depth) {
                let (_, block) = stack.pop().unwrap();
                result.blocks[block].lines.end = i;
            }
            if depth > 0 && stack.last().is_none_or(|(d, _)| *d < depth) {
                let block = result.blocks.len();
                result.blocks.push(Block {
                    lines: i..result.lines.len(),
                    column: stack.last().map_or(0, |(d, _)| *d),
                });
                stack.push((depth, block));
            }
            result.lines[i].block = stack.last().map(|(_, block)| *block);
        }
        // A block header activates its following body, including across blanks.
        // Retain the body's original block before changing its header selection.
        let mut next: Option<(usize, Option<usize>)> = None;
        for i in (0..result.lines.len()).rev() {
            if !result.lines[i].blank {
                let own = (result.lines[i].columns, result.lines[i].block);
                if let Some((depth, block)) = next {
                    if depth > own.0 {
                        result.lines[i].block = block;
                    }
                }
                next = Some(own);
            }
        }
        result
    }

    fn active(&self, line: usize) -> Option<&Block> {
        self.lines.get(line)?.block.map(|block| &self.blocks[block])
    }
}

#[derive(Clone, Debug)]
struct BracketPair { open: usize, close: usize, color: Color }

fn bracket_pairs(source: &str, spans: &[TextStyleSpan]) -> Vec<BracketPair> {
    let mut stack = Vec::new();
    let mut pairs = Vec::new();
    // Semantic punctuation spans are exactly one byte. Strings/comments remain
    // lexical spans, so braces inside them cannot create a guide.
    for span in spans {
        if span.range.len() != 1 { continue; }
        let Some(&ch) = source.as_bytes().get(span.range.start) else { continue; };
        if matches!(ch, b'{' | b'[' | b'(') {
            stack.push((ch, span.range.start, span.color));
        } else if matches!(ch, b'}' | b']' | b')') {
            let expected = match ch { b'}' => b'{', b']' => b'[', _ => b'(' };
            while let Some((open, byte, color)) = stack.pop() {
                if open == expected {
                    pairs.push(BracketPair { open: byte, close: span.range.start, color });
                    break;
                }
            }
        }
    }
    pairs
}

// Map source offsets through tab expansion and wrapping using TextView's own
// measured rows, so guide endpoints land at the actual bracket glyph.
fn row_x(row: &scarlet_ui::views::text_view::VisualLine, byte: usize) -> f32 {
    let end = row.display_text().char_indices().map(|(i, _)| i)
        .chain(std::iter::once(row.display_text().len()))
        .find(|i| row.source_byte_at_display_byte(*i) >= byte)
        .unwrap_or(row.display_text().len());
    row.x + row.display_advance(end)
}

#[derive(Clone)]
pub(super) struct DecoratedEditor {
    editor: TextView,
    revision: u64,
    backgrounds: bool,
    guide: bool,
    colors: WorkbenchColors,
    brackets: Arc<Vec<BracketPair>>,
    link: Option<Range<usize>>,
    observer: Arc<dyn Fn(&Event)>,
}

impl DecoratedEditor {
    pub(super) fn new(
        editor: TextView,
        revision: u64,
        backgrounds: bool,
        guide: bool,
        colors: WorkbenchColors,
    ) -> Self {
        Self {
            editor,
            revision,
            backgrounds,
            guide,
            colors,
            brackets: Arc::new(Vec::new()),
            link: None,
            observer: Arc::new(|_| {}),
        }
    }
}

impl DecoratedEditor {
    pub(super) fn observe(mut self, observer: impl Fn(&Event) + 'static) -> Self { self.observer = Arc::new(observer); self }
    pub(super) fn decorations(mut self, source: &str, spans: &[TextStyleSpan], link: Option<Range<usize>>) -> Self {
        self.brackets = Arc::new(bracket_pairs(source, spans));
        self.link = link;
        self
    }
}

impl View for DecoratedEditor {
    fn create_element(&self) -> Box<dyn Element> {
        Box::new(RenderElement::with_view_children_and_updater(
            self.clone(),
            |view| {
                let mut render = Decorations {
                    view: view.clone(),
                    revision: None,
                    indents: Indents::default(),
                    size: Size::ZERO,
                    marks: Vec::new(),
                };
                render.update(view);
                render
            },
            |render, view| {
                render.update(view);
                UpdateResult::Updated
            },
            |view| vec![Box::new(super::editor_events::ObservedEditor { editor: view.editor.clone(), observer: view.observer.clone() })],
        ))
    }
    fn listenables(&self) -> Vec<&dyn Listenable> {
        // Subscribe locally, so moving the caret does not rebuild syntax spans
        // or workbench chrome. TextView remains the actual editor child.
        self.editor.listenables()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct Decorations {
    view: DecoratedEditor,
    revision: Option<u64>,
    indents: Indents,
    size: Size,
    marks: Vec<(Rect, Color)>,
}

impl Decorations {
    fn update(&mut self, view: &DecoratedEditor) {
        self.view = view.clone();
        if !view.backgrounds && !view.guide {
            self.indents = Indents::default();
            self.revision = None;
        } else if self.revision != Some(view.revision) {
            let document = view.editor.document_state().expect("document editor").get();
            self.indents = Indents::new(&document.as_str());
            self.revision = Some(view.revision);
        }
    }

    fn measure(&mut self, layout: &TextViewLayout) {
        self.marks.clear();
        if !self.view.backgrounds && !self.view.guide && self.view.link.is_none() {
            return;
        }
        let document = self.view.editor.document_state().unwrap().get();
        let caret_line =
            document.line_index_at_byte(self.view.editor.selection_state().get().caret.byte);
        let active = self
            .view
            .guide
            .then(|| self.indents.active(caret_line))
            .flatten().filter(|_| self.view.brackets.is_empty());
        let colors = self.view.colors;
        // Stable, distinct hues remain distinguishable even in monochrome themes.
        let palette = [
            Color::rgb(220, 195, 90),
            Color::rgb(100, 175, 135),
            Color::rgb(185, 130, 190),
            Color::rgb(100, 155, 195),
        ];
        let dark = colors.background.r + colors.background.g + colors.background.b < 1.5;
        let opacity = if dark { 0.065 } else { 0.045 };
        let base = layout.visual_lines.first().map_or(0.0, |line| line.x);
        let left = PADDING + layout.gutter_width;
        let right = (self.size.width - 1.0).max(left);
        for row in &layout.visual_lines[layout.visible_lines.clone()] {
            let Some(line) = self.indents.lines.get(row.logical_line) else {
                continue;
            };
            let first = layout
                .visual_lines
                .partition_point(|row| row.logical_line < line.reference);
            let Some(reference) = layout.visual_lines.get(first) else {
                continue;
            };
            if !reference.display_text().starts_with(' ') {
                continue;
            }
            let columns = line.columns.min(reference.display_text().len());
            // Query measured prefixes, not a multiplied/rounded space width.
            let x_at = |column: usize| base + reference.display_advance(column.min(columns));
            let first = layout
                .visual_lines
                .partition_point(|r| r.logical_line < row.logical_line);
            let continuation = layout.visual_lines[first].text_range.start != row.text_range.start;
            // A continuation can suppress indentation in a narrow viewport.
            // Only decorate its measured empty margin, never wrapped code.
            let whitespace_right = if line.blank {
                x_at(columns)
            } else if continuation {
                row.x
            } else {
                let spaces = row
                    .display_text()
                    .bytes()
                    .take_while(|b| *b == b' ')
                    .count();
                row.x + row.display_advance(spaces)
            };
            let top = row.y.max(1.0);
            let bottom = (row.y + layout.line_height).min(self.size.height - 1.0);
            if bottom <= top {
                continue;
            }
            if self.view.backgrounds || self.view.guide {
                let bands = columns.div_ceil(INDENT);
                let (mut start, mut end) = (0, bands);
                // Skip horizontally offscreen levels without visiting them.
                while start < end {
                    let middle = start + (end - start) / 2;
                    if x_at((middle + 1) * INDENT) <= left {
                        start = middle + 1;
                    } else {
                        end = middle;
                    }
                }
                for depth in start..bands {
                    let x = x_at(depth * INDENT).max(left);
                    if x >= whitespace_right.min(right) {
                        break;
                    }
                    let end = x_at((depth + 1) * INDENT).min(whitespace_right).min(right);
                    if end > x {
                        if self.view.backgrounds {
                            self.marks.push((
                                Rect::from_xywh(x, top, end - x, bottom - top),
                                palette[depth % palette.len()].with_opacity(opacity),
                            ));
                        }
                        if depth > 0 {
                            self.marks.push((Rect::from_xywh(x, top, 1.0, bottom - top), colors.foreground.with_opacity(0.12)));
                        }
                    }
                }
            }
            if let Some(block) = active.filter(|block| block.lines.contains(&row.logical_line)) {
                let x = x_at(block.column);
                if x >= left && x + 1.0 <= whitespace_right.min(right) {
                    self.marks.push((
                        Rect::from_xywh(x, top, 1.0, bottom - top),
                        colors.warning.with_opacity(0.90),
                    ));
                }
            }
        }
        let pair = self.view.guide.then(|| self.view.brackets.iter()
            .filter(|p| {
                let open = document.line_index_at_byte(p.open);
                let close = document.line_index_at_byte(p.close);
                open < close && open <= caret_line && caret_line <= close
            })
            .min_by_key(|p| p.close - p.open)).flatten();
        if let Some(pair) = pair {
            let start = layout.visual_lines.partition_point(|r| r.text_range.end <= pair.open);
            let end = layout.visual_lines.partition_point(|r| r.text_range.end <= pair.close);
            if let (Some(open), Some(close)) = (layout.visual_lines.get(start), layout.visual_lines.get(end)) {
                if start != end {
                    let open_x = row_x(open, pair.open);
                    let close_x = row_x(close, pair.close);
                    let x = close_x.min(open_x);
                    let y1 = open.y + layout.line_height - 1.0;
                    let y2 = close.y + layout.line_height - 1.0;
                    let top = y1.max(1.0);
                    let bottom = y2.min(self.size.height - 1.0);
                    let color = pair.color.with_opacity(0.9);
                    if x >= left && x < right && bottom > top {
                        self.marks.push((Rect::from_xywh(x, top, 1.0, bottom - top), color));
                    }
                    for (y, row, byte) in [(y1, open, pair.open), (y2, close, pair.close)] {
                        if y >= 1.0 && y < self.size.height - 1.0 {
                            let from = x.max(left);
                            let to = row_x(row, byte + 1).min(right);
                            if to > from { self.marks.push((Rect::from_xywh(from, y, to - from, 1.0), color)); }
                        }
                    }
                }
            }
        }
        if let Some(link) = &self.view.link {
            for row in &layout.visual_lines[layout.visible_lines.clone()] {
                let start = link.start.max(row.text_range.start);
                let end = link.end.min(row.text_range.end);
                if start >= end { continue; }
                let x = row_x(row, start).max(left);
                let end = row_x(row, end).min(right);
                let y = row.y + layout.line_height - 2.0;
                if end > x && y > 0.0 && y < self.size.height {
                    self.marks.push((Rect::from_xywh(x, y, end - x, 1.5), colors.accent));
                }
            }
        }
    }
}

impl ElementRenderObject for Decorations {
    // RenderElement containers otherwise delegate subscriptions to children.
    // These marks depend on the child's selection and scroll, too.
    fn owns_view_state(&self) -> bool {
        true
    }
    fn update_needs_layout(&self) -> bool {
        true
    }
    fn layout(&mut self, constraints: LayoutConstraints) -> Size {
        self.size = constraints.constrain(Size::new(400.0, 300.0));
        self.size
    }
    fn layout_with_children(
        &mut self,
        constraints: LayoutConstraints,
        children: &mut [Box<dyn Element>],
    ) -> Size {
        if let Some(editor) = children.first_mut() {
            self.size = editor.layout(constraints);
            editor.set_position(Point::ZERO);
            if let Some(text) = editor
                .render_object()
                .and_then(|r| r.as_any().downcast_ref::<TextViewRenderObject>())
            {
                self.measure(text.text_layout());
            }
        }
        self.size
    }
    fn size(&self) -> Size {
        self.size
    }
    fn render(&mut self) {}
    fn paint_overlay(&self, ctx: &mut PaintContext<'_>, origin: Point) -> bool {
        for (rect, color) in &self.marks {
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
        true
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kureha_code::theme::Theme;

    #[test]
    fn bracket_guides_use_pair_color_and_reach_the_opening_bracket() {
        let source = "fn main() {\n    if ready {\n        run();\n    }\n}\n";
        let color = Color::rgb(218, 112, 214);
        let open = source.find("ready {").unwrap() + 6;
        let close = source.find("    }").unwrap() + 4;
        let spans = vec![TextStyleSpan { range: open..open + 1, color }, TextStyleSpan { range: close..close + 1, color }];
        let view = view(source, false, true, WrapMode::None).decorations(source, &spans, None);
        view.editor.selection_state().set(TextSelection::collapsed(source.find("run").unwrap()));
        let mut element = view.create_element();
        element.layout(LayoutConstraints::tight(600.0, 200.0));
        let render = decorations(element.as_ref());
        let guide: Vec<_> = render.marks.iter().filter(|(_, c)| c.a > 0.8).collect();
        assert_eq!(guide.len(), 3);
        assert!(guide.iter().all(|(_, c)| *c == color.with_opacity(0.9)));
        let layout = element.children()[0].render_object().unwrap().as_any().downcast_ref::<TextViewRenderObject>().unwrap().text_layout();
        let row = &layout.visual_lines[1];
        let endpoint = row_x(row, open + 1);
        assert!(guide.iter().any(|(r, _)| r.size.height == 1.0 && (r.origin.x + r.size.width - endpoint).abs() < 0.01));
        assert!(guide.iter().any(|(r, _)| r.size.width == 1.0 && r.size.height > 20.0));
    }

    #[test]
    fn native_pipeline_updates_the_guide_on_keyboard_input_without_rebuilding_the_view() {
        use scarlet_ui::{
            Event, KeyCode,
            event::{KeyEvent, KeyModifiers},
            pipeline::RenderingPipeline,
        };
        fn find(element: &dyn Element) -> Option<&Decorations> {
            element
                .render_object()
                .and_then(|r| r.as_any().downcast_ref::<Decorations>())
                .or_else(|| element.children().iter().find_map(|c| find(c.as_ref())))
        }
        let mut view = view(
            "root\n    branch\n        leaf\n    other\nend",
            true,
            true,
            WrapMode::Code,
        );
        view.editor = view.editor.autofocus(true);
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_root(
            Window::new("Indent QA", view)
                .decorated(false)
                .size(Size::new(420.0, 240.0))
                .create_element(),
        );
        pipeline.layout_initial();
        pipeline.render();
        let before = find(pipeline.element_tree().root().unwrap())
            .unwrap()
            .marks
            .clone();
        for _ in 0..2 {
            pipeline.handle_event(&Event::Keyboard(KeyEvent::Pressed {
                keycode: KeyCode::Down,
                modifiers: KeyModifiers::default(),
            }));
            pipeline.render();
        }
        let after = find(pipeline.element_tree().root().unwrap()).unwrap();
        assert_ne!(before, after.marks);
        assert!(
            after
                .marks
                .iter()
                .any(|(rect, color)| color.a > 0.8 && rect.size.width == 1.0)
        );
        pipeline.teardown();
    }

    #[test]
    fn nested_headers_siblings_dedents_and_blank_lines_select_their_own_block() {
        let index = Indents::new(
            "fn main() {\n    if ok {\n        first();\n\n        second();\n    }\n    done();\n}\n",
        );
        assert_eq!(index.active(0).unwrap().lines, 1..7);
        assert_eq!(index.active(1).unwrap().lines, 2..5);
        assert_eq!(index.active(2), index.active(4));
        assert_eq!(index.active(3), index.active(2));
        assert_eq!(index.active(2).unwrap().column, 4);
        assert_eq!(index.active(5), index.active(0));
        assert_eq!(index.active(6), index.active(0));
        assert!(index.active(7).is_none());
        assert!(index.active(8).is_none());
    }

    #[test]
    fn tabs_crlf_unicode_and_non_four_space_blocks_match_document_lines() {
        let source = "ヘッダ\r\n \tα\r\n \t  β\r\n\r\nend\r\n";
        let index = Indents::new(source);
        let doc = TextDocument::from_str(source);
        assert_eq!(index.lines.len(), doc.line_count());
        assert_eq!(index.lines[1].columns, 5); // ScarletUI expands each tab to four spaces.
        assert_eq!(index.lines[2].columns, 7);
        assert_eq!(index.active(1).unwrap().column, 5);
        assert_eq!(index.active(1).unwrap().lines, 2..3);
        assert!(index.active(3).is_none());
        assert!(Indents::new("").active(0).is_none());
        assert!(Indents::new("\n  \n\t\n").blocks.is_empty());
    }

    fn view(source: &str, backgrounds: bool, guide: bool, wrap: WrapMode) -> DecoratedEditor {
        let document = State::new(
            scarlet_ui::generate_state_id(),
            TextDocument::from_str(source),
        );
        let selection = State::new(scarlet_ui::generate_state_id(), TextSelection::collapsed(0));
        DecoratedEditor::new(
            TextView::with_document(document, selection)
                .scroll_state(State::new(
                    scarlet_ui::generate_state_id(),
                    TextViewScroll::default(),
                ))
                .line_numbers(true)
                .padding(PADDING)
                .font_size(14.0)
                .wrap_mode(wrap)
                .borderless(),
            1,
            backgrounds,
            guide,
            Theme::Dark.editor_colors(),
        )
    }

    fn decorations(element: &dyn Element) -> &Decorations {
        element
            .render_object()
            .unwrap()
            .as_any()
            .downcast_ref::<Decorations>()
            .unwrap()
    }

    #[test]
    fn real_editor_layout_keeps_marks_in_whitespace_when_wrapped_resized_and_scrolled() {
        let source = format!(
            "root\n    {}\n        deep\nend\n",
            "猫 long words ".repeat(70)
        );
        for wrap in [WrapMode::None, WrapMode::Code, WrapMode::CodeAnywhere] {
            let view = view(&source, true, true, wrap);
            for width in [120.0, 420.0] {
                let mut element = view.create_element();
                element.layout(LayoutConstraints::tight(width, 120.0));
                let render = decorations(element.as_ref());
                assert!(!render.marks.is_empty());
                let child = element.children()[0].render_object().unwrap();
                let layout = child
                    .as_any()
                    .downcast_ref::<TextViewRenderObject>()
                    .unwrap()
                    .text_layout();
                for (rect, _) in &render.marks {
                    assert!(rect.origin.x >= PADDING + layout.gutter_width);
                    assert!(rect.origin.x + rect.size.width <= width - 1.0);
                    assert!(rect.origin.y >= 1.0);
                    assert!(rect.origin.y + rect.size.height <= 119.0);
                    let row = layout
                        .visual_lines
                        .iter()
                        .find(|r| r.y <= rect.origin.y && r.y + layout.line_height > rect.origin.y)
                        .unwrap();
                    let leading = row
                        .display_text()
                        .bytes()
                        .take_while(|b| *b == b' ')
                        .count();
                    let text_start = row.x + row.display_advance(leading);
                    assert!(
                        rect.origin.x + rect.size.width <= text_start + 0.01,
                        "decoration covered code: {wrap:?}"
                    );
                }
                let gutter_width = layout.gutter_width;
                // Scroll via the same public State used by the workbench.
                let scroll = State::new(
                    scarlet_ui::generate_state_id(),
                    TextViewScroll { x: 20.0, y: 30.0 },
                );
                let scrolled = DecoratedEditor {
                    editor: view.editor.clone().scroll_state(scroll),
                    ..view.clone()
                };
                element.update(&scrolled);
                element.layout(LayoutConstraints::tight(width, 120.0));
                assert!(
                    decorations(element.as_ref())
                        .marks
                        .iter()
                        .all(|(rect, _)| rect.origin.x >= PADDING + gutter_width)
                );
            }
        }
    }

    #[test]
    fn caret_updates_reuse_analysis_and_document_edits_invalidate_it() {
        let mut view = view(
            "root\n    one\n        two\nend",
            true,
            true,
            WrapMode::Code,
        );
        let mut element = view.create_element();
        let constraints = LayoutConstraints::tight(400.0, 200.0);
        element.layout(constraints);
        let index_ptr = decorations(element.as_ref()).indents.lines.as_ptr();
        let before = decorations(element.as_ref()).marks.clone();
        view.editor
            .selection_state()
            .set(TextSelection::collapsed(20));
        element.update(&view);
        element.layout(constraints);
        assert_eq!(
            index_ptr,
            decorations(element.as_ref()).indents.lines.as_ptr()
        );
        assert_ne!(before, decorations(element.as_ref()).marks);
        view.editor
            .document_state()
            .unwrap()
            .set(TextDocument::from_str("root\n  edited\nend"));
        view.revision += 1;
        element.update(&view);
        element.layout(constraints);
        assert_eq!(decorations(element.as_ref()).indents.lines.len(), 3);
        assert_eq!(decorations(element.as_ref()).indents.lines[1].columns, 2);
        view.backgrounds = false;
        view.guide = false;
        element.update(&view);
        element.layout(constraints);
        let render = decorations(element.as_ref());
        assert!(render.indents.lines.is_empty() && render.marks.is_empty());
    }

    #[test]
    fn paint_work_is_bounded_by_the_viewport_and_options_are_independent() {
        let source = format!("root\n{}end", "    one\n        two\n".repeat(10_000));
        for (backgrounds, guide) in [(true, false), (false, true), (false, false)] {
            let view = view(&source, backgrounds, guide, WrapMode::None);
            let mut element = view.create_element();
            element.layout(LayoutConstraints::tight(400.0, 100.0));
            let render = decorations(element.as_ref());
            assert!(render.marks.len() < 25);
            if backgrounds {
                assert!(render.marks.iter().all(|(_, color)| color.a < 0.2));
            } else if guide {
                assert!(!render.marks.is_empty());
                assert!(
                    render
                        .marks
                        .iter()
                        .all(|(rect, _)| rect.size.width == 1.0)
                );
            } else {
                assert!(render.indents.lines.is_empty() && render.marks.is_empty());
            }
        }
    }
}
