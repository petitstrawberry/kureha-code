//! Filter the server's completion set before imposing the visible-item limit.
use super::{Choice, Value, range};

fn prefix(text: &str, caret: usize) -> &str {
    let Some(before) = text.get(..caret) else {
        return "";
    };
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
        .last()
        .map_or(caret, |(i, _)| i);
    &text[start..caret]
}

// Exact prefixes precede case-insensitive prefixes, then fuzzy subsequences.
// sortText remains the server's tie breaker (scope/relevance ordering).
fn score(candidate: &str, query: &str) -> Option<(u8, usize)> {
    if query.is_empty() || candidate.starts_with(query) {
        return Some((0, 0));
    }
    let candidate = candidate.to_lowercase();
    let query = query.to_lowercase();
    if candidate.starts_with(&query) {
        return Some((1, 0));
    }
    let mut wanted = query.chars();
    let mut next = wanted.next();
    let mut skipped = 0;
    for c in candidate.chars() {
        if Some(c) == next {
            next = wanted.next();
            if next.is_none() {
                return Some((2, skipped));
            }
        } else {
            skipped += 1;
        }
    }
    None
}

pub(super) fn choices(result: &Value, text: &str, caret: usize) -> Vec<Choice> {
    let values = result
        .as_array()
        .or_else(|| result.get("items").and_then(Value::as_array));
    let query = prefix(text, caret);
    let mut matches: Vec<_> = values
        .into_iter()
        .flatten()
        .filter_map(|item| {
            // We negotiate plain text with servers; never insert snippet syntax literally.
            if item.get("insertTextFormat").and_then(Value::as_u64) == Some(2) {
                return None;
            }
            let label = item.get("label")?.as_str()?;
            let filter = item
                .get("filterText")
                .and_then(Value::as_str)
                .unwrap_or(label);
            let query = item
                .pointer("/textEdit/range")
                .or_else(|| item.pointer("/textEdit/insert"))
                .and_then(|v| range(text, v))
                .filter(|r| r.start <= caret && caret <= r.end)
                .and_then(|r| text.get(r.start..caret))
                .unwrap_or(query);
            let rank = score(filter, query)?;
            let sort = item
                .get("sortText")
                .and_then(Value::as_str)
                .unwrap_or(label);
            Some((rank, sort, label, item))
        })
        .collect();
    matches.sort_by(|a, b| (&a.0, a.1, a.2).cmp(&(&b.0, b.1, b.2)));
    matches
        .into_iter()
        .take(100)
        .map(|(_, _, label, item)| Choice {
            label: label.into(),
            detail: item
                .get("detail")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(120)
                .collect(),
            item: item.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn typed_macro_survives_large_unfiltered_server_list() {
        let mut items: Vec<_> = (0..150)
            .map(|i| json!({"label":format!("local_{i}")}))
            .collect();
        items.extend([
            json!({"label":"plan"}),
            json!({"label":"text"}),
            json!({"label":"println!(…)","filterText":"println!","insertText":"println!()"}),
        ]);
        let items = choices(&json!({"items":items}), "fn main() { printl", 18);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "println!(…)");
    }

    #[test]
    fn relevance_precedes_server_order_and_filter_text_precedes_label() {
        let items = choices(
            &json!([
                {"label":"unrelated","filterText":"println","sortText":"1"},
                {"label":"Print","sortText":"0"},
                {"label":"parse_int","sortText":"0"},
                {"label":"plan","sortText":"0"}
            ]),
            "prin",
            4,
        );
        assert_eq!(
            items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
            ["unrelated", "Print", "parse_int"]
        );
    }

    #[test]
    fn member_and_unicode_prefixes_and_empty_queries() {
        assert_eq!(prefix("obj.prin", 8), "prin");
        assert_eq!(prefix("猫.名前", "猫.名前".len()), "名前");
        assert_eq!(
            choices(&json!([{"label":"len"},{"label":"push"}]), "obj.", 4).len(),
            2
        );
    }
}
