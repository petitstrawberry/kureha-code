//! Bounded Unicode search shared by document and workspace search.
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub match_case: bool,
    pub whole_word: bool,
    pub regex: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            match_case: true,
            whole_word: false,
            regex: false,
        }
    }
}
pub struct Pattern(regex::Regex);
impl Pattern {
    pub fn new(query: &str, options: Options) -> Result<Self, String> {
        if query.is_empty() {
            return Err("Enter text to find".into());
        }
        if query.len() > 4096 {
            return Err("Search is limited to 4,096 bytes".into());
        }
        let mut expression = if options.regex {
            query.to_owned()
        } else {
            regex::escape(query)
        };
        if options.whole_word {
            expression = format!(r"\b(?:{expression})\b");
        }
        regex::RegexBuilder::new(&expression)
            .case_insensitive(!options.match_case)
            .multi_line(true)
            .size_limit(2 * 1024 * 1024)
            .dfa_size_limit(2 * 1024 * 1024)
            .build()
            .map(Self)
            .map_err(|e| format!("Invalid search: {e}"))
    }
    pub fn ranges<'a>(&'a self, text: &'a str) -> impl Iterator<Item = Range<usize>> + 'a {
        self.0.find_iter(text).map(|m| m.range())
    }
    pub fn next(
        &self,
        text: &str,
        start: usize,
        backwards: bool,
        skip: Option<Range<usize>>,
    ) -> Option<Range<usize>> {
        let mut first = None;
        let mut last = None;
        let mut previous = None;
        let mut skipped = None;
        for range in self.ranges(text) {
            if skip.as_ref() == Some(&range) {
                skipped = Some(range);
                continue;
            }
            if first.is_none() {
                first = Some(range.clone());
            }
            if !backwards && range.start >= start {
                return Some(range);
            }
            if backwards && range.end <= start {
                previous = Some(range.clone());
            }
            last = Some(range);
        }
        if backwards {
            previous.or(last).or(skipped)
        } else {
            first.or(skipped)
        }
    }
    pub fn replacement(
        &self,
        text: &str,
        range: Range<usize>,
        replacement: &str,
        expand: bool,
    ) -> Option<String> {
        let captures = self.0.captures_at(text, range.start)?;
        if captures.get(0)?.range() != range {
            return None;
        }
        let mut output = String::new();
        if expand {
            captures.expand(replacement, &mut output);
        } else {
            output.push_str(replacement);
        }
        Some(output)
    }
    pub fn replace(
        &self,
        text: &str,
        replacement: &str,
        expand: bool,
        limit: usize,
    ) -> Result<Option<(String, usize)>, String> {
        self.replacement_plan(text, replacement, expand, limit)
            .map(|plan| plan.map(|(text, edits)| (text, edits.len())))
    }
    pub fn replacement_plan(
        &self,
        text: &str,
        replacement: &str,
        expand: bool,
        limit: usize,
    ) -> Result<Option<(String, Vec<scarlet_ui::prelude::EditDelta>)>, String> {
        let mut output = String::new();
        let mut end = 0;
        let mut edits = Vec::new();
        for captures in self.0.captures_iter(text) {
            if edits.len() >= 100_000 {
                return Err("Replacement exceeds 100,000 edits; narrow the search".into());
            }
            let found = captures.get(0).unwrap();
            let mut inserted = String::new();
            if expand {
                captures.expand(replacement, &mut inserted);
            } else {
                inserted.push_str(replacement);
            }
            if output
                .len()
                .saturating_add(found.start() - end)
                .saturating_add(inserted.len())
                > limit
            {
                return Err("Replacement would exceed the document size limit".into());
            }
            output.push_str(&text[end..found.start()]);
            output.push_str(&inserted);
            end = found.end();
            edits.push(scarlet_ui::prelude::EditDelta {
                replaced_range: found.range(),
                inserted,
            });
        }
        if edits.is_empty() {
            return Ok(None);
        }
        if output.len().saturating_add(text.len() - end) > limit {
            return Err("Replacement would exceed the document size limit".into());
        }
        output.push_str(&text[end..]);
        Ok(Some((output, edits)))
    }
}

pub fn find(text: &str, query: &str, start: usize, backwards: bool) -> Option<Range<usize>> {
    if query.is_empty() {
        return None;
    }
    let mut start = start.min(text.len());
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let offset = if backwards {
        text[..start].rfind(query).or_else(|| text.rfind(query))
    } else {
        text[start..]
            .find(query)
            .map(|i| start + i)
            .or_else(|| text.find(query))
    }?;
    Some(offset..offset + query.len())
}

pub fn replace_all(
    text: &str,
    query: &str,
    replacement: &str,
    limit: usize,
) -> Result<Option<(String, usize)>, String> {
    if query.is_empty() {
        return Ok(None);
    }
    let count = text.matches(query).count();
    if count == 0 {
        return Ok(None);
    }
    let size = text.len().checked_sub(count * query.len()).and_then(|n| {
        replacement
            .len()
            .checked_mul(count)
            .and_then(|r| n.checked_add(r))
    });
    if size.is_none_or(|size| size > limit) {
        return Err("Replacement would exceed the 16 MiB document limit".into());
    }
    Ok(Some((text.replace(query, replacement), count)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn next_wraps_to_the_only_match_including_zero_width() {
        let pattern = Pattern::new("猫", Options::default()).unwrap();
        for backwards in [false, true] {
            assert_eq!(pattern.next("a猫b", 4, backwards, Some(1..4)), Some(1..4));
        }
        let pattern = Pattern::new(
            "^",
            Options {
                regex: true,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(pattern.next("one line", 0, false, Some(0..0)), Some(0..0));
    }
    #[test]
    fn regex_captures_case_words_and_zero_width_matches_are_bounded() {
        let p = Pattern::new(
            "cat",
            Options {
                match_case: false,
                whole_word: true,
                regex: false,
            },
        )
        .unwrap();
        assert_eq!(
            p.ranges("Cat scatter cat").collect::<Vec<_>>(),
            [0..3, 12..15]
        );
        let p = Pattern::new(
            r"(猫)(\d+)",
            Options {
                regex: true,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(
            p.replace("猫12 猫3", "$2$1", true, 30).unwrap(),
            Some(("12猫 3猫".into(), 2))
        );
        assert!(p.replace("猫12", "longer", false, 1).is_err());
        assert!(
            Pattern::new(
                "[",
                Options {
                    regex: true,
                    ..Options::default()
                }
            )
            .is_err()
        );
        let p = Pattern::new(
            "^",
            Options {
                regex: true,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(p.next("a\nb", 0, false, Some(0..0)), Some(2..2));
        assert_eq!(
            p.replace("a\nb", ">", true, 10).unwrap(),
            Some((">a\n>b".into(), 2))
        );
    }
    #[test]
    fn unicode_search_wraps_both_directions_and_empty_query_is_safe() {
        let text = "猫 a 猫";
        assert_eq!(find(text, "猫", 3, false), Some(6..9));
        assert_eq!(find(text, "猫", 9, false), Some(0..3));
        assert_eq!(find(text, "猫", 6, true), Some(0..3));
        assert_eq!(find(text, "猫", 0, true), Some(6..9));
        assert_eq!(find(text, "", 0, false), None);
        assert_eq!(find("abc", "abc", 1, false), Some(0..3));
        assert_eq!(find("abc", "abc", 1, true), Some(0..3));
        assert_eq!(
            replace_all(text, "猫", "🍁", 20).unwrap(),
            Some(("🍁 a 🍁".into(), 2))
        );
        assert!(replace_all(text, "猫", "much longer", 10).is_err());
    }
}
