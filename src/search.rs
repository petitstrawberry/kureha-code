//! Literal, case-sensitive search. Byte ranges remain valid for Unicode text.
use std::ops::Range;

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
