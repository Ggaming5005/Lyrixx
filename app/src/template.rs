//! Status text templates, truncation and the profanity filter.

/// Values available to a status template.
#[derive(Debug, Clone, Default)]
pub struct TemplateContext<'a> {
    /// `{line}`: the current lyric line.
    pub line: Option<&'a str>,
    /// `{next}`: the next lyric line.
    pub next: Option<&'a str>,
    /// `{title}`
    pub title: &'a str,
    /// `{artist}`
    pub artist: &'a str,
    /// `{album}`
    pub album: Option<&'a str>,
}

/// Renders a template such as `🎵 {line}` or `{title} · {artist}`.
///
/// - Placeholders: `{line}`, `{next}`, `{title}`, `{artist}`, `{album}`.
///   A missing optional value renders as an empty string.
/// - `{{` and `}}` render literal braces.
/// - Unknown placeholders such as `{foo}` are left unchanged.
/// - The result has leading/trailing whitespace trimmed, and any run of
///   whitespace left by an empty value collapses to a single space.
/// - Separators left dangling by an empty value at the start or end
///   (` · `, ` - `, ` — `, ` | `) are trimmed, so `{title} · {album}` with no
///   album renders as just the title.
pub fn render(template: &str, ctx: &TemplateContext<'_>) -> String {
    let _ = (template, ctx);
    todo!()
}

/// Shortens `s` to at most `max_chars` Unicode scalar values, ending with `…`
/// when it had to cut. Never splits a character. Prefers cutting at the last
/// space within the final 30% of the allowed length. `max_chars == 0` returns "".
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    // Leave room for the ellipsis.
    let keep = max_chars - 1;
    let head: String = s.chars().take(keep).collect();
    // Prefer a word boundary within the last 30% of the allowed length.
    let min_cut = keep - keep * 3 / 10;
    let cut = head
        .char_indices()
        .filter(|&(i, c)| c == ' ' && i > 0 && head[..i].chars().count() >= min_cut)
        .map(|(i, _)| i)
        .last();
    let mut out = match cut {
        Some(i) => head[..i].trim_end().to_string(),
        None => head.trim_end().to_string(),
    };
    out.push('…');
    out
}

/// Masks each listed word (case-insensitive, whole words only) by keeping its
/// first character and replacing the rest with `*`. Words are matched on
/// Unicode word boundaries (alphanumeric runs), so `class` is not touched by `ass`.
pub fn filter_profanity(s: &str, words: &[String]) -> String {
    let _ = (s, words);
    todo!()
}

/// The built-in word list used when the profanity filter is on and the user
/// has not listed their own words. Short, common English words only.
pub fn default_profanity_words() -> Vec<String> {
    todo!()
}

#[cfg(test)]
mod truncate_tests {
    use super::truncate_chars;

    #[test]
    fn short_strings_are_untouched() {
        assert_eq!(truncate_chars("hello", 5), "hello");
        assert_eq!(truncate_chars("", 0), "");
    }

    #[test]
    fn cuts_at_a_word_boundary_near_the_end() {
        assert_eq!(truncate_chars("never gonna give you up", 15), "never gonna…");
    }

    #[test]
    fn cuts_mid_word_when_no_space_is_close() {
        assert_eq!(truncate_chars("abcdefghij", 5), "abcd…");
        assert_eq!(truncate_chars("abc", 0), "");
        assert_eq!(truncate_chars("abc", 1), "…");
    }

    #[test]
    fn never_splits_multibyte_characters() {
        let s = "🎵🎵🎵🎵🎵";
        assert_eq!(truncate_chars(s, 3), "🎵🎵…");
        assert_eq!(truncate_chars("ქართული ენა", 8), "ქართული…");
    }
}
