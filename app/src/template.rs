//! Status text templates, truncation and the profanity filter.

use std::collections::HashSet;

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
///
/// Details of the cleanup around empty values (a value that is missing, empty
/// or only whitespace):
/// - Values are inserted with their own surrounding whitespace trimmed and are
///   never expanded again, so a title containing `{line}` stays as written.
/// - A separator is one of `·` `•` `-` `–` `—` `|` `/` `:` `,` standing on its
///   own (whitespace or a placeholder on both sides), so `Now playing: {line}`
///   keeps its colon.
/// - An empty value with a separator on each side keeps only one:
///   `{title} · {album} · {artist}` with no album renders as `Title · Artist`.
/// - An empty value with a separator on one side only drops it unless a value
///   stands on the other side: `🎵 {artist} - {title}` with no artist renders as
///   `🎵 Title`, while `{title} {album} · {artist}` keeps its `·`.
/// - Brackets directly around an empty value are dropped: `{title} ({album})`
///   with no album renders as just the title.
/// - Separators written in the template with no empty value next to them are
///   kept as written.
pub fn render(template: &str, ctx: &TemplateContext<'_>) -> String {
    let mut pieces = tokenize(template, ctx);
    drop_empty_brackets(&mut pieces);
    let mut pieces = merge_empty_runs(pieces);
    drop_dangling_separators(&mut pieces);
    join(&pieces)
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
        .next_back();
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
///
/// One `*` replaces each remaining character (not byte), and the first
/// character keeps its original case. List entries are trimmed and compared
/// case-insensitively; empty entries are ignored. An entry that is not a single
/// alphanumeric word (for example `f-word`) can never match.
pub fn filter_profanity(s: &str, words: &[String]) -> String {
    let banned: HashSet<String> = words
        .iter()
        .map(|word| word.trim().to_lowercase())
        .filter(|word| !word.is_empty())
        .collect();
    if banned.is_empty() {
        return s.to_string();
    }

    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(first) = rest.chars().next() {
        let in_word = first.is_alphanumeric();
        // The run ends where alphanumeric-ness changes.
        let end = rest
            .find(|c: char| c.is_alphanumeric() != in_word)
            .unwrap_or(rest.len());
        let (Some(run), Some(tail)) = (rest.get(..end), rest.get(end..)) else {
            // Unreachable (`find` returns a character boundary); never panic.
            out.push_str(rest);
            break;
        };
        if in_word && banned.contains(&run.to_lowercase()) {
            out.push(first);
            for _ in run.chars().skip(1) {
                out.push('*');
            }
        } else {
            out.push_str(run);
        }
        rest = tail;
    }
    out
}

/// The built-in word list used when the profanity filter is on and the user
/// has not listed their own words. Short, common English words only.
pub fn default_profanity_words() -> Vec<String> {
    const WORDS: &[&str] = &[
        "fuck",
        "fucks",
        "fucked",
        "fucker",
        "fuckers",
        "fuckin",
        "fucking",
        "motherfucker",
        "motherfuckers",
        "motherfuckin",
        "motherfucking",
        "shit",
        "shits",
        "shitty",
        "bullshit",
        "bitch",
        "bitches",
        "ass",
        "asshole",
        "assholes",
        "dick",
        "dicks",
        "cock",
        "cocks",
        "cunt",
        "cunts",
        "pussy",
        "bastard",
        "bastards",
        "damn",
        "goddamn",
        "whore",
        "whores",
        "slut",
        "sluts",
        "nigga",
        "niggas",
        "nigger",
        "niggers",
        "fag",
        "faggot",
        "faggots",
    ];
    WORDS.iter().map(|word| word.to_string()).collect()
}

// ---------------------------------------------------------------------------
// Template rendering helpers
// ---------------------------------------------------------------------------

/// Characters that count as a separator when they stand on their own.
const SEPARATORS: &[char] = &['·', '•', '-', '–', '—', '|', '/', ':', ','];

/// One piece of a parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece<'a> {
    /// Literal template text, with escapes and unknown placeholders resolved.
    Lit(String),
    /// A placeholder with a non-empty value (trimmed).
    Val(&'a str),
    /// A placeholder whose value is missing or blank. `space` records that
    /// whitespace sat inside a run of empty values that was merged into this one.
    Empty { space: bool },
}

fn tokenize<'a>(template: &str, ctx: &TemplateContext<'a>) -> Vec<Piece<'a>> {
    let mut pieces = Vec::new();
    let mut lit = String::new();
    let mut rest = template;
    while let Some(brace) = rest.find(['{', '}']) {
        let (Some(before), Some(tail)) = (rest.get(..brace), rest.get(brace..)) else {
            break;
        };
        lit.push_str(before);
        if let Some(after) = tail.strip_prefix("{{") {
            lit.push('{');
            rest = after;
        } else if let Some(after) = tail.strip_prefix("}}") {
            lit.push('}');
            rest = after;
        } else if let Some(after) = tail.strip_prefix('{') {
            match split_placeholder(after) {
                Some((name, after_close)) => {
                    match lookup(name, ctx) {
                        Some(value) => {
                            if !lit.is_empty() {
                                pieces.push(Piece::Lit(std::mem::take(&mut lit)));
                            }
                            let value = value.trim();
                            pieces.push(if value.is_empty() {
                                Piece::Empty { space: false }
                            } else {
                                Piece::Val(value)
                            });
                        }
                        None => {
                            lit.push('{');
                            lit.push_str(name);
                            lit.push('}');
                        }
                    }
                    rest = after_close;
                }
                None => {
                    // An opening brace with no closing one is literal text.
                    lit.push('{');
                    rest = after;
                }
            }
        } else if let Some(after) = tail.strip_prefix('}') {
            // A lone closing brace is literal text.
            lit.push('}');
            rest = after;
        } else {
            rest = tail;
            break;
        }
    }
    lit.push_str(rest);
    if !lit.is_empty() {
        pieces.push(Piece::Lit(lit));
    }
    pieces
}

/// For the text after `{`: the placeholder name and the text after its `}`.
/// `None` when another `{` comes first or there is no `}`.
fn split_placeholder(after_open: &str) -> Option<(&str, &str)> {
    let end = after_open.find(['{', '}'])?;
    let name = after_open.get(..end)?;
    let after_close = after_open.get(end..)?.strip_prefix('}')?;
    Some((name, after_close))
}

/// The value of a known placeholder (missing optional values are ""), or
/// `None` for an unknown name.
fn lookup<'a>(name: &str, ctx: &TemplateContext<'a>) -> Option<&'a str> {
    match name {
        "line" => Some(ctx.line.unwrap_or("")),
        "next" => Some(ctx.next.unwrap_or("")),
        "title" => Some(ctx.title),
        "artist" => Some(ctx.artist),
        "album" => Some(ctx.album.unwrap_or("")),
        _ => None,
    }
}

/// `({album})` with no album: drops the `(` and `)` around the empty value.
fn drop_empty_brackets(pieces: &mut [Piece<'_>]) {
    for i in 1..pieces.len().saturating_sub(1) {
        if !matches!(pieces.get(i), Some(Piece::Empty { .. })) {
            continue;
        }
        let (left, right) = pieces.split_at_mut(i);
        let (Some(Piece::Lit(l)), Some(Piece::Lit(r))) = (left.last_mut(), right.get_mut(1)) else {
            continue;
        };
        let close = match l.chars().next_back() {
            Some('(') => ')',
            Some('[') => ']',
            _ => continue,
        };
        if let Some(after) = r.strip_prefix(close) {
            *r = after.to_string();
            l.pop();
        }
    }
}

/// Drops empty literals and merges empty values separated only by whitespace,
/// so the passes below see each run of empty values as one.
fn merge_empty_runs(pieces: Vec<Piece<'_>>) -> Vec<Piece<'_>> {
    let mut out: Vec<Piece<'_>> = Vec::with_capacity(pieces.len());
    for piece in pieces {
        match piece {
            Piece::Lit(s) if s.is_empty() => {}
            Piece::Empty { space } => {
                let whitespace_between = matches!(out.last(), Some(Piece::Lit(s)) if s.trim().is_empty())
                    && matches!(
                        out.len().checked_sub(2).and_then(|i| out.get(i)),
                        Some(Piece::Empty { .. })
                    );
                if whitespace_between {
                    out.pop();
                }
                match out.last_mut() {
                    Some(Piece::Empty { space: previous }) => {
                        *previous = *previous || space || whitespace_between;
                    }
                    _ => out.push(Piece::Empty { space }),
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Removes the separators that empty values leave dangling. For each empty
/// value, the nearest value or text on each side is found (whitespace and
/// other empty values are skipped; the template's start and end count as
/// nothing):
/// - a separator on both sides: the right one goes, so `A · {x} · B` → `A · B`
///   and `A · {x} · {y} · B` with both empty also keeps a single separator;
/// - a separator on one side only: it goes, unless a value stands on the other
///   side. `{x} - B`, `A · {x}` and `🎵 {x} - B` lose it; `A {x} · B` keeps it,
///   because it still separates two values.
fn drop_dangling_separators(pieces: &mut [Piece<'_>]) {
    // Index of the nearest value or non-blank literal to the left.
    let mut left: Option<usize> = None;
    for i in 0..pieces.len() {
        match pieces.get(i) {
            Some(Piece::Val(_)) => left = Some(i),
            Some(Piece::Lit(s)) => {
                if !s.trim().is_empty() {
                    left = Some(i);
                }
            }
            Some(Piece::Empty { .. }) => {
                let right = (i.saturating_add(1)..pieces.len()).find(|&j| match pieces.get(j) {
                    Some(Piece::Val(_)) => true,
                    Some(Piece::Lit(s)) => !s.trim().is_empty(),
                    _ => false,
                });
                let left_rest =
                    left.and_then(|j| lit_at(pieces, j).and_then(strip_trailing_separator));
                let right_rest =
                    right.and_then(|j| lit_at(pieces, j).and_then(strip_leading_separator));
                let value_at = |at: Option<usize>| {
                    matches!(at.and_then(|j| pieces.get(j)), Some(Piece::Val(_)))
                };
                let (left_is_value, right_is_value) = (value_at(left), value_at(right));
                let replace = match (left_rest, right_rest) {
                    (Some(_), Some(rest)) => right.map(|j| (j, rest)),
                    (Some(rest), None) if !right_is_value => left.map(|j| (j, rest)),
                    (None, Some(rest)) if !left_is_value => right.map(|j| (j, rest)),
                    _ => None,
                };
                if let Some((j, rest)) = replace {
                    if let Some(piece) = pieces.get_mut(j) {
                        *piece = Piece::Lit(rest);
                    }
                }
            }
            None => break,
        }
    }
}

/// The text of the literal at `index`, if that piece is a literal.
fn lit_at<'p>(pieces: &'p [Piece<'_>], index: usize) -> Option<&'p str> {
    match pieces.get(index) {
        Some(Piece::Lit(s)) => Some(s),
        _ => None,
    }
}

/// `" - Title"`-style text without its leading separator, or `None` when it
/// does not start with a standalone separator.
fn strip_leading_separator(s: &str) -> Option<String> {
    let mut chars = s.trim_start().chars();
    let first = chars.next()?;
    let after = chars.as_str();
    let standalone = after.is_empty() || after.starts_with(char::is_whitespace);
    (SEPARATORS.contains(&first) && standalone).then(|| after.to_string())
}

/// Mirror of [`strip_leading_separator`] for the end of `s`.
fn strip_trailing_separator(s: &str) -> Option<String> {
    let mut chars = s.trim_end().chars();
    let last = chars.next_back()?;
    let before = chars.as_str();
    let standalone = before.is_empty() || before.ends_with(char::is_whitespace);
    (SEPARATORS.contains(&last) && standalone).then(|| before.to_string())
}

/// Concatenates the pieces. Whitespace touching an empty value (on either
/// side) collapses to a single space; the result is trimmed.
fn join(pieces: &[Piece<'_>]) -> String {
    let mut out = String::new();
    let mut after_empty = false;
    let mut pending_space = false;
    for piece in pieces {
        let text: &str = match piece {
            Piece::Empty { space } => {
                let kept = out.trim_end().len();
                if kept < out.len() || *space {
                    pending_space = true;
                }
                out.truncate(kept);
                after_empty = true;
                continue;
            }
            Piece::Lit(s) => s,
            Piece::Val(v) => v,
        };
        let text = if after_empty {
            let trimmed = text.trim_start();
            if trimmed.len() < text.len() {
                pending_space = true;
            }
            trimmed
        } else {
            text
        };
        if text.is_empty() {
            continue;
        }
        if pending_space && !out.is_empty() {
            out.push(' ');
        }
        pending_space = false;
        after_empty = false;
        out.push_str(text);
    }
    out.trim().to_string()
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
        assert_eq!(
            truncate_chars("never gonna give you up", 15),
            "never gonna…"
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(
        line: Option<&'a str>,
        next: Option<&'a str>,
        title: &'a str,
        artist: &'a str,
        album: Option<&'a str>,
    ) -> TemplateContext<'a> {
        TemplateContext {
            line,
            next,
            title,
            artist,
            album,
        }
    }

    /// Everything filled in.
    fn full() -> TemplateContext<'static> {
        ctx(
            Some("the line"),
            Some("the next line"),
            "Title",
            "Artist",
            Some("Album"),
        )
    }

    /// Only the title.
    fn title_only() -> TemplateContext<'static> {
        ctx(None, None, "Title", "", None)
    }

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| w.to_string()).collect()
    }

    // ---- render: placeholders ---------------------------------------------

    #[test]
    fn renders_every_placeholder() {
        assert_eq!(
            render("{line}|{next}|{title}|{artist}|{album}", &full()),
            "the line|the next line|Title|Artist|Album"
        );
    }

    #[test]
    fn assignment_examples() {
        assert_eq!(render("{title} · {album}", &title_only()), "Title");
        assert_eq!(render("{artist} - {title}", &title_only()), "Title");
        let with_line = ctx(Some("the line"), None, "Title", "Artist", None);
        assert_eq!(render("🎵 {line}", &with_line), "🎵 the line");
    }

    #[test]
    fn default_templates() {
        assert_eq!(render("🎵 {line}", &full()), "🎵 the line");
        assert_eq!(render("{title} · {artist}", &full()), "Title · Artist");
        assert_eq!(render("{title} · {artist}", &title_only()), "Title");
        let no_title = ctx(None, None, "", "Artist", None);
        assert_eq!(render("{title} · {artist}", &no_title), "Artist");
        assert_eq!(
            render("{title} · {artist}", &TemplateContext::default()),
            ""
        );
    }

    #[test]
    fn missing_optional_values_render_empty() {
        let c = title_only();
        assert_eq!(render("[{line}]", &c), "");
        assert_eq!(render("a{next}b", &c), "ab");
        assert_eq!(render("{album}", &c), "");
        assert_eq!(render("🎵 {line}", &c), "🎵");
    }

    #[test]
    fn whitespace_only_values_count_as_empty() {
        let c = ctx(Some("   "), None, "Title", " \t", Some(""));
        assert_eq!(render("{title} - {artist}", &c), "Title");
        assert_eq!(render("{line} · {title}", &c), "Title");
    }

    #[test]
    fn values_are_trimmed_and_inserted_verbatim() {
        let c = ctx(Some("  {title} and {{x}}  "), None, " Title ", "A", None);
        assert_eq!(render("🎵 {line}", &c), "🎵 {title} and {{x}}");
        assert_eq!(render("[{title}]", &c), "[Title]");
        // Inner whitespace of a value is left alone.
        let c = ctx(Some("so   much  space"), None, "T", "A", None);
        assert_eq!(render("{line}", &c), "so   much  space");
    }

    #[test]
    fn template_without_placeholders() {
        assert_eq!(render("Listening to music", &full()), "Listening to music");
        assert_eq!(render("  padded  ", &full()), "padded");
        assert_eq!(render("", &full()), "");
        assert_eq!(render("   ", &full()), "");
    }

    #[test]
    fn same_placeholder_twice() {
        assert_eq!(render("{title} {title}", &full()), "Title Title");
    }

    // ---- render: braces ------------------------------------------------------

    #[test]
    fn double_braces_are_literal() {
        assert_eq!(render("{{line}}", &full()), "{line}");
        assert_eq!(render("{{{line}}}", &full()), "{the line}");
        assert_eq!(render("{{", &full()), "{");
        assert_eq!(render("}}", &full()), "}");
        assert_eq!(render("a {{ b }} c", &full()), "a { b } c");
    }

    #[test]
    fn unknown_placeholders_are_left_unchanged() {
        assert_eq!(render("{foo} {title}", &full()), "{foo} Title");
        assert_eq!(render("{ line }", &full()), "{ line }");
        assert_eq!(render("{Line}", &full()), "{Line}");
        assert_eq!(render("{}", &full()), "{}");
        assert_eq!(render("{🎵}", &full()), "{🎵}");
    }

    #[test]
    fn stray_braces_are_literal() {
        assert_eq!(render("{line", &full()), "{line");
        assert_eq!(render("line}", &full()), "line}");
        assert_eq!(render("{", &full()), "{");
        assert_eq!(render("}", &full()), "}");
        assert_eq!(render("{li{ne}", &full()), "{li{ne}");
        assert_eq!(render("{{line}", &full()), "{line}");
        assert_eq!(render("{line}}", &full()), "the line}");
        assert_eq!(render("{x{title}", &full()), "{xTitle");
    }

    // ---- render: whitespace ----------------------------------------------------

    #[test]
    fn whitespace_left_by_empty_values_collapses() {
        let c = ctx(Some("L"), None, "T", "A", None);
        assert_eq!(render("{title} {album} {artist}", &c), "T A");
        assert_eq!(render("{title}   {album}   {artist}", &c), "T A");
        assert_eq!(render("{title} {album}{next} {artist}", &c), "T A");
        assert_eq!(render("{title}{album} {next}{artist}", &c), "T A");
        assert_eq!(render("{title}{album}{artist}", &c), "TA");
        assert_eq!(render("  {album}  {title}  ", &c), "T");
    }

    #[test]
    fn whitespace_not_next_to_an_empty_value_is_kept() {
        assert_eq!(render("{title}   {artist}", &full()), "Title   Artist");
    }

    // ---- render: separators ------------------------------------------------------

    #[test]
    fn dangling_separators_at_the_end_are_trimmed() {
        let c = title_only();
        for sep in ["·", "-", "—", "|", "–", "•", "/", ":", ","] {
            let template = format!("{{title}} {sep} {{album}}");
            assert_eq!(render(&template, &c), "Title", "{template}");
        }
    }

    #[test]
    fn dangling_separators_at_the_start_are_trimmed() {
        let c = title_only();
        for sep in ["·", "-", "—", "|", "–", "•", "/", ":", ","] {
            let template = format!("{{artist}} {sep} {{title}}");
            assert_eq!(render(&template, &c), "Title", "{template}");
        }
    }

    #[test]
    fn separators_without_spaces_next_to_placeholders() {
        let c = title_only();
        assert_eq!(render("{artist}-{title}", &c), "Title");
        assert_eq!(render("{title}—{album}", &c), "Title");
        assert_eq!(render("{title} -{album}", &c), "Title");
    }

    #[test]
    fn several_empty_values_at_the_edges() {
        let c = title_only();
        assert_eq!(render("{artist} - {album} - {title}", &c), "Title");
        assert_eq!(render("{title} · {album} · {artist}", &c), "Title");
        assert_eq!(
            render("{line} | {next} | {title} | {album} | {artist}", &c),
            "Title"
        );
        assert_eq!(render("{artist}{album} - {title}", &c), "Title");
    }

    #[test]
    fn empty_value_between_separators_keeps_one() {
        let c = ctx(None, None, "Title", "Artist", None);
        assert_eq!(render("{title} · {album} · {artist}", &c), "Title · Artist");
        assert_eq!(render("{title} - {album} | {artist}", &c), "Title - Artist");
        assert_eq!(
            render("{title} · {album} · {next} · {artist}", &c),
            "Title · Artist"
        );
    }

    #[test]
    fn separators_next_to_decoration_are_dropped() {
        // Text such as an emoji on the far side of the empty value is not a
        // value, so the separator has nothing to separate.
        let no_artist = title_only();
        assert_eq!(render("🎵 {artist} - {title}", &no_artist), "🎵 Title");
        assert_eq!(render("{title} · {artist} 🎶", &no_artist), "Title 🎶");
        assert_eq!(
            render("Listening to {artist} - {title}", &no_artist),
            "Listening to Title"
        );
        assert_eq!(
            render("🎵 {artist} - {title} 🎶", &no_artist),
            "🎵 Title 🎶"
        );
        let next_only = ctx(None, Some("next"), "Title", "Artist", None);
        assert_eq!(render("🎵 {line} · {next}", &next_only), "🎵 next");
        assert_eq!(render("🎵 {next} · {line} 🎶", &next_only), "🎵 next 🎶");
    }

    #[test]
    fn separators_between_values_survive_an_empty_neighbour() {
        let c = ctx(None, None, "Title", "Artist", None);
        // A value on the far side of the empty one: the separator still
        // separates two values.
        assert_eq!(render("{title} {album} · {artist}", &c), "Title · Artist");
        assert_eq!(render("{title} · {album} {artist}", &c), "Title · Artist");
        assert_eq!(render("{title}{album} - {artist}", &c), "Title - Artist");
        assert_eq!(render("{title} ({album}) - {artist}", &c), "Title - Artist");
        // A separator on both sides keeps one, even next to plain text.
        assert_eq!(render("{title} · {album} · 🎶", &c), "Title · 🎶");
        assert_eq!(render("Lyrix | {album} | {title}", &c), "Lyrix | Title");
    }

    #[test]
    fn all_values_empty_renders_empty() {
        let c = TemplateContext::default();
        assert_eq!(render("{artist} - {title}", &c), "");
        assert_eq!(render("{title} · {album} · {artist}", &c), "");
        assert_eq!(render("{line}", &c), "");
    }

    #[test]
    fn separators_written_without_an_empty_value_are_kept() {
        let c = full();
        assert_eq!(render("- {line}", &c), "- the line");
        assert_eq!(render("{line} |", &c), "the line |");
        assert_eq!(render("{title} - {artist}", &c), "Title - Artist");
        assert_eq!(
            render("{title} · {album} · {artist}", &c),
            "Title · Album · Artist"
        );
    }

    #[test]
    fn words_ending_in_punctuation_are_not_separators() {
        let c = title_only();
        assert_eq!(render("Now playing: {line}", &c), "Now playing:");
        assert_eq!(render("{line} -ish", &c), "-ish");
        // `'s` is not a separator; the ` - ` next to the empty artist is.
        assert_eq!(render("{title} - {artist}'s pick", &c), "Title 's pick");
        assert_eq!(
            render("{title} - {artist}'s pick", &full()),
            "Title - Artist's pick"
        );
    }

    #[test]
    fn brackets_around_empty_values_are_dropped() {
        assert_eq!(render("{title} ({album})", &title_only()), "Title");
        assert_eq!(render("{title} [{album}]", &title_only()), "Title");
        assert_eq!(render("{title} · ({album})", &title_only()), "Title");
        assert_eq!(render("{title} ({album})", &full()), "Title (Album)");
        // Not directly around the value: left alone.
        assert_eq!(render("{title} ( {album} )", &title_only()), "Title ( )");
    }

    #[test]
    fn unicode_templates() {
        let c = ctx(Some("ქართული ენა"), None, "სიმღერა", "", None);
        assert_eq!(render("🎵 {line} 🎶", &c), "🎵 ქართული ენა 🎶");
        assert_eq!(render("{artist} — {title}", &c), "სიმღერა");
        assert_eq!(render("{title} · {album}", &c), "სიმღერა");
    }

    #[test]
    fn never_panics_on_odd_templates() {
        let templates = [
            "{",
            "}",
            "{{{",
            "}}}",
            "{{}}",
            "{line}{",
            "}{line}",
            "{🎵{line}🎵}",
            "·",
            " · ",
            "{line}·{line}",
            "( {line} )",
            "({line}",
            "{line})",
            "-{album}-",
            "{album}",
        ];
        let contexts = [full(), title_only(), TemplateContext::default()];
        for template in templates {
            for c in &contexts {
                let _ = render(template, c);
            }
        }
        let tricky = "🎵 {title} · ({album}) — {{x}} {line";
        for (i, _) in tricky.char_indices() {
            let _ = render(&tricky[..i], &title_only());
            let _ = render(&tricky[i..], &full());
        }
    }

    // ---- filter_profanity -------------------------------------------------------

    #[test]
    fn masks_listed_words() {
        let list = words(&["fuck", "shit"]);
        assert_eq!(filter_profanity("what the fuck", &list), "what the f***");
        assert_eq!(filter_profanity("shit happens", &list), "s*** happens");
        assert_eq!(filter_profanity("fuck fuck", &list), "f*** f***");
    }

    #[test]
    fn matching_is_case_insensitive_and_keeps_the_first_character() {
        let list = words(&["fuck"]);
        assert_eq!(filter_profanity("FUCK", &list), "F***");
        assert_eq!(filter_profanity("Fuck", &list), "F***");
        assert_eq!(filter_profanity("fUcK", &list), "f***");
        let upper_list = words(&["FUCK"]);
        assert_eq!(filter_profanity("fuck", &upper_list), "f***");
    }

    #[test]
    fn only_whole_words_match() {
        let list = words(&["ass"]);
        assert_eq!(filter_profanity("class", &list), "class");
        assert_eq!(filter_profanity("assassin", &list), "assassin");
        assert_eq!(filter_profanity("bass ass", &list), "bass a**");
        assert_eq!(filter_profanity("ass2", &list), "ass2");
        assert_eq!(filter_profanity("assé", &list), "assé");
    }

    #[test]
    fn punctuation_and_symbols_are_word_boundaries() {
        let list = words(&["fuck", "shit"]);
        assert_eq!(filter_profanity("fuck!", &list), "f***!");
        assert_eq!(filter_profanity("(shit)", &list), "(s***)");
        assert_eq!(filter_profanity("fuck's sake", &list), "f***'s sake");
        assert_eq!(filter_profanity("fuck_this", &list), "f***_this");
        assert_eq!(filter_profanity("shit-faced", &list), "s***-faced");
        assert_eq!(filter_profanity("🎵fuck🎵", &list), "🎵f***🎵");
        assert_eq!(filter_profanity("  fuck  ", &list), "  f***  ");
    }

    #[test]
    fn unicode_words_and_first_characters() {
        let list = words(&["ёлка", "სიტყვა"]);
        assert_eq!(filter_profanity("Ёлка!", &list), "Ё***!");
        assert_eq!(filter_profanity("ЁЛКА", &list), "Ё***");
        assert_eq!(filter_profanity("ეს სიტყვა არის", &list), "ეს ს***** არის");
        // Accented letters belong to the word.
        assert_eq!(filter_profanity("ёлкаé", &list), "ёлкаé");
    }

    #[test]
    fn single_character_words_stay_visible() {
        let list = words(&["a"]);
        assert_eq!(filter_profanity("a b A", &list), "a b A");
    }

    #[test]
    fn empty_inputs() {
        assert_eq!(filter_profanity("", &words(&["fuck"])), "");
        assert_eq!(filter_profanity("fuck", &[]), "fuck");
        assert_eq!(filter_profanity("fuck", &words(&["", "  "])), "fuck");
    }

    #[test]
    fn list_entries_are_trimmed_and_multi_word_entries_never_match() {
        assert_eq!(filter_profanity("fuck", &words(&["  fuck "])), "f***");
        assert_eq!(
            filter_profanity("f-word here", &words(&["f-word"])),
            "f-word here"
        );
        assert_eq!(
            filter_profanity("son of a gun", &words(&["son of a gun"])),
            "son of a gun"
        );
    }

    #[test]
    fn text_without_banned_words_is_unchanged() {
        let list = default_profanity_words();
        let text = "Never gonna give you up, never gonna let you down 🎵";
        assert_eq!(filter_profanity(text, &list), text);
    }

    // ---- default_profanity_words -------------------------------------------------

    #[test]
    fn default_list_is_clean() {
        let list = default_profanity_words();
        assert!(!list.is_empty());
        let unique: HashSet<&String> = list.iter().collect();
        assert_eq!(unique.len(), list.len(), "no duplicates");
        for word in &list {
            assert!(!word.is_empty());
            assert_eq!(word, &word.to_lowercase(), "{word} is lowercase");
            assert!(
                word.chars().all(char::is_alphanumeric),
                "{word} is one word"
            );
            assert!(word.is_ascii(), "{word} is English");
        }
        assert!(list.iter().any(|w| w == "fuck"));
        assert!(list.iter().any(|w| w == "shit"));
    }

    #[test]
    fn default_list_masks_common_words() {
        let list = default_profanity_words();
        assert_eq!(
            filter_profanity("What the FUCK, this shit is fucking crazy", &list),
            "What the F***, this s*** is f****** crazy"
        );
        assert_eq!(
            filter_profanity("first class bass", &list),
            "first class bass"
        );
    }

    // ---- truncate_chars (extra cases) ----------------------------------------------

    #[test]
    fn truncate_handles_huge_limits_and_exact_lengths() {
        assert_eq!(truncate_chars("hello", usize::MAX), "hello");
        assert_eq!(truncate_chars("hello", 6), "hello");
        assert_eq!(truncate_chars("hello world", 6), "hello…");
        assert_eq!(truncate_chars("🎵", 1), "🎵");
        assert_eq!(truncate_chars("🎵🎵", 1), "…");
    }

    #[test]
    fn truncate_result_never_exceeds_the_limit() {
        let s = "We're no strangers to love, you know the rules and so do I 🎵 ქართული";
        for max in 0..=s.chars().count() + 2 {
            let out = truncate_chars(s, max);
            assert!(out.chars().count() <= max, "{max}: {out}");
        }
    }
}
