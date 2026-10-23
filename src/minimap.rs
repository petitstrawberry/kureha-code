//! Bounded source overview with real syntax colors and shared editor scrolling.
//!
//! Keep one `MinimapCache` in app runtime. After requesting `syntax::spans`, call
//! `cache.snapshot(id, revision, path, &document)`. Refresh on `syntax::poll()`;
//! unchanged snapshots are shared without rescanning source or copying spans.
//! Mount `Minimap::new(snapshot, editor_scroll, theme).view()` beside an unwrapped
//! TextView at the same height. Set `line_height` from the editor font metrics
//! and `editor_padding` to its padding. Visibility can follow a menu toggle via
//! `enabled`, or the parent can omit the view. No header or text layout is used.
//! Rows keep a two-pixel pitch and scroll with the editor. Files exceeding the
//! row budget retain a whole-document sample at that same compact density.
use crate::{syntax, theme::Theme};
use scarlet_ui::{
    element::{RenderElement, UpdateResult},
    prelude::*,
    renderer::PaintContext,
};
use std::{
    any::Any,
    cell::Cell,
    collections::VecDeque,
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

pub const MAX_ROWS: usize = 768;
pub const MAX_COLUMNS: usize = 96;
pub const MAX_MARKS_PER_ROW: usize = 32;
pub const MAX_SOURCE_BYTES: usize = 256 * 1024;
pub const MAX_LINE_BYTES: usize = 2048;
const MAX_DOCUMENTS: usize = 4;
// Fixed logical-pixel density, independent of pane height and document length.
// Large files retain the bounded whole-document sample rather than adding rows.
const ROW_PITCH: f32 = 2.0;
const MARK_HEIGHT: f32 = 1.0;

/// A contiguous run of occupied source columns. Whitespace is left blank.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub columns: Range<u16>,
    /// None means the selected theme's ordinary foreground.
    pub color: Option<Color>,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub line: usize,
    /// UTF-8 byte offset at the beginning of this sampled logical line.
    pub byte: usize,
    pub marks: Vec<Mark>,
}

/// Holds only sampled geometry/colors, never a document or full source copy.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub rows: Vec<Row>,
    pub line_count: usize,
    pub source_len: usize,
    pub theme: Theme,
    /// True if lines, columns, or pathological lines were omitted.
    pub sampled: bool,
    pub source_bytes_read: usize,
}

#[derive(PartialEq, Eq)]
struct Key {
    id: u64,
    revision: u64,
    path: Option<PathBuf>,
    theme: Theme,
    syntax_generation: Option<u64>,
}

/// Four LRU document summaries, each capped at 768 rows × 32 marks. Cache hits
/// inspect only document identity/revision/path and syntax generation.
#[derive(Default)]
pub struct MinimapCache {
    entries: VecDeque<(Key, Arc<Snapshot>)>,
}
impl MinimapCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn remove(&mut self, id: u64) {
        self.entries.retain(|(key, _)| key.id != id);
    }

    /// `revision` must change for every source change, including undo/redo.
    /// Uses indexed lines, including for documents too large to highlight.
    /// Syntax is neutral until the background worker publishes exact colors.
    pub fn snapshot(
        &mut self,
        id: u64,
        revision: u64,
        path: Option<&Path>,
        document: &TextDocument,
    ) -> Arc<Snapshot> {
        let colors = syntax::cached(id, revision, path);
        let theme = colors.as_ref().map_or_else(syntax::theme, |c| c.theme);
        let key = Key {
            id,
            revision,
            path: path.map(Path::to_path_buf),
            theme,
            syntax_generation: colors.as_ref().map(|c| c.generation),
        };
        if let Some(index) = self.entries.iter().position(|(cached, _)| cached == &key) {
            let entry = self.entries.remove(index).unwrap();
            let snapshot = entry.1.clone();
            self.entries.push_back(entry);
            return snapshot;
        }
        self.remove(id);
        let snapshot = Arc::new(Snapshot::build(
            document,
            colors.as_ref().map_or(&[], |c| c.spans()),
            theme,
        ));
        if self.entries.len() == MAX_DOCUMENTS {
            self.entries.pop_front();
        }
        self.entries.push_back((key, snapshot.clone()));
        snapshot
    }
}

impl Snapshot {
    fn build(document: &TextDocument, spans: &[TextStyleSpan], theme: Theme) -> Self {
        let line_count = document.line_count().max(1);
        let count = line_count.min(MAX_ROWS);
        let mut result = Self {
            rows: Vec::with_capacity(count),
            line_count,
            source_len: document.len(),
            theme,
            sampled: line_count > count,
            source_bytes_read: 0,
        };
        for index in 0..count {
            // Include both ends, including a final empty line after a newline.
            let line = if count <= 1 {
                0
            } else {
                ((index as u128 * (line_count - 1) as u128) / (count - 1) as u128) as usize
            };
            let range = document.line_range(line).unwrap_or(0..0);
            let mut row = Row {
                line,
                byte: range.start,
                marks: Vec::new(),
            };
            // line_text may copy across tree chunks. Never ask it to copy an
            // unbounded line, or exceed the total source-read budget.
            if range.len() > MAX_LINE_BYTES
                || range.len() > MAX_SOURCE_BYTES - result.source_bytes_read
            {
                result.sampled = true;
            } else if let Some(text) = document.line_text(line) {
                result.source_bytes_read += text.len();
                let mut column = 0usize;
                let mut span_index = spans.partition_point(|span| span.range.end <= range.start);
                for (offset, ch) in text.char_indices() {
                    if ch == '\n' || ch == '\r' {
                        break;
                    }
                    if column >= MAX_COLUMNS {
                        result.sampled = true;
                        break;
                    }
                    let byte = range.start + offset;
                    while span_index < spans.len() && spans[span_index].range.end <= byte {
                        span_index += 1;
                    }
                    let color = spans
                        .get(span_index)
                        .filter(|span| span.range.contains(&byte))
                        .map(|span| span.color);
                    let next = if ch == '\t' {
                        (column / 4 + 1) * 4
                    } else {
                        column + 1
                    };
                    if !ch.is_whitespace() {
                        if let Some(last) = row.marks.last_mut().filter(|last| {
                            last.columns.end as usize == column && last.color == color
                        }) {
                            last.columns.end = next as u16;
                        } else if row.marks.len() < MAX_MARKS_PER_ROW {
                            row.marks.push(Mark {
                                columns: column as u16..next as u16,
                                color,
                            });
                        } else {
                            result.sampled = true;
                            break;
                        }
                    }
                    column = next;
                }
            }
            result.rows.push(row);
        }
        result
    }

    /// Nearest represented line; returned bytes are always actual line starts.
    pub fn navigation_at(&self, fraction: f32) -> (usize, usize) {
        let fraction = finite_nonnegative(fraction).min(1.0);
        let line = (fraction * self.line_count.saturating_sub(1) as f32).round() as usize;
        let next = self.rows.partition_point(|row| row.line < line);
        let index = if next == self.rows.len() {
            next.saturating_sub(1)
        } else if next > 0 && line - self.rows[next - 1].line <= self.rows[next].line - line {
            next - 1
        } else {
            next
        };
        self.rows
            .get(index)
            .map_or((0, 0), |row| (row.line, row.byte))
    }
}

/// Navigation reports a valid sampled byte offset without moving the caret.
/// Parent hooks may use it for status or their own navigation model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Navigation {
    pub line: usize,
    pub byte: usize,
    pub scroll: TextViewScroll,
}

/// Toggleable minimap. Drawing and input use logical coordinates, so pointer
/// navigation stays aligned at any display scale. Dragging preserves x scroll.
#[derive(Clone)]
pub struct Minimap {
    snapshot: Arc<Snapshot>,
    scroll: State<TextViewScroll>,
    theme: Theme,
    enabled: bool,
    width: f32,
    line_height: f32,
    editor_padding: f32,
    viewport_height: Option<f32>,
    on_navigate: Option<Rc<dyn Fn(Navigation)>>,
}

impl Minimap {
    pub fn new(snapshot: Arc<Snapshot>, scroll: State<TextViewScroll>, theme: Theme) -> Self {
        Self {
            snapshot,
            scroll,
            theme,
            enabled: true,
            width: 100.0,
            line_height: 20.0,
            editor_padding: 10.0,
            viewport_height: None,
            on_navigate: None,
        }
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    pub fn width(mut self, width: f32) -> Self {
        self.width = finite_nonnegative(width).clamp(32.0, 240.0);
        self
    }
    /// For exact alignment use `graphics::line_height_sized_with_font_stack`
    /// (or `line_height_sized`) with the TextView's font size and stack.
    pub fn line_height(mut self, pixels: f32) -> Self {
        self.line_height = finite_nonnegative(pixels).max(1.0);
        self
    }
    pub fn editor_padding(mut self, pixels: f32) -> Self {
        self.editor_padding = finite_nonnegative(pixels);
        self
    }
    /// Optional editor outer height; otherwise the minimap's height is used.
    pub fn viewport_height(mut self, pixels: f32) -> Self {
        self.viewport_height = Some(finite_nonnegative(pixels));
        self
    }
    pub fn on_navigate(mut self, callback: impl Fn(Navigation) + 'static) -> Self {
        self.on_navigate = Some(Rc::new(callback));
        self
    }

    fn content_height(&self) -> f32 {
        self.snapshot.line_count as f32 * self.line_height
    }
    fn viewport(&self, height: f32) -> f32 {
        (self.viewport_height.unwrap_or(height) - self.editor_padding * 2.0).max(1.0)
    }
    fn map_height(&self) -> f32 {
        self.snapshot.rows.len() as f32 * ROW_PITCH
    }
    fn scroll_progress(&self, height: f32) -> f32 {
        let max_scroll = (self.content_height() - self.viewport(height)).max(0.0);
        if max_scroll > 0.0 {
            finite_nonnegative(self.scroll.get().y).min(max_scroll) / max_scroll
        } else {
            0.0
        }
    }
    fn map_offset(&self, height: f32) -> f32 {
        (self.map_height() - finite_nonnegative(height)).max(0.0) * self.scroll_progress(height)
    }
    fn overlay(&self, height: f32) -> (f32, f32) {
        let map_height = self.map_height();
        let visible_height = map_height.min(finite_nonnegative(height));
        let content = self.content_height().max(1.0);
        let viewport = self.viewport(height);
        let extent =
            (map_height * viewport / content).clamp(visible_height.min(8.0), visible_height);
        (
            (visible_height - extent) * self.scroll_progress(height),
            extent,
        )
    }
    fn navigate(&self, y: f32, height: f32) {
        self.navigate_at(y, height, self.map_offset(height));
    }
    fn navigate_at(&self, y: f32, height: f32, map_offset: f32) {
        if !self.enabled || height <= 0.0 {
            return;
        }
        let y = finite_nonnegative(y).min(height);
        let index = (((y + map_offset) / ROW_PITCH).floor() as usize)
            .min(self.snapshot.rows.len().saturating_sub(1));
        let (line, byte) = self
            .snapshot
            .rows
            .get(index)
            .map_or((0, 0), |row| (row.line, row.byte));
        let mut scroll = self.scroll.get();
        scroll.y = (line as f32 * self.line_height - self.viewport(height) / 2.0).clamp(
            0.0,
            (self.content_height() - self.viewport(height)).max(0.0),
        );
        self.scroll.set(scroll);
        if let Some(callback) = &self.on_navigate {
            callback(Navigation { line, byte, scroll });
        }
    }

    pub fn view(self) -> impl View + Clone {
        let width = if self.enabled { self.width } else { 0.0 };
        let surface = Surface {
            minimap: self,
            size: Rc::new(Cell::new(Size::ZERO)),
        };
        let input = surface.clone();
        let dragging = Rc::new(Cell::new(None));
        surface
            .on_event(move |event| {
                if !input.minimap.enabled {
                    return false;
                }
                match event {
                    Event::Mouse(MouseEvent::ButtonPressed {
                        button: MouseButton::Left,
                        y,
                        ..
                    }) => {
                        // Freeze the overview's origin for this gesture so a
                        // stationary pointer cannot drift as editor scroll moves.
                        dragging.set(Some(input.minimap.map_offset(input.size.get().height)));
                        input.minimap.navigate(*y as f32, input.size.get().height);
                        true
                    }
                    Event::Mouse(MouseEvent::Moved { y, .. }) if dragging.get().is_some() => {
                        input.minimap.navigate_at(
                            *y as f32,
                            input.size.get().height,
                            dragging.get().unwrap(),
                        );
                        true
                    }
                    Event::Mouse(
                        MouseEvent::ButtonReleased {
                            button: MouseButton::Left,
                            ..
                        }
                        | MouseEvent::ButtonCancelled {
                            button: MouseButton::Left,
                            ..
                        }
                        | MouseEvent::Exited { .. },
                    ) => dragging.replace(None).is_some(),
                    _ => false,
                }
            })
            .frame(width, f32::INFINITY)
    }
}

fn finite_nonnegative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

#[derive(Clone)]
struct Surface {
    minimap: Minimap,
    size: Rc<Cell<Size>>,
}
impl View for Surface {
    fn create_element(&self) -> Box<dyn Element> {
        Box::new(RenderElement::new(
            self.clone(),
            SurfaceRenderer {
                surface: self.clone(),
            },
        ))
    }
    fn listenables(&self) -> Vec<&dyn Listenable> {
        vec![&self.minimap.scroll]
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
struct SurfaceRenderer {
    surface: Surface,
}
impl ElementRenderObject for SurfaceRenderer {
    fn layout(&mut self, constraints: LayoutConstraints) -> Size {
        let m = &self.surface.minimap;
        let size = Size::new(
            if m.enabled {
                m.width.clamp(constraints.min_width, constraints.max_width)
            } else {
                0.0
            },
            if constraints.max_height.is_finite() {
                constraints.max_height.max(0.0)
            } else {
                constraints.min_height.max(300.0)
            },
        );
        self.surface.size.set(size);
        size
    }
    fn size(&self) -> Size {
        self.surface.size.get()
    }
    fn render(&mut self) {}
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn update(&mut self, view: &dyn View) -> UpdateResult {
        let Some(surface) = view.as_any().downcast_ref::<Surface>() else {
            return UpdateResult::Replaced;
        };
        // Reconciliation can replace the input closure. Copy geometry into its
        // new shared cell even when the update requires only a repaint.
        surface.size.set(self.size());
        self.surface = surface.clone();
        UpdateResult::Updated
    }
    fn paint<'a>(&'a self, ctx: &mut PaintContext<'a>, origin: Point) -> bool {
        let m = &self.surface.minimap;
        let size = self.size();
        if !m.enabled || size.width <= 0.0 || size.height <= 0.0 {
            return true;
        }
        let colors = m.theme.colors();
        ctx.fill_rect(Rect::new(origin, size), colors.background);
        ctx.fill_rect(
            Rect::from_xywh(origin.x, origin.y, 1.0, size.height),
            colors.border,
        );
        let column_width = (size.width - 8.0).max(0.0) / MAX_COLUMNS as f32;
        let offset = m.map_offset(size.height);
        let first = (offset / ROW_PITCH).floor() as usize;
        // Skip invisible rows. Both source work and paint work remain bounded.
        for (index, row) in m.snapshot.rows.iter().enumerate().skip(first) {
            let y = index as f32 * ROW_PITCH - offset;
            if y >= size.height {
                break;
            }
            let top = y.max(0.0);
            let bottom = (y + MARK_HEIGHT).min(size.height);
            if bottom <= top {
                continue;
            }
            for mark in &row.marks {
                // A retained old snapshot cannot leak old-theme syntax colors.
                let color = if m.snapshot.theme == m.theme {
                    mark.color
                } else {
                    None
                }
                .unwrap_or(colors.foreground);
                ctx.fill_rect(
                    Rect::from_xywh(
                        origin.x + 4.0 + mark.columns.start as f32 * column_width,
                        origin.y + top,
                        (mark.columns.len() as f32 * column_width).max(0.5),
                        bottom - top,
                    ),
                    Color { a: 0.80, ..color },
                );
            }
        }
        let (top, extent) = m.overlay(size.height);
        ctx.fill_rect(
            Rect::from_xywh(origin.x + 1.0, origin.y + top, size.width - 1.0, extent),
            // Neutral translucent fill only; never use the theme accent/frame.
            Color::rgba(128, 128, 128, 0.15),
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(text: &str, spans: &[TextStyleSpan]) -> Snapshot {
        Snapshot::build(&TextDocument::from_str(text), spans, Theme::Light)
    }
    fn scroll(y: f32) -> State<TextViewScroll> {
        State::new(StateId::new(0xf000_1000), TextViewScroll { x: 17.0, y })
    }

    #[test]
    fn source_marks_keep_indentation_gaps_unicode_and_actual_syntax_ranges() {
        let text = "\tlet 猫 = \"hello\";\r\n\n// end";
        let string = text.find('"').unwrap();
        let color = Color::rgb(163, 21, 21);
        let map = snapshot(
            text,
            &[TextStyleSpan {
                range: string..string + 7,
                color,
            }],
        );
        assert_eq!(map.line_count, 3);
        assert!(map.rows[1].marks.is_empty());
        assert_eq!(map.rows[0].marks[0].columns, 4..7);
        assert_eq!(map.rows[0].marks[1].columns, 8..9); // multibyte 猫 occupies one overview column
        let colored = map.rows[0]
            .marks
            .iter()
            .find(|m| m.color.is_some())
            .unwrap();
        assert_eq!(colored.columns, 12..19);
        assert_eq!(colored.color, Some(color));
        assert_eq!(map.rows[2].byte, text.find("// end").unwrap());
        for fraction in [0.0, 0.2, 0.5, 1.0, f32::INFINITY, f32::NAN, -1.0] {
            let (_, byte) = map.navigation_at(fraction);
            assert!(byte <= text.len() && text.is_char_boundary(byte));
        }
    }

    #[test]
    fn huge_source_samples_the_whole_document_with_strict_work_and_memory_limits() {
        let text = format!("{}tail 猫", "    let value = 42;\n".repeat(100_000));
        let map = snapshot(&text, &[]);
        assert!(map.sampled);
        assert_eq!(map.rows.len(), MAX_ROWS);
        assert_eq!(map.rows.first().unwrap().byte, 0);
        assert_eq!(map.rows.last().unwrap().byte, text.find("tail").unwrap());
        assert_eq!(
            map.navigation_at(1.0),
            (100_000, text.find("tail").unwrap())
        );
        assert!(map.source_bytes_read <= MAX_SOURCE_BYTES);
        assert!(
            map.rows
                .iter()
                .all(|row| row.marks.len() <= MAX_MARKS_PER_ROW)
        );
        assert!(map.rows.windows(2).all(|rows| rows[0].line < rows[1].line));
    }

    #[test]
    fn giant_lines_and_adversarial_color_runs_do_not_escape_limits() {
        let long = format!("{}\nend", "猫".repeat(500_000));
        let map = snapshot(&long, &[]);
        assert!(map.sampled);
        assert!(map.rows[0].marks.is_empty());
        assert_eq!(map.source_bytes_read, 3);
        assert_eq!(map.rows[1].byte, long.find("end").unwrap());

        let text = "x".repeat(MAX_COLUMNS * 2);
        let spans = (0..MAX_COLUMNS * 2)
            .map(|i| TextStyleSpan {
                range: i..i + 1,
                color: if i % 2 == 0 { Color::RED } else { Color::BLUE },
            })
            .collect::<Vec<_>>();
        let map = snapshot(&text, &spans);
        assert_eq!(map.rows[0].marks.len(), MAX_MARKS_PER_ROW);
        assert!(map.sampled);
        let map = snapshot(&"x".repeat(MAX_COLUMNS * 2), &[]);
        assert_eq!(map.rows[0].marks[0].columns, 0..MAX_COLUMNS as u16);
        assert!(map.sampled);

        let map = snapshot(
            &format!("{}\n", "x".repeat(MAX_LINE_BYTES - 1)).repeat(MAX_ROWS),
            &[],
        );
        assert!(map.source_bytes_read <= MAX_SOURCE_BYTES);
    }

    #[test]
    fn cache_reuses_geometry_and_invalidates_revision_path_and_lru() {
        let doc = TextDocument::from_str("fn main() {}\n");
        let mut cache = MinimapCache::new();
        let first = cache.snapshot(1, 1, None, &doc);
        assert!(Arc::ptr_eq(&first, &cache.snapshot(1, 1, None, &doc)));
        assert!(!Arc::ptr_eq(&first, &cache.snapshot(1, 2, None, &doc)));
        let renamed = cache.snapshot(1, 2, Some(Path::new("main.rs")), &doc);
        assert!(!Arc::ptr_eq(&renamed, &cache.snapshot(1, 2, None, &doc)));
        for id in 2..20 {
            cache.snapshot(id, 1, None, &doc);
        }
        assert_eq!(cache.entries.len(), MAX_DOCUMENTS);
        assert!(!cache.entries.iter().any(|(key, _)| key.id == 1));
        cache.remove(19);
        assert_eq!(cache.entries.len(), MAX_DOCUMENTS - 1);
        cache.clear();
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn viewport_and_navigation_follow_scroll_without_moving_horizontal_offset() {
        let text = format!("{}last", "line\n".repeat(99));
        let map = Arc::new(snapshot(&text, &[]));
        let state = scroll(0.0);
        let navigation = Rc::new(Cell::new(None));
        let observed = navigation.clone();
        let minimap = Minimap::new(map, state.clone(), Theme::Light)
            .line_height(20.0)
            .editor_padding(10.0)
            .on_navigate(move |nav| observed.set(Some(nav)));
        assert_eq!(minimap.overlay(220.0), (0.0, 20.0));
        state.set(TextViewScroll { x: 17.0, y: 1800.0 });
        assert_eq!(minimap.overlay(220.0), (180.0, 20.0));
        minimap.navigate(100.0, 220.0);
        assert_eq!(state.get(), TextViewScroll { x: 17.0, y: 900.0 });
        let nav = navigation.get().unwrap();
        assert_eq!(nav.line, 50);
        assert_eq!(nav.byte, 250);
        minimap.navigate(220.0, 220.0);
        assert_eq!(state.get().y, 1800.0);
        assert_eq!(navigation.get().unwrap().byte, text.find("last").unwrap());
        minimap.clone().enabled(false).navigate(0.0, 220.0);
        assert_eq!(state.get().y, 1800.0);
        minimap.navigate(0.0, 220.0);
        assert_eq!(state.get().y, 0.0);
    }

    #[test]
    fn empty_short_and_minimum_size_viewports_stay_in_bounds() {
        for text in ["", "猫", "\n"] {
            let map = Arc::new(snapshot(text, &[]));
            assert!(map.rows.len() <= 2);
            assert_eq!(map.navigation_at(0.0), (0, 0));
            let m = Minimap::new(map, scroll(100_000.0), Theme::Dark);
            for height in [0.0, 1.0, 4.0, 300.0] {
                let (top, extent) = m.overlay(height);
                assert!(top >= 0.0 && top + extent <= height);
            }
        }
    }

    fn paint_rects(minimap: Minimap, height: f32) -> Vec<(Vec<Point>, Color)> {
        use scarlet_ui::renderer::PaintCommand;
        let renderer = SurfaceRenderer {
            surface: Surface {
                minimap,
                size: Rc::new(Cell::new(Size::new(100.0, height))),
            },
        };
        let mut context = PaintContext::new();
        renderer.paint(&mut context, Point::new(0.0, 0.0));
        context
            .commands()
            .iter()
            .map(|command| match command {
                PaintCommand::FillPath { path, color } => (path.clone(), *color),
                _ => panic!("minimap should only paint filled rectangles"),
            })
            .collect()
    }

    #[test]
    fn short_source_keeps_compact_rows_and_only_a_neutral_viewport_fill() {
        for theme in Theme::ALL {
            for height in [40.0, 220.0, 800.0] {
                let m = Minimap::new(
                    Arc::new(snapshot("one\ntwo\nthree", &[])),
                    scroll(0.0),
                    theme,
                );
                assert_eq!(m.map_height(), 6.0);
                let rects = paint_rects(m, height);
                // Background, pane separator, three source marks, viewport fill.
                assert_eq!(rects.len(), 6);
                for (index, (path, _)) in rects[2..5].iter().enumerate() {
                    assert_eq!(path[0].y, index as f32 * 2.0);
                    assert_eq!(path[2].y - path[0].y, 1.0);
                }
                let (path, color) = rects.last().unwrap();
                assert_eq!(path[0], Point::new(1.0, 0.0));
                assert!(path[2].y <= 6.0);
                assert_eq!(color.r, color.g);
                assert_eq!(color.g, color.b);
                assert_eq!(color.a, 0.15);
            }
        }
    }

    #[test]
    fn long_overview_scrolls_at_fixed_density_and_clips_to_the_pane() {
        let map = Arc::new(snapshot(&format!("{}last", "line\n".repeat(100_000)), &[]));
        let state = scroll(0.0);
        let m = Minimap::new(map, state.clone(), Theme::Light)
            .line_height(20.0)
            .editor_padding(0.0);
        assert_eq!(m.map_height(), 1536.0);
        for height in [1.0, 220.0, 440.0, 2000.0] {
            let max_scroll = m.content_height() - height;
            for progress in [0.0, 0.5, 1.0] {
                state.set(TextViewScroll {
                    x: 17.0,
                    y: max_scroll * progress,
                });
                assert!(
                    (m.map_offset(height) - (1536.0 - height).max(0.0) * progress).abs() < 0.01
                );
                let (top, extent) = m.overlay(height);
                assert!(top >= 0.0 && top + extent <= height);
                let rects = paint_rects(m.clone(), height);
                let marks = &rects[2..rects.len() - 1];
                // A one-pixel pane may show only the gap between source rows.
                if height > 1.0 {
                    assert!(!marks.is_empty());
                }
                assert!(marks.len() <= ((height / 2.0).ceil() as usize + 1).min(MAX_ROWS));
                for (path, _) in marks {
                    assert!(path[0].y >= 0.0 && path[2].y <= height);
                    assert!(path[2].y - path[0].y <= 1.0);
                }
                for pair in marks.windows(2) {
                    assert!((pair[1].0[0].y - pair[0].0[0].y - 2.0).abs() < 0.01);
                }
            }
        }
    }

    #[test]
    fn navigation_uses_the_visible_overview_and_a_stable_drag_origin() {
        let map = Arc::new(snapshot(&format!("{}last", "line\n".repeat(599)), &[]));
        let state = scroll(5900.0);
        let m = Minimap::new(map, state.clone(), Theme::Light)
            .line_height(20.0)
            .editor_padding(0.0);
        assert_eq!(m.map_offset(200.0), 500.0);
        assert_eq!(m.overlay(200.0), (90.0, 20.0));
        m.navigate(100.0, 200.0);
        assert_eq!(state.get(), TextViewScroll { x: 17.0, y: 5900.0 });
        let origin = m.map_offset(200.0);
        m.navigate_at(120.0, 200.0, origin);
        assert_eq!(state.get().y, 6100.0);
        m.navigate_at(120.0, 200.0, origin);
        assert_eq!(state.get().y, 6100.0);
        state.set(TextViewScroll {
            x: 17.0,
            y: 11800.0,
        });
        assert_eq!(m.map_offset(200.0), 1000.0);
        m.navigate(200.0, 200.0);
        assert_eq!(state.get().y, 11800.0);
    }

    #[test]
    fn mounted_view_routes_pointer_input_and_paints_the_source_and_viewport() {
        use scarlet_ui::pipeline::RenderingPipeline;
        let state = scroll(0.0);
        let minimap = Minimap::new(
            Arc::new(snapshot(
                &format!("{}last", "let value = 42;\n".repeat(99)),
                &[],
            )),
            state.clone(),
            Theme::Dark,
        )
        .line_height(20.0)
        .editor_padding(10.0);
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_root(
            Window::new("Minimap test", minimap.view())
                .decorated(false)
                .size(Size::new(100.0, 220.0))
                .create_element(),
        );
        pipeline.layout_initial();
        assert!(pipeline.render().is_some());
        assert!(
            pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonPressed {
                button: MouseButton::Left,
                x: 50,
                y: 100,
                click_count: 1,
            }))
        );
        assert_eq!(state.get(), TextViewScroll { x: 17.0, y: 900.0 });
        assert!(pipeline.render().is_some());
        assert!(pipeline.handle_event(&Event::Mouse(MouseEvent::Moved { x: 50, y: 200 })));
        assert!(state.get().y > 1600.0);
        assert!(
            pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonReleased {
                button: MouseButton::Left,
                x: 50,
                y: 200,
                click_count: 1,
            }))
        );
    }

    #[test]
    fn repaint_reconciliation_keeps_input_geometry_and_scroll_dependency() {
        let surface = Surface {
            minimap: Minimap::new(Arc::new(snapshot("hi", &[])), scroll(0.0), Theme::Dark),
            size: Rc::new(Cell::new(Size::new(100.0, 400.0))),
        };
        assert_eq!(surface.listenables().len(), 1);
        let mut renderer = SurfaceRenderer {
            surface: surface.clone(),
        };
        let new = Surface {
            size: Rc::new(Cell::new(Size::ZERO)),
            ..surface
        };
        assert!(matches!(renderer.update(&new), UpdateResult::Updated));
        assert_eq!(new.size.get(), Size::new(100.0, 400.0));
        assert!(!renderer.update_needs_layout());
    }
}
