use super::*;
use std::ops::Range;

fn legend(types: &[&str]) -> Value {
    json!({"tokenTypes": types, "tokenModifiers": []})
}

fn decoded(text: &str, data: Value, types: &[&str]) -> Vec<TextStyleSpan> {
    decode(text, &json!({"data": data}), &legend(types), &Theme::Light)
        .expect("valid semantic token response")
}

fn ranges(spans: &[TextStyleSpan]) -> Vec<Range<usize>> {
    spans.iter().map(|span| span.range.clone()).collect()
}

fn color_at(spans: &[TextStyleSpan], byte: usize) -> Option<Color> {
    spans
        .iter()
        .find(|span| span.range.contains(&byte))
        .map(|span| span.color)
}

fn assert_valid_spans(text: &str, spans: &[TextStyleSpan]) {
    let mut previous_end = 0;
    for span in spans {
        assert!(
            span.range.start >= previous_end,
            "overlapping or unsorted span"
        );
        assert!(span.range.start < span.range.end);
        assert!(span.range.end <= text.len());
        assert!(text.is_char_boundary(span.range.start));
        assert!(text.is_char_boundary(span.range.end));
        previous_end = span.range.end;
    }
}

#[test]
fn utf16_surrogates_bmp_characters_and_crlf_map_to_exact_byte_ranges() {
    let text = "a😀名 z\r\n\r\nβ𐐀x\nlast";
    let spans = decoded(
        text,
        json!([
            0, 1, 2, 0, 0, 0, 2, 1, 0, 0, 0, 2, 1, 0, 0, 2, 0, 1, 0, 0, 0, 1, 2, 0, 0, 0, 2, 1, 0,
            0, 1, 0, 4, 0, 0
        ]),
        &["type"],
    );
    assert_eq!(
        ranges(&spans),
        vec![1..5, 5..8, 9..10, 14..16, 16..20, 20..21, 22..26]
    );
    assert_valid_spans(text, &spans);
}

#[test]
fn surrogate_interior_is_invalid_at_either_token_boundary() {
    for (start, length) in [(1, 1), (0, 1), (1, 2)] {
        assert!(
            decode(
                "😀x",
                &json!({"data": [0, start, length, 0, 0]}),
                &legend(&["type"]),
                &Theme::Light
            )
            .is_none(),
            "start={start}, length={length}"
        );
    }
    assert_eq!(
        ranges(&decoded(
            "😀x",
            json!([0, 0, 2, 0, 0, 0, 2, 1, 0, 0]),
            &["type"]
        )),
        vec![0..4, 4..5]
    );
}

#[test]
fn overlong_tokens_clamp_before_crlf_and_do_not_consume_the_next_line() {
    let text = "abc\r\n😀z\n";
    let spans = decoded(text, json!([0, 1, 99, 0, 0, 1, 0, 99, 0, 0]), &["type"]);
    assert_eq!(ranges(&spans), vec![1..3, 5..10]);
    assert_valid_spans(text, &spans);
    for (line, start) in [(0, 3), (0, 4), (1, 3), (2, 0)] {
        assert!(
            decode(
                text,
                &json!({"data": [line, start, 1, 0, 0]}),
                &legend(&["type"]),
                &Theme::Light
            )
            .is_none()
        );
    }
}

#[test]
fn malformed_responses_and_invalid_coordinates_reject_the_entire_batch() {
    let invalid = [
        json!({}),
        json!([]),
        json!({"data": null}),
        json!({"data": "tokens"}),
        json!({"data": [0, 0, 1, 0]}),
        json!({"data": [0, 0, 0, 0, 0]}),
        json!({"data": [0, 0, 1, 1, 0]}),
        json!({"data": [1, 0, 1, 0, 0]}),
        json!({"data": [0, 4, 1, 0, 0]}),
        // A valid first token must not survive a corrupt suffix.
        json!({"data": [0, 0, 1, 0, 0, 0, 1, 0, 0, 0]}),
        json!({"data": [0, 0, 1, 0, 0, 1, 0, 1, 0, 0]}),
    ];
    for result in invalid {
        assert!(
            decode("abc", &result, &legend(&["type"]), &Theme::Light).is_none(),
            "{result}"
        );
    }
    for field in 0..5 {
        for bad in [
            json!(-1),
            json!(1.5),
            json!("1"),
            json!(null),
            json!(true),
            json!(4_294_967_296u64),
        ] {
            let mut data = json!([0, 0, 1, 0, 0]);
            data[field] = bad;
            assert!(
                decode(
                    "abc",
                    &json!({"data": data}),
                    &legend(&["type"]),
                    &Theme::Light
                )
                .is_none(),
                "field {field}"
            );
        }
    }
}

#[test]
fn malformed_legends_and_out_of_range_type_indices_are_rejected() {
    for invalid in [
        json!({}),
        json!({"tokenTypes": ["type"]}),
        json!({"tokenTypes": null, "tokenModifiers": []}),
        json!({"tokenTypes": [null], "tokenModifiers": []}),
        json!({"tokenTypes": [], "tokenModifiers": []}),
        json!({"tokenTypes": ["type"], "tokenModifiers": null}),
    ] {
        assert!(
            decode(
                "x",
                &json!({"data": [0, 0, 1, 0, 0]}),
                &invalid,
                &Theme::Light
            )
            .is_none(),
            "{invalid}"
        );
    }
}

#[test]
fn overlapping_duplicate_and_ignored_tokens_are_still_validated() {
    for (first_kind, second_start) in [(0, 0), (0, 1), (1, 1)] {
        assert!(
            decode(
                "abcd",
                &json!({"data": [0, 0, 2, first_kind, 0, 0, second_start, 2, 0, 0]}),
                &legend(&["type", "string"]),
                &Theme::Light
            )
            .is_none()
        );
    }
    assert_eq!(
        ranges(&decoded(
            "abcd",
            json!([0, 0, 2, 0, 0, 0, 2, 2, 0, 0]),
            &["type"]
        )),
        vec![0..2, 2..4]
    );
    assert!(
        decode(
            "😀",
            &json!({"data": [0, 0, 1, 0, 0]}),
            &legend(&["unknownServerType"]),
            &Theme::Light
        )
        .is_none()
    );
}

#[test]
fn empty_null_and_unsupported_token_responses_produce_no_spans() {
    assert!(
        decode("", &Value::Null, &Value::Null, &Theme::Light)
            .unwrap()
            .is_empty()
    );
    assert!(decoded("", json!([]), &[]).is_empty());
    for kind in ["comment", "string", "regexp", "number", "unknownServerType"] {
        assert!(
            decoded("x", json!([0, 0, 1, 0, 0]), &[kind]).is_empty(),
            "{kind}"
        );
    }
    // An ignored kind still participates in delta positions.
    assert_eq!(
        ranges(&decoded(
            "a bc",
            json!([0, 0, 1, 0, 0, 0, 2, 2, 1, 0]),
            &["unknown", "type"]
        )),
        vec![2..4]
    );
}

#[test]
fn response_and_document_limits_accept_the_boundary_and_reject_excess() {
    let text = "x".repeat(crate::syntax::MAX_BYTES);
    assert!(decode(&text, &json!({"data": []}), &legend(&[]), &Theme::Light).is_some());
    assert!(
        decode(
            &(text + "x"),
            &json!({"data": []}),
            &legend(&[]),
            &Theme::Light
        )
        .is_none()
    );
    let text = "x".repeat(80_001);
    let mut data = vec![0u32, 0, 1, 0, 0];
    for _ in 1..80_000 {
        data.extend([0, 1, 1, 0, 0]);
    }
    let spans = decoded(&text, json!(data), &["type"]);
    assert_eq!(spans.len(), 80_000);
    assert_eq!(spans.last().unwrap().range, 79_999..80_000);
    data.extend([0, 1, 1, 0, 0]);
    assert!(
        decode(
            &text,
            &json!({"data": data}),
            &legend(&["type"]),
            &Theme::Light
        )
        .is_none()
    );
}

#[test]
fn capabilities_request_syntax_tokens_and_advertise_rust_analyzer_types() {
    let capabilities = capabilities();
    // rust-analyzer omits unmodified keywords and brackets when this is true.
    assert_eq!(capabilities["augmentsSyntaxTokens"], json!(false));
    let types = capabilities["tokenTypes"].as_array().unwrap();
    for kind in ["typeAlias", "union"] {
        assert!(types.contains(&json!(kind)), "missing token type: {kind}");
    }
}

#[test]
fn semantic_types_keywords_parameters_and_modifiers_use_custom_theme_colors() {
    let theme = crate::theme::parse_theme(
        r##"
base = 'dark'
[syntax]
type = '#123456'
keyword = '#654321'
function = '#224466'
constant = '#abcdef'
[[syntax_rules]]
scope = 'variable.parameter'
color = '#10203080'
[[syntax_rules]]
scope = 'variable.other'
color = '#405060'
"##,
    )
    .unwrap();
    let types = [
        "type",
        "class",
        "struct",
        "typeAlias",
        "union",
        "enum",
        "interface",
        "typeParameter",
        "builtinType",
        "selfTypeKeyword",
        "keyword",
        "modifier",
        "selfKeyword",
        "parameter",
        "function",
        "method",
        "macro",
        "variable",
        "namespace",
    ];
    let legend =
        json!({"tokenTypes": types, "tokenModifiers": ["declaration", "async", "readonly"]});
    let cases = [
        ("type", 0, Color::rgb(0x12, 0x34, 0x56)),
        ("class", 0, Color::rgb(0x12, 0x34, 0x56)),
        ("struct", 0, Color::rgb(0x12, 0x34, 0x56)),
        ("typeAlias", 0, Color::rgb(0x12, 0x34, 0x56)),
        ("union", 0, Color::rgb(0x12, 0x34, 0x56)),
        ("enum", 0, Color::rgb(0x12, 0x34, 0x56)),
        ("interface", 0, Color::rgb(0x12, 0x34, 0x56)),
        ("typeParameter", 0, Color::rgb(0x12, 0x34, 0x56)),
        ("builtinType", 0, Color::rgb(0x12, 0x34, 0x56)),
        ("selfTypeKeyword", 0, Color::rgb(0x12, 0x34, 0x56)),
        ("keyword", 0, Color::rgb(0x65, 0x43, 0x21)),
        ("modifier", 0, Color::rgb(0x65, 0x43, 0x21)),
        ("selfKeyword", 0, Color::rgb(0x65, 0x43, 0x21)),
        ("parameter", 0, Color::rgba(0x10, 0x20, 0x30, 128u8)),
        ("function", 0, Color::rgb(0x22, 0x44, 0x66)),
        ("method", 0, Color::rgb(0x22, 0x44, 0x66)),
        ("macro", 0, Color::rgb(0x22, 0x44, 0x66)),
        ("variable", 0, Color::rgb(0x40, 0x50, 0x60)),
        ("variable", 4, Color::rgb(0xab, 0xcd, 0xef)),
        ("variable", 2, Color::rgb(0x40, 0x50, 0x60)),
        ("namespace", 0, Color::rgb(0x12, 0x34, 0x56)),
        ("namespace", 1, Color::rgb(0x22, 0x44, 0x66)),
    ];
    for (kind, modifiers, expected) in cases {
        let index = types.iter().position(|value| *value == kind).unwrap();
        let spans = decode(
            "Name",
            &json!({"data": [0, 0, 4, index, modifiers]}),
            &legend,
            &theme,
        )
        .unwrap();
        assert_eq!(ranges(&spans), vec![0..4]);
        assert_eq!(spans[0].color, expected, "{kind}, modifiers={modifiers}");
    }
}

#[test]
fn built_in_themes_give_types_keywords_and_functions_distinct_colors() {
    let mut type_colors = Vec::new();
    for theme in Theme::ALL {
        let spans = decode(
            "T k f",
            &json!({"data": [0, 0, 1, 0, 0, 0, 2, 1, 1, 0, 0, 2, 1, 2, 0]}),
            &legend(&["type", "keyword", "function"]),
            &theme,
        )
        .unwrap();
        assert_ne!(spans[0].color, spans[1].color);
        assert_ne!(spans[0].color, spans[2].color);
        assert_ne!(spans[1].color, spans[2].color);
        type_colors.push(spans[0].color);
    }
    assert_ne!(type_colors[0], type_colors[1]);
}

#[test]
fn nested_brackets_balance_across_lines_cycle_depth_and_reset_for_siblings() {
    let text = "{[({\n})]}()";
    let data = json!([
        0, 0, 1, 0, 0, 0, 1, 1, 1, 0, 0, 1, 1, 2, 0, 0, 1, 1, 3, 0, 1, 0, 1, 3, 0, 0, 1, 1, 2, 0,
        0, 1, 1, 1, 0, 0, 1, 1, 0, 0, 0, 1, 1, 2, 0, 0, 1, 1, 2, 0
    ]);
    let custom = crate::theme::parse_theme(
        r##"
base = 'light'
[[syntax_rules]]
scope = 'punctuation.section.brackets.level1'
color = '#112233'
[[syntax_rules]]
scope = 'punctuation.section.brackets.level2'
color = '#445566'
[[syntax_rules]]
scope = 'punctuation.section.brackets.level3'
color = '#778899'
"##,
    )
    .unwrap();
    for theme in [Theme::Light, Theme::Dark, custom.clone()] {
        let spans = decode(
            text,
            &json!({"data": data}),
            &legend(&["brace", "bracket", "parenthesis", "punctuation"]),
            &theme,
        )
        .unwrap();
        assert_eq!(
            ranges(&spans),
            vec![
                0..1,
                1..2,
                2..3,
                3..4,
                5..6,
                6..7,
                7..8,
                8..9,
                9..10,
                10..11
            ]
        );
        for (open, close) in [(0, 7), (1, 6), (2, 5), (3, 4), (8, 9)] {
            assert_eq!(spans[open].color, spans[close].color);
        }
        assert_ne!(spans[0].color, spans[1].color);
        assert_ne!(spans[1].color, spans[2].color);
        assert_ne!(spans[0].color, spans[2].color);
        assert_eq!(spans[0].color, spans[3].color);
        assert_eq!(spans[0].color, spans[8].color);
        if theme == custom {
            assert_eq!(spans[0].color, Color::rgb(0x11, 0x22, 0x33));
            assert_eq!(spans[1].color, Color::rgb(0x44, 0x55, 0x66));
            assert_eq!(spans[2].color, Color::rgb(0x77, 0x88, 0x99));
        }
        assert_valid_spans(text, &spans);
    }
}

#[test]
fn eighty_thousand_mismatched_brackets_finish_without_false_pair_colors() {
    const HALF: usize = 40_000;
    let text = "(".repeat(HALF) + &"]".repeat(HALF);
    let mut data = Vec::with_capacity(HALF * 2 * 5);
    for index in 0..HALF * 2 {
        data.extend([0u32, u32::from(index != 0), 1, u32::from(index >= HALF), 0]);
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    let input = text.clone();
    std::thread::spawn(move || {
        let spans = decoded(&input, json!(data), &["parenthesis", "bracket"]);
        let _ = sender.send(spans);
    });
    // A generous hang guard, not a benchmark: unmatched closers must not scan
    // the same 40,000 openers 40,000 times.
    let spans = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("80,000 bracket tokens must finish without quadratic recovery");
    assert_eq!(spans.len(), HALF * 2, "the full token limit is accepted");
    assert_valid_spans(&text, &spans);
    let unmatched = decoded("]", json!([0, 0, 1, 0, 0]), &["bracket"])[0].color;
    for opener in &spans[..3] {
        assert_ne!(opener.color, unmatched);
    }
    for (index, span) in spans.iter().enumerate() {
        assert_eq!(span.range, index..index + 1);
        if index < HALF {
            assert_eq!(span.color, spans[index % 3].color);
        } else {
            assert_eq!(span.color, unmatched, "unmatched closer at {index}");
        }
    }
}

#[test]
fn overlay_splits_lexical_spans_and_handles_gaps_and_semantic_only_regions() {
    let base_color = Color::rgb(10, 20, 30);
    let other_color = Color::rgb(30, 40, 50);
    let top_color = Color::rgb(50, 60, 70);
    let lexical = vec![
        TextStyleSpan {
            range: 2..8,
            color: base_color,
        },
        TextStyleSpan {
            range: 10..14,
            color: other_color,
        },
        TextStyleSpan {
            range: 16..20,
            color: base_color,
        },
    ];
    let semantic: Vec<_> = [0..1, 4..6, 7..12, 14..15, 18..22]
        .into_iter()
        .map(|range| TextStyleSpan {
            range,
            color: top_color,
        })
        .collect();
    let result = overlay(&lexical, &semantic);
    assert_eq!(
        ranges(&result),
        vec![
            0..1,
            2..4,
            4..6,
            6..7,
            7..12,
            12..14,
            14..15,
            16..18,
            18..22
        ]
    );
    for byte in 0..24 {
        assert_eq!(
            color_at(&result, byte),
            color_at(&semantic, byte).or_else(|| color_at(&lexical, byte)),
            "byte {byte}"
        );
    }
    assert_valid_spans(&"x".repeat(24), &result);
    for (base, top) in [
        (&lexical[..], &[][..]),
        (&[][..], &semantic[..]),
        (&[][..], &[][..]),
    ] {
        let result = overlay(base, top);
        assert_eq!(
            ranges(&result),
            ranges(if top.is_empty() { base } else { top })
        );
        for byte in 0..24 {
            assert_eq!(
                color_at(&result, byte),
                color_at(top, byte).or_else(|| color_at(base, byte))
            );
        }
    }
}

#[test]
fn overlay_matches_bytewise_precedence_for_all_small_disjoint_span_sets() {
    // Independent byte-wise oracle covers gaps, touching ends, complete coverage,
    // and repeated cuts through one lexical span without duplicating the merger.
    fn spans(mask: u32, color: Color) -> Vec<TextStyleSpan> {
        let mut result = Vec::new();
        let mut start = 0;
        while start < 6 {
            if mask & (1 << start) == 0 {
                start += 1;
                continue;
            }
            let mut end = start + 1;
            while end < 6 && mask & (1 << end) != 0 {
                end += 1;
            }
            result.push(TextStyleSpan {
                range: start..end,
                color,
            });
            start = end;
        }
        result
    }

    for base_mask in 0..64 {
        for top_mask in 0..64 {
            let base = spans(base_mask, Color::rgb(10, 20, 30));
            let top = spans(top_mask, Color::rgb(40, 50, 60));
            let result = overlay(&base, &top);
            assert_valid_spans("abcdef", &result);
            for byte in 0..6 {
                assert_eq!(
                    color_at(&result, byte),
                    color_at(&top, byte).or_else(|| color_at(&base, byte)),
                    "base={base_mask}, top={top_mask}, byte={byte}"
                );
            }
        }
    }
}

#[test]
fn decoded_overlay_preserves_lexical_strings_escapes_comments_regexps_and_numbers() {
    let text = "Type \"a\\n😀\" //doc 42 /rx/";
    let lexical: Vec<_> = [0..4, 5..7, 7..9, 9..14, 15..17, 17..20, 21..23, 24..28]
        .into_iter()
        .enumerate()
        .map(|(i, range)| TextStyleSpan {
            range,
            color: Color::rgb((i * 20) as u8, 40, 60),
        })
        .collect();
    let semantic = decoded(
        text,
        json!([
            0, 0, 4, 0, 0, 0, 5, 7, 1, 0, 0, 8, 5, 2, 0, 0, 6, 2, 3, 0, 0, 3, 4, 4, 0
        ]),
        &["type", "string", "comment", "number", "regexp"],
    );
    assert_eq!(ranges(&semantic), vec![0..4]);
    let result = overlay(&lexical, &semantic);
    assert_valid_spans(text, &result);
    assert_ne!(color_at(&result, 0), color_at(&lexical, 0));
    for byte in 4..text.len() {
        assert_eq!(
            color_at(&result, byte),
            color_at(&lexical, byte),
            "byte {byte}"
        );
    }
    assert_ne!(
        color_at(&result, 5),
        color_at(&result, 7),
        "escape coloring survives"
    );
    assert_ne!(
        color_at(&result, 15),
        color_at(&result, 17),
        "doc detail survives"
    );
}
