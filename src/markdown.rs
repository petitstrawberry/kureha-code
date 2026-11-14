//! CommonMark parsing for the native preview. No HTML execution or network I/O.
use crate::theme::Theme;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use scarlet_ui::prelude::Color;
use std::{path::Path, sync::OnceLock};
use syntect::{easy::HighlightLines, parsing::SyntaxSet, util::LinesWithEndings};

pub const MAX_BYTES: usize = 256 * 1024;
const MAX_BLOCKS: usize = 4000;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Style {
    pub strong: bool,
    pub emphasis: bool,
    pub strike: bool,
    pub code: bool,
    pub link: Option<String>,
    pub image: Option<String>,
    pub image_id: usize,
    pub color: Option<Color>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Kind {
    #[default]
    Paragraph,
    Heading(u8),
    Code(String),
    Rule,
    TableRow(bool),
}

#[derive(Clone, Debug, Default)]
pub struct Block {
    pub kind: Kind,
    pub cells: Vec<Vec<Span>>,
    pub indent: usize,
    pub quote: usize,
    pub marker: String,
    pub source_line: usize,
}

impl Block {
    fn push(&mut self, text: &str, style: &Style) {
        if self.cells.is_empty() {
            self.cells.push(Vec::new());
        }
        let spans = self.cells.last_mut().unwrap();
        if let Some(last) = spans.last_mut().filter(|last| last.style == *style) {
            last.text.push_str(text);
        } else {
            spans.push(Span {
                text: text.into(),
                style: style.clone(),
            });
        }
    }

    pub fn text(&self) -> String {
        self.cells
            .iter()
            .flatten()
            .map(|s| s.text.as_str())
            .collect()
    }
}

#[derive(Clone, Default)]
pub struct Document {
    pub blocks: std::sync::Arc<Vec<Block>>,
    pub source_lines: usize,
    pub images: std::collections::BTreeMap<String, crate::markdown_resources::Resource>,
}

pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "md" | "markdown" | "mdown" | "mkd"
            )
        })
}

pub fn parse(source: &str, theme: &Theme) -> Document {
    if source.len() > MAX_BYTES {
        let mut block = Block::default();
        block.push(
            "Markdown preview is limited to 256 KiB. Open Source to view the full document.",
            &Style::default(),
        );
        return Document {
            blocks: std::sync::Arc::new(vec![block]),
            ..Document::default()
        };
    }
    let mut blocks = Vec::new();
    let mut block = Block::default();
    let mut style = Style::default();
    let mut stack = Vec::new();
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut image_id = 0;
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;

    fn flush(block: &mut Block, blocks: &mut Vec<Block>) {
        if !block.cells.is_empty() || block.kind == Kind::Rule {
            blocks.push(block.clone());
            block.marker.clear();
        }
        block.cells.clear();
        block.kind = Kind::Paragraph;
    }

    let newlines: Vec<_> = source.match_indices('\n').map(|(i, _)| i).collect();
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        if block.cells.is_empty() {
            block.source_line = newlines.partition_point(|i| *i < range.start);
        }
        if blocks.len() >= MAX_BLOCKS {
            break;
        }
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    flush(&mut block, &mut blocks);
                }
                Tag::Heading { level, .. } => {
                    flush(&mut block, &mut blocks);
                    block.kind = Kind::Heading(level as u8);
                }
                Tag::CodeBlock(kind) => {
                    flush(&mut block, &mut blocks);
                    block.kind = Kind::Code(match kind {
                        CodeBlockKind::Fenced(info) => {
                            info.split_whitespace().next().unwrap_or("").into()
                        }
                        CodeBlockKind::Indented => String::new(),
                    });
                }
                Tag::BlockQuote(_) => {
                    flush(&mut block, &mut blocks);
                    block.quote += 1;
                }
                Tag::List(start) => {
                    flush(&mut block, &mut blocks);
                    lists.push(start);
                    block.indent = lists.len();
                }
                Tag::Item => {
                    flush(&mut block, &mut blocks);
                    block.marker = match lists.last_mut() {
                        Some(Some(number)) => {
                            let marker = format!("{}.", number);
                            *number = number.saturating_add(1);
                            marker
                        }
                        _ => "•".into(),
                    };
                }
                Tag::Table(_) => {
                    flush(&mut block, &mut blocks);
                }
                Tag::TableHead | Tag::TableRow => {
                    flush(&mut block, &mut blocks);
                    block.kind = Kind::TableRow(matches!(tag, Tag::TableHead));
                }
                Tag::TableCell => {
                    block.cells.push(Vec::new());
                }
                Tag::Emphasis
                | Tag::Strong
                | Tag::Strikethrough
                | Tag::Link { .. }
                | Tag::Image { .. } => {
                    stack.push(style.clone());
                    match tag {
                        Tag::Emphasis => style.emphasis = true,
                        Tag::Strong => style.strong = true,
                        Tag::Strikethrough => style.strike = true,
                        Tag::Link { dest_url, .. } => style.link = Some(dest_url.into_string()),
                        Tag::Image { dest_url, .. } => {
                            image_id += 1;
                            style.image_id = image_id;
                            style.image = Some(dest_url.into_string());
                            block.push("", &style);
                        }
                        _ => {}
                    }
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::CodeBlock
                | TagEnd::TableHead
                | TagEnd::TableRow
                | TagEnd::Item
                | TagEnd::HtmlBlock => flush(&mut block, &mut blocks),
                TagEnd::BlockQuote(_) => {
                    flush(&mut block, &mut blocks);
                    block.quote = block.quote.saturating_sub(1);
                }
                TagEnd::List(_) => {
                    flush(&mut block, &mut blocks);
                    lists.pop();
                    block.indent = lists.len();
                }
                TagEnd::Emphasis
                | TagEnd::Strong
                | TagEnd::Strikethrough
                | TagEnd::Link
                | TagEnd::Image => {
                    if tag == TagEnd::Image {
                        if !block
                            .cells
                            .iter()
                            .flatten()
                            .any(|s| s.style.image == style.image)
                        {
                            block.push("Image", &style);
                        }
                    }
                    style = stack.pop().unwrap_or_default();
                }
                _ => {}
            },
            Event::Text(text) => block.push(&text, &style),
            Event::Code(text) => block.push(
                &text,
                &Style {
                    code: true,
                    ..style.clone()
                },
            ),
            Event::Html(text) | Event::InlineHtml(text) => block.push(
                &text,
                &Style {
                    code: true,
                    ..style.clone()
                },
            ),
            Event::SoftBreak => block.push(" ", &style),
            Event::HardBreak => block.push("\n", &style),
            Event::Rule => {
                flush(&mut block, &mut blocks);
                block.kind = Kind::Rule;
                flush(&mut block, &mut blocks);
            }
            Event::TaskListMarker(checked) => {
                block.marker = if checked { "☑" } else { "☐" }.into();
            }
            _ => {}
        }
    }
    flush(&mut block, &mut blocks);
    if blocks.len() >= MAX_BLOCKS {
        let mut notice = Block {
            source_line: newlines.len(),
            ..Block::default()
        };
        notice.push(
            "Preview block limit reached. Open Source to view the full document.",
            &Style::default(),
        );
        blocks.push(notice);
    }
    // Alt text may contain emphasis. It still describes a single image.
    for block in &mut blocks {
        for cell in &mut block.cells {
            let mut joined: Vec<Span> = Vec::new();
            for span in std::mem::take(cell) {
                if let Some(previous) = joined
                    .last_mut()
                    .filter(|p| p.style.image.is_some() && p.style.image_id == span.style.image_id)
                {
                    previous.text.push_str(&span.text);
                } else {
                    joined.push(span);
                }
            }
            *cell = joined;
        }
    }
    // Syntax grammars are shared, but parsing happens only on the preview worker.
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    let palette = theme.syntect_theme();
    let started = std::time::Instant::now();
    for block in &mut blocks {
        let Kind::Code(language) = &block.kind else {
            continue;
        };
        let source = block.text();
        if source.len() > 16 * 1024
            || source.lines().any(|l| l.len() > 2048)
            || started.elapsed().as_millis() > 100
        {
            continue;
        }
        let syntaxes = SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines);
        let Some(syntax) = syntaxes.find_syntax_by_token(language) else {
            continue;
        };
        let mut highlighter = HighlightLines::new(syntax, &palette);
        let mut spans = Vec::new();
        for line in LinesWithEndings::from(&source) {
            let Ok(tokens) = highlighter.highlight_line(line, syntaxes) else {
                break;
            };
            for (style, text) in tokens {
                spans.push(Span {
                    text: text.into(),
                    style: Style {
                        code: true,
                        color: Some(Color::rgb(
                            style.foreground.r,
                            style.foreground.g,
                            style.foreground.b,
                        )),
                        ..Style::default()
                    },
                });
            }
        }
        if spans.iter().map(|s| s.text.len()).sum::<usize>() == source.len() {
            block.cells = vec![spans];
        }
    }
    Document {
        blocks: std::sync::Arc::new(blocks),
        source_lines: newlines.len() + 1,
        ..Document::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commonmark_structure_preserves_nested_lists_and_inline_styles() {
        let doc = parse(
            "# 日本語\n\n**bold** and *em* and `code` [link](a.md)\n\n3. item\n   - nested\n\n> quote\n\n- [x] done\n\n| A | B |\n|---|---|\n| one | two |\n\n```rust\nfn main() {}\n```",
            &Theme::Light,
        );
        assert_eq!(doc.blocks[0].kind, Kind::Heading(1));
        assert!(doc.blocks[1].cells[0].iter().any(|s| s.style.strong));
        assert!(doc.blocks.iter().any(|b| b.marker == "3."));
        assert!(doc.blocks.iter().any(|b| b.indent == 2 && b.marker == "•"));
        assert!(doc.blocks.iter().any(|b| b.quote == 1));
        assert!(doc.blocks.iter().any(|b| b.marker == "☑"));
        assert!(
            doc.blocks
                .iter()
                .any(|b| b.kind == Kind::TableRow(true) && b.cells.len() == 2)
        );
        assert!(
            doc.blocks.last().unwrap().cells[0]
                .iter()
                .any(|s| s.style.color.is_some())
        );
    }

    #[test]
    fn formatted_alt_text_and_repeated_images_keep_distinct_occurrences() {
        let doc = parse("![**one** image](same.png) ![](same.png)", &Theme::Light);
        let images: Vec<_> = doc.blocks[0].cells[0]
            .iter()
            .filter(|s| s.style.image.is_some())
            .collect();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].text, "one image");
        assert_ne!(images[0].style.image_id, images[1].style.image_id);
    }

    #[test]
    fn html_is_literal_images_are_links_and_large_input_is_bounded() {
        let doc = parse(
            "<script>alert(1)</script>\n\n![alt](https://example.test/a.png)",
            &Theme::Dark,
        );
        assert!(doc.blocks[0].text().contains("<script>"));
        assert_eq!(
            doc.blocks[1].cells[0][0].style.image.as_deref(),
            Some("https://example.test/a.png")
        );
        assert_eq!(
            parse(&"x".repeat(MAX_BYTES + 1), &Theme::Light)
                .blocks
                .len(),
            1
        );
    }
}
