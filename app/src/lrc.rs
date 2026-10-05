//! Parsing LRC lyric files and finding the line for a song position.
//!
//! Supported LRC features:
//! - `[mm:ss.xx]`, `[mm:ss.xxx]`, `[mm:ss]` and `[m:ss.x]` timestamps; minutes may exceed 59.
//! - Several timestamps on one line (`[00:10.00][01:20.00]chorus`) expand to one line per stamp.
//! - `[offset:+250]` / `[offset:-100]` (ms): positive offset makes lines appear earlier,
//!   per the LRC convention, so every stamp is shifted by `-offset` and clamped at 0.
//! - Metadata tags (`[ar:]`, `[ti:]`, `[al:]`, `[by:]`, `[length:]`, `[re:]`, `[ve:]`, `[#:]` …) are ignored.
//! - Enhanced (word-level) LRC: inline `<mm:ss.xx>` word stamps are removed from the text.
//! - Text is trimmed; CRLF line endings and a UTF-8 BOM are handled.
//! - Lines without a timestamp in a file that has timestamps are ignored.

use crate::types::{LyricLine, Lyrics};
use std::fmt::Write as _;

/// Where a song position falls within the lyrics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinePosition<'a> {
    /// Before the first non-empty line (song intro), or the lyrics have no lines.
    Intro,
    /// A line is being sung.
    Line { index: usize, text: &'a str },
    /// The current timed line is empty: an instrumental break between vocal sections.
    Break,
}

/// Parses LRC text. Returns `synced = true` lyrics when at least one timestamp
/// is found; otherwise falls back to [`from_plain`]. `source` is left empty for
/// the caller to fill in.
///
/// In the plain fallback, metadata tags such as `[ar:Artist]` are still dropped
/// and enhanced-LRC word stamps are still removed, so only the lyric text remains.
pub fn parse_lrc(input: &str) -> Lyrics {
    let input = strip_bom(input);
    let mut timed: Vec<LyricLine> = Vec::new();
    let mut untimed: Vec<String> = Vec::new();
    let mut offset_ms: i64 = 0;

    for raw in split_lines(input) {
        let tags = read_leading_tags(raw);
        if let Some(offset) = tags.offset {
            offset_ms = offset;
        }
        let text = clean_text(tags.rest);
        if tags.stamps.is_empty() {
            if !text.is_empty() {
                untimed.push(text);
            }
            continue;
        }
        for &start_ms in &tags.stamps {
            timed.push(LyricLine {
                start_ms,
                text: text.clone(),
            });
        }
    }

    if timed.is_empty() {
        return plain_lyrics(untimed);
    }

    // `sort_by_key` is stable: equal timestamps keep their file order. Sorting
    // before the offset keeps lines that clamp to 0 in the order they are sung;
    // the shift moves every line by the same amount, so the order stays sorted.
    timed.sort_by_key(|line| line.start_ms);
    if offset_ms != 0 {
        for line in &mut timed {
            line.start_ms = shift_earlier(line.start_ms, offset_ms);
        }
    }

    Lyrics {
        lines: timed,
        synced: true,
        instrumental: false,
        source: String::new(),
    }
}

/// Builds unsynced lyrics from plain text: one [`LyricLine`] per non-empty input
/// line, in order, each with `start_ms = 0`, and `synced = false`.
pub fn from_plain(text: &str) -> Lyrics {
    let lines = split_lines(strip_bom(text))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    plain_lyrics(lines)
}

/// Formats lyrics back to LRC text (`[mm:ss.xx]text` per line, newline separated).
/// Unsynced lyrics are written as plain text lines.
///
/// Minutes are zero-padded to two digits and may be longer (`[120:00.00]`).
/// A start time that is not a whole number of hundredths is written with
/// millisecond precision (`[mm:ss.xxx]`) so [`parse_lrc`] reads it back exactly.
/// Line breaks inside a line's text are written as spaces.
pub fn to_lrc(lyrics: &Lyrics) -> String {
    let mut out = String::new();
    for (i, line) in lyrics.lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if lyrics.synced {
            push_timestamp(&mut out, line.start_ms);
        }
        for c in line.text.chars() {
            out.push(if c == '\n' || c == '\r' { ' ' } else { c });
        }
    }
    out
}

impl Lyrics {
    /// Index of the last line whose `start_ms <= position_ms`, or `None` before
    /// the first line or when there are no lines. Uses binary search.
    pub fn line_index_at(&self, position_ms: u64) -> Option<usize> {
        let started = self
            .lines
            .partition_point(|line| line.start_ms <= position_ms);
        started.checked_sub(1)
    }

    /// Where `position_ms` falls: [`LinePosition::Intro`] before the first
    /// non-empty line, [`LinePosition::Break`] on an empty line, otherwise the line.
    pub fn at(&self, position_ms: u64) -> LinePosition<'_> {
        let Some(first_text) = self.lines.iter().position(|line| !is_blank(&line.text)) else {
            return LinePosition::Intro;
        };
        let Some(index) = self.line_index_at(position_ms) else {
            return LinePosition::Intro;
        };
        if index < first_text {
            return LinePosition::Intro;
        }
        match self.lines.get(index) {
            Some(line) if !is_blank(&line.text) => LinePosition::Line {
                index,
                text: line.text.trim(),
            },
            Some(_) => LinePosition::Break,
            None => LinePosition::Intro,
        }
    }

    /// The text of the first non-empty line after the line at `position_ms`,
    /// for a "next line" preview.
    pub fn next_text(&self, position_ms: u64) -> Option<&str> {
        // Before the first line every line is still ahead.
        let from = self
            .line_index_at(position_ms)
            .map_or(0, |index| index.saturating_add(1));
        self.lines
            .get(from..)?
            .iter()
            .map(|line| line.text.trim())
            .find(|text| !text.is_empty())
    }

    /// Start time of the first line strictly after `position_ms`, used to
    /// schedule the next update.
    pub fn next_change_ms(&self, position_ms: u64) -> Option<u64> {
        let started = self
            .lines
            .partition_point(|line| line.start_ms <= position_ms);
        self.lines.get(started).map(|line| line.start_ms)
    }

    /// Gives unsynced lyrics estimated timings by spreading the lines evenly
    /// over the song. The first line starts at 5% of the duration and the last
    /// starts no later than 90%; lines keep their order. Does nothing for synced
    /// lyrics, empty lyrics or a zero duration. `synced` stays false.
    ///
    /// A single line starts at 5%. With several lines the last one starts at
    /// exactly 90% (rounded down to the millisecond).
    pub fn spread_evenly(&mut self, duration_ms: u64) {
        if self.synced || self.lines.is_empty() || duration_ms == 0 {
            return;
        }
        // u128 keeps `duration * 90` and `span * i` free of overflow.
        let duration = u128::from(duration_ms);
        let first = duration * 5 / 100;
        let last = duration * 90 / 100;
        let span = last - first;
        let gaps = self.lines.len().saturating_sub(1) as u128;
        for (i, line) in self.lines.iter_mut().enumerate() {
            // A single line (no gaps) stays at `first`.
            let start = first + (span * (i as u128)).checked_div(gaps).unwrap_or(0);
            line.start_ms = u64::try_from(start).unwrap_or(u64::MAX);
        }
    }

    /// Returns a copy with every line shifted by `offset_ms` (positive = later),
    /// clamped at 0, still sorted.
    pub fn shifted(&self, offset_ms: i64) -> Lyrics {
        let mut out = self.clone();
        for line in &mut out.lines {
            line.start_ms = shift_later(line.start_ms, offset_ms);
        }
        out.lines.sort_by_key(|line| line.start_ms);
        out
    }

    /// True when there is at least one line with non-empty text.
    pub fn has_text(&self) -> bool {
        self.lines.iter().any(|line| !is_blank(&line.text))
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Unsynced lyrics from already-cleaned, non-empty lines.
fn plain_lyrics(lines: Vec<String>) -> Lyrics {
    Lyrics {
        lines: lines
            .into_iter()
            .map(|text| LyricLine { start_ms: 0, text })
            .collect(),
        synced: false,
        instrumental: false,
        source: String::new(),
    }
}

fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

/// Splits on `\n`, `\r\n` and lone `\r`. A CRLF pair yields an extra empty
/// piece, which every caller skips.
fn split_lines(s: &str) -> impl Iterator<Item = &str> {
    s.split(['\n', '\r'])
}

/// A line is blank when it has no visible text.
fn is_blank(text: &str) -> bool {
    text.trim().is_empty()
}

/// `ms` moved later by `delta` (earlier when negative), clamped to `0..=u64::MAX`.
fn shift_later(ms: u64, delta: i64) -> u64 {
    if delta >= 0 {
        ms.saturating_add(delta.unsigned_abs())
    } else {
        ms.saturating_sub(delta.unsigned_abs())
    }
}

/// `ms` moved earlier by `offset` (the LRC `[offset:]` convention), clamped.
fn shift_earlier(ms: u64, offset: i64) -> u64 {
    if offset >= 0 {
        ms.saturating_sub(offset.unsigned_abs())
    } else {
        ms.saturating_add(offset.unsigned_abs())
    }
}

/// The bracketed tags at the start of one LRC line and the text after them.
struct LineTags<'a> {
    stamps: Vec<u64>,
    offset: Option<i64>,
    rest: &'a str,
}

/// One `[...]` tag.
enum Tag<'a> {
    /// A valid timestamp, in ms.
    Time(u64),
    /// `[key:value]` metadata, e.g. `[ar:Artist]` or `[offset:+250]`.
    Meta { key: &'a str, value: &'a str },
    /// Looks like a timestamp but is not a valid one: skipped.
    Malformed,
}

/// Reads every `[...]` tag at the start of `line`. Reading stops at the first
/// bracket that is not a tag (for example a `[Chorus]` label), which then
/// belongs to the text.
fn read_leading_tags(line: &str) -> LineTags<'_> {
    let mut stamps = Vec::new();
    let mut offset = None;
    let mut rest = line.trim_start();
    while let Some(after_open) = rest.strip_prefix('[') {
        let Some((content, after_close)) = split_tag(after_open) else {
            break;
        };
        match classify_tag(content) {
            Some(Tag::Time(ms)) => stamps.push(ms),
            Some(Tag::Meta { key, value }) => {
                if key.eq_ignore_ascii_case("offset") {
                    if let Ok(ms) = value.parse::<i64>() {
                        offset = Some(ms);
                    }
                }
            }
            Some(Tag::Malformed) => {}
            None => break,
        }
        rest = after_close.trim_start();
    }
    LineTags {
        stamps,
        offset,
        rest,
    }
}

/// For the text after a `[`: the tag content and the text after its matching
/// `]`. Nested brackets stay in the content, so `[ti:Song [Live]]` is one tag.
/// `None` when the bracket is never closed.
fn split_tag(after_open: &str) -> Option<(&str, &str)> {
    let mut depth: usize = 0;
    for (i, byte) in after_open.bytes().enumerate() {
        match byte {
            b'[' => depth = depth.saturating_add(1),
            b']' if depth == 0 => {
                return Some((after_open.get(..i)?, after_open.get(i + 1..)?));
            }
            b']' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// `None` means the bracket is not a tag at all.
fn classify_tag(content: &str) -> Option<Tag<'_>> {
    if let Some(ms) = parse_timestamp(content) {
        return Some(Tag::Time(ms));
    }
    let (key, value) = content.split_once(':')?;
    let key = key.trim();
    if is_meta_key(key) {
        return Some(Tag::Meta {
            key,
            value: value.trim(),
        });
    }
    // An empty or numeric key (`[:12.00]`, `[00:1x.00]`, `[-00:01.00]`) is a
    // broken timestamp.
    if key
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | ','))
    {
        return Some(Tag::Malformed);
    }
    None
}

/// Metadata keys are short identifiers such as `ar`, `length`, `offset` or `#`.
fn is_meta_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some('#') => chars.as_str().is_empty(),
        Some(c) if c.is_ascii_alphabetic() => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        }
        _ => false,
    }
}

/// Parses `m:ss`, `mm:ss.x`, `mm:ss.xx`, `mm:ss.xxx` (any number of minute
/// digits). The fraction may also be written after `,` or `:`; digits past
/// the third are ignored. Seconds must be below 60. Returns ms.
fn parse_timestamp(s: &str) -> Option<u64> {
    let (minutes, rest) = s.trim().split_once(':')?;
    let (seconds, fraction) = match rest.split_once(['.', ',', ':']) {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => (rest, None),
    };
    if !all_digits(minutes) || !all_digits(seconds) || seconds.len() > 2 {
        return None;
    }
    let minutes: u64 = minutes.parse().ok()?;
    let seconds: u64 = seconds.parse().ok()?;
    if seconds >= 60 {
        return None;
    }
    let fraction_ms = match fraction {
        Some(fraction) => parse_fraction(fraction)?,
        None => 0,
    };
    minutes
        .checked_mul(60_000)?
        .checked_add(seconds * 1_000)?
        .checked_add(fraction_ms)
}

/// 1 digit = tenths, 2 = hundredths, 3 = ms; later digits are ignored.
fn parse_fraction(s: &str) -> Option<u64> {
    if !all_digits(s) {
        return None;
    }
    let mut ms = 0;
    let mut place = 100;
    for digit in s.bytes().take(3) {
        ms += u64::from(digit - b'0') * place;
        place /= 10;
    }
    Some(ms)
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Removes enhanced-LRC word stamps and trims. When stamps were removed, runs
/// of whitespace they leave behind collapse to one space.
fn clean_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut removed = false;
    let mut rest = text;
    while let Some((before, after_open)) = rest.split_once('<') {
        out.push_str(before);
        // A stamp ends at the first `>`; another `<` first means this one is
        // not a stamp. Looking only that far keeps the scan linear.
        match after_open.find(['<', '>']) {
            Some(end) if after_open[end..].starts_with('>') => {
                let inner = &after_open[..end];
                if is_word_stamp(inner) {
                    removed = true;
                    rest = &after_open[end + 1..];
                } else {
                    out.push('<');
                    rest = after_open;
                }
            }
            _ => {
                out.push('<');
                rest = after_open;
            }
        }
    }
    out.push_str(rest);
    if removed {
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        out.trim().to_string()
    }
}

/// `00:12.50`, `1:02`, and broken variants made only of digits, `:`, `.` and `,`.
fn is_word_stamp(inner: &str) -> bool {
    inner.starts_with(|c: char| c.is_ascii_digit())
        && inner.contains(':')
        && inner
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ':' | '.' | ','))
}

/// Appends `[mm:ss.xx]`, or `[mm:ss.xxx]` when the time has ms precision.
fn push_timestamp(out: &mut String, ms: u64) {
    let minutes = ms / 60_000;
    let seconds = ms / 1_000 % 60;
    let millis = ms % 1_000;
    // Writing to a String cannot fail.
    let _ = if millis % 10 == 0 {
        write!(out, "[{minutes:02}:{seconds:02}.{:02}]", millis / 10)
    } else {
        write!(out, "[{minutes:02}:{seconds:02}.{millis:03}]")
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(start_ms: u64, text: &str) -> LyricLine {
        LyricLine {
            start_ms,
            text: text.to_string(),
        }
    }

    fn synced(lines: Vec<LyricLine>) -> Lyrics {
        Lyrics {
            lines,
            synced: true,
            instrumental: false,
            source: String::new(),
        }
    }

    fn times(lyrics: &Lyrics) -> Vec<u64> {
        lyrics.lines.iter().map(|l| l.start_ms).collect()
    }

    fn texts(lyrics: &Lyrics) -> Vec<&str> {
        lyrics.lines.iter().map(|l| l.text.as_str()).collect()
    }

    /// Lyrics used by most lookup tests:
    /// 0 s break, 5 s "One", 10 s "Two", 15 s break, 20 s "Three", 20 s "Four".
    fn sample() -> Lyrics {
        synced(vec![
            line(0, ""),
            line(5_000, "One"),
            line(10_000, "Two"),
            line(15_000, ""),
            line(20_000, "Three"),
            line(20_000, "Four"),
        ])
    }

    // ---- parse_lrc: timestamps ------------------------------------------

    #[test]
    fn parses_basic_synced_lyrics() {
        let lyrics = parse_lrc("[00:12.34]Hello\n[00:15.00]World");
        assert_eq!(
            lyrics,
            Lyrics {
                lines: vec![line(12_340, "Hello"), line(15_000, "World")],
                synced: true,
                instrumental: false,
                source: String::new(),
            }
        );
    }

    #[test]
    fn fraction_digit_counts() {
        let lyrics = parse_lrc(
            "[00:01.5]tenths\n[00:02.05]hundredths\n[00:03.005]millis\n[00:04]none\n[1:05.3]short minutes",
        );
        assert_eq!(times(&lyrics), vec![1_500, 2_050, 3_005, 4_000, 65_300]);
        assert_eq!(
            texts(&lyrics),
            vec!["tenths", "hundredths", "millis", "none", "short minutes"]
        );
    }

    #[test]
    fn extra_fraction_digits_are_truncated_to_ms() {
        let lyrics = parse_lrc("[00:01.12345]a\n[00:02.999999999999999999999999]b");
        assert_eq!(times(&lyrics), vec![1_123, 2_999]);
    }

    #[test]
    fn alternative_fraction_separators() {
        let lyrics = parse_lrc("[00:01,50]comma\n[00:02:25]colon");
        assert_eq!(times(&lyrics), vec![1_500, 2_250]);
    }

    #[test]
    fn single_digit_seconds_and_spaces_inside_brackets() {
        let lyrics = parse_lrc("[0:5.1]a\n[ 00:06.00 ]b");
        assert_eq!(times(&lyrics), vec![5_100, 6_000]);
    }

    #[test]
    fn minutes_may_exceed_59_and_99() {
        let lyrics = parse_lrc("[75:00.00]long\n[120:30.50]longer\n[1000:00]huge");
        assert_eq!(times(&lyrics), vec![4_500_000, 7_230_500, 60_000_000]);
    }

    #[test]
    fn largest_representable_timestamp() {
        let lyrics = parse_lrc("[307445734561825:51.615]max");
        assert_eq!(times(&lyrics), vec![u64::MAX]);
        // One ms more overflows and is skipped, not wrapped.
        let lyrics = parse_lrc("[307445734561825:51.616]over\n[00:01.00]ok");
        assert_eq!(texts(&lyrics), vec!["ok"]);
        let lyrics = parse_lrc("[99999999999999999999999:00.00]over\n[00:01.00]ok");
        assert_eq!(texts(&lyrics), vec!["ok"]);
    }

    #[test]
    fn several_stamps_on_one_line_expand() {
        let lyrics = parse_lrc("[00:10.00][01:20.00]chorus\n[00:30.00]verse");
        assert_eq!(
            lyrics.lines,
            vec![
                line(10_000, "chorus"),
                line(30_000, "verse"),
                line(80_000, "chorus")
            ]
        );
    }

    #[test]
    fn several_stamps_separated_by_spaces() {
        let lyrics = parse_lrc("[00:10.00] [00:20.00]  [00:30.00] la la");
        assert_eq!(times(&lyrics), vec![10_000, 20_000, 30_000]);
        assert!(lyrics.lines.iter().all(|l| l.text == "la la"));
    }

    #[test]
    fn equal_timestamps_keep_file_order() {
        let lyrics = parse_lrc("[00:20.00]x\n[00:10.00]y\n[00:20.00]z\n[00:10.00]w");
        assert_eq!(texts(&lyrics), vec!["y", "w", "x", "z"]);
        assert_eq!(times(&lyrics), vec![10_000, 10_000, 20_000, 20_000]);
    }

    #[test]
    fn unsorted_input_is_sorted() {
        let lyrics = parse_lrc("[00:30.00]c\n[00:10.00]a\n[00:20.00]b");
        assert_eq!(texts(&lyrics), vec!["a", "b", "c"]);
    }

    #[test]
    fn empty_timed_line_is_a_break() {
        let lyrics = parse_lrc("[00:10.00]Hi\n[00:20.00]\n[00:25.00]   \n[00:30.00]Bye");
        assert_eq!(
            lyrics.lines,
            vec![
                line(10_000, "Hi"),
                line(20_000, ""),
                line(25_000, ""),
                line(30_000, "Bye")
            ]
        );
        assert!(lyrics.synced);
    }

    #[test]
    fn file_with_only_breaks_is_still_synced() {
        let lyrics = parse_lrc("[00:00.00]\n[00:10.00]");
        assert!(lyrics.synced);
        assert_eq!(lyrics.lines.len(), 2);
        assert!(!lyrics.has_text());
    }

    // ---- parse_lrc: text cleanup ------------------------------------------

    #[test]
    fn text_is_trimmed() {
        let lyrics = parse_lrc("[00:01.00]   spaced out \t ");
        assert_eq!(texts(&lyrics), vec!["spaced out"]);
    }

    #[test]
    fn inner_whitespace_is_kept_without_word_stamps() {
        let lyrics = parse_lrc("[00:01.00]a  b");
        assert_eq!(texts(&lyrics), vec!["a  b"]);
    }

    #[test]
    fn enhanced_word_stamps_are_removed() {
        let lyrics = parse_lrc(
            "[00:12.00]<00:12.00>Never <00:12.50>gonna <00:13.00>give<00:13.40>\n\
             [00:14.00]<00:14.00> you <00:14.50> up <00:15.00>",
        );
        assert_eq!(texts(&lyrics), vec!["Never gonna give", "you up"]);
        assert_eq!(times(&lyrics), vec![12_000, 14_000]);
    }

    #[test]
    fn word_stamps_inside_words_join_the_word() {
        let lyrics = parse_lrc("[00:01.00]beau<00:01.20>tiful");
        assert_eq!(texts(&lyrics), vec!["beautiful"]);
    }

    #[test]
    fn angle_brackets_that_are_not_stamps_stay() {
        let lyrics = parse_lrc(
            "[00:01.00]I <3 you\n[00:02.00]a < b > c\n[00:03.00]<i>italic</i>\n[00:04.00]<<00:01.00>x",
        );
        assert_eq!(
            texts(&lyrics),
            vec!["I <3 you", "a < b > c", "<i>italic</i>", "<x"]
        );
    }

    #[test]
    fn only_word_stamps_make_a_break() {
        let lyrics = parse_lrc("[00:01.00]<00:01.00> <00:02.00>");
        assert_eq!(lyrics.lines, vec![line(1_000, "")]);
    }

    #[test]
    fn section_labels_after_a_stamp_are_text() {
        let lyrics = parse_lrc("[00:01.00][Chorus]\n[00:02.00][Verse 1: Someone] la");
        assert_eq!(texts(&lyrics), vec!["[Chorus]", "[Verse 1: Someone] la"]);
    }

    #[test]
    fn brackets_later_in_the_text_are_kept() {
        let lyrics = parse_lrc("[00:01.00]the time is [10:30] now");
        assert_eq!(texts(&lyrics), vec!["the time is [10:30] now"]);
        assert_eq!(times(&lyrics), vec![1_000]);
    }

    #[test]
    fn unicode_text_is_preserved() {
        let lyrics =
            parse_lrc("[00:01.00]ქართული ენა 🎵\n[00:02.00]日本語の歌詞\n[00:03.00]Ünïcödé");
        assert_eq!(
            texts(&lyrics),
            vec!["ქართული ენა 🎵", "日本語の歌詞", "Ünïcödé"]
        );
    }

    // ---- parse_lrc: metadata, offset, line endings -------------------------

    #[test]
    fn metadata_tags_are_ignored() {
        let input =
            "[ar:Rick Astley]\n[ti:Never Gonna Give You Up]\n[al:Whenever You Need Somebody]\n\
                     [by:someone]\n[length: 03:33]\n[re:Some Tool]\n[ve:1.0]\n[#:a comment]\n\
                     [au:Stock]\n[la:en]\n[00:01.00]Line";
        let lyrics = parse_lrc(input);
        assert_eq!(lyrics.lines, vec![line(1_000, "Line")]);
        assert!(lyrics.synced);
    }

    #[test]
    fn untimed_lines_in_a_timed_file_are_ignored() {
        let lyrics = parse_lrc("Some header\n[00:01.00]A\nstray text\n\n[00:02.00]B\n");
        assert_eq!(texts(&lyrics), vec!["A", "B"]);
    }

    #[test]
    fn bom_and_crlf_are_handled() {
        let lyrics = parse_lrc("\u{feff}[ar:X]\r\n[00:01.00]A\r\n[00:02.00]B\r\n");
        assert_eq!(lyrics.lines, vec![line(1_000, "A"), line(2_000, "B")]);
    }

    #[test]
    fn lone_carriage_returns_split_lines() {
        let lyrics = parse_lrc("[00:01.00]A\r[00:02.00]B");
        assert_eq!(texts(&lyrics), vec!["A", "B"]);
    }

    #[test]
    fn positive_offset_moves_lines_earlier_and_clamps() {
        let lyrics = parse_lrc("[offset:+250]\n[00:01.00]A\n[00:00.10]B\n[00:00.25]C");
        assert_eq!(
            lyrics.lines,
            vec![line(0, "B"), line(0, "C"), line(750, "A")]
        );
    }

    #[test]
    fn offset_clamping_keeps_time_order() {
        // B (300 ms) and A (100 ms) both clamp to 0. A was sung first, so it
        // must stay first and B must be the current line at position 0.
        let lyrics = parse_lrc("[offset:+500]\n[00:00.30]B\n[00:00.10]A\n[00:01.00]C");
        assert_eq!(
            lyrics.lines,
            vec![line(0, "A"), line(0, "B"), line(500, "C")]
        );
        assert_eq!(
            lyrics.at(0),
            LinePosition::Line {
                index: 1,
                text: "B"
            }
        );
    }

    #[test]
    fn negative_offset_moves_lines_later() {
        let lyrics = parse_lrc("[offset:-100]\n[00:01.00]A");
        assert_eq!(times(&lyrics), vec![1_100]);
    }

    #[test]
    fn offset_variants() {
        assert_eq!(times(&parse_lrc("[offset:500]\n[00:01.00]A")), vec![500]);
        assert_eq!(times(&parse_lrc("[offset: 500 ]\n[00:01.00]A")), vec![500]);
        assert_eq!(times(&parse_lrc("[OFFSET:500]\n[00:01.00]A")), vec![500]);
        // Placement in the file does not matter.
        assert_eq!(times(&parse_lrc("[00:01.00]A\n[offset:500]")), vec![500]);
        // The last offset tag wins.
        assert_eq!(
            times(&parse_lrc("[offset:100]\n[offset:300]\n[00:01.00]A")),
            vec![700]
        );
    }

    #[test]
    fn malformed_offsets_are_ignored() {
        for tag in ["[offset:abc]", "[offset:]", "[offset:12ms]", "[offset:1.5]"] {
            let lyrics = parse_lrc(&format!("{tag}\n[00:01.00]A"));
            assert_eq!(times(&lyrics), vec![1_000], "{tag}");
        }
    }

    #[test]
    fn extreme_offsets_saturate() {
        let lyrics = parse_lrc("[offset:9223372036854775807]\n[00:01.00]A\n[99:00.00]B");
        assert_eq!(times(&lyrics), vec![0, 0]);
        let lyrics = parse_lrc("[offset:-9223372036854775808]\n[00:01.00]A");
        assert_eq!(times(&lyrics), vec![9_223_372_036_854_776_808]);
        let lyrics = parse_lrc("[offset:-9223372036854775808]\n[307445734561825:51.615]A");
        assert_eq!(times(&lyrics), vec![u64::MAX]);
    }

    // ---- parse_lrc: malformed input -----------------------------------------

    #[test]
    fn malformed_stamps_are_skipped() {
        let input = "[00:1x.00]bad\n[00:60.00]sixty seconds\n[-00:01.00]negative\n\
                     [:12.00]no minutes\n[00:]no seconds\n[00:123]long seconds\n\
                     [00:01.]empty fraction\n[00:01.a]letter fraction\n[01:02:03.45]hours\n\
                     [00:02.00]good";
        let lyrics = parse_lrc(input);
        assert_eq!(lyrics.lines, vec![line(2_000, "good")]);
    }

    #[test]
    fn malformed_stamp_next_to_a_good_one() {
        let lyrics = parse_lrc("[00:01.00][00:xx]text\n[00:zz][00:02.00]more");
        assert_eq!(lyrics.lines, vec![line(1_000, "text"), line(2_000, "more")]);
    }

    #[test]
    fn unclosed_bracket_has_no_stamp() {
        let lyrics = parse_lrc("[00:01.00 broken\n[00:02.00]fine");
        assert_eq!(texts(&lyrics), vec!["fine"]);
    }

    #[test]
    fn never_panics_on_odd_input() {
        let samples = [
            "[",
            "]",
            "[]",
            "[:]",
            "[0:]",
            "[:0]",
            "<",
            ">",
            "<>",
            "<:>",
            "[00:00.00]<",
            "[00:00.00]<00:",
            "[00:00.00]<00:00.00",
            "[ö:ü]",
            "[00:0ü]",
            "[00:00.0ü]",
            "[ü",
            "[offset:ü]",
            "[00:00.00][",
            "\u{feff}",
            "\u{feff}\u{feff}",
            "\r\n\r\n",
            "[[[[[[[[",
            "]]]]]]]]",
            "[[]]",
            "[[]]]]",
            "[a[b]c]d",
            "[ti:[[[]",
            "[00:01.00][[[",
            "[ü[ö]ä]x",
            "<<<<<<<<>>>>>>>",
            "[00:00.00]🎵<00:00.00>🎵<🎵>",
        ];
        for sample in samples {
            let _ = parse_lrc(sample);
            let _ = from_plain(sample);
        }
        // Every prefix of a tricky line, cut on character boundaries.
        let tricky =
            "\u{feff}[offset:+5][00:01.5ü][00:02.00]Ü<00:02.50>bér [Chorus] 🎵\r\n[99:59.999]x";
        for (i, _) in tricky.char_indices() {
            let _ = parse_lrc(&tricky[..i]);
            let _ = parse_lrc(&tricky[i..]);
        }
    }

    #[test]
    fn many_angle_brackets_are_handled() {
        let line_text = "<".repeat(10_000) + &">".repeat(10_000);
        let lyrics = parse_lrc(&format!("[00:01.00]{line_text}"));
        assert_eq!(lyrics.lines.len(), 1);
        assert_eq!(lyrics.lines[0].text, line_text);
    }

    // ---- parse_lrc: plain fallback -------------------------------------------

    #[test]
    fn falls_back_to_plain_without_timestamps() {
        let lyrics = parse_lrc("Hello\n\nWorld\n");
        assert_eq!(lyrics, from_plain("Hello\n\nWorld\n"));
        assert!(!lyrics.synced);
        assert_eq!(lyrics.lines, vec![line(0, "Hello"), line(0, "World")]);
    }

    #[test]
    fn plain_fallback_drops_metadata_tags() {
        let lyrics = parse_lrc("[ar:Someone]\n[ti:Song]\n[offset:500]\nHello\n[Chorus]\nWorld");
        assert!(!lyrics.synced);
        assert_eq!(texts(&lyrics), vec!["Hello", "[Chorus]", "World"]);
        assert!(lyrics.lines.iter().all(|l| l.start_ms == 0));
    }

    #[test]
    fn metadata_values_may_contain_brackets() {
        // A title such as `Song [Live]` must not leave a stray `]` behind.
        let input = "[ti:Song [Live]]\n[al:Album [Deluxe] [2024]]\nHello";
        let lyrics = parse_lrc(input);
        assert!(!lyrics.synced);
        assert_eq!(texts(&lyrics), vec!["Hello"]);

        let lyrics = parse_lrc("[ti:Song [Live]]\n[00:01.00]Hello\n[00:02.00][Chorus [x]] la");
        assert_eq!(texts(&lyrics), vec!["Hello", "[Chorus [x]] la"]);
        // An unclosed nested bracket is not a tag.
        let lyrics = parse_lrc("[00:01.00 [x]\n[00:02.00]ok");
        assert_eq!(lyrics.lines, vec![line(2_000, "ok")]);
    }

    #[test]
    fn empty_input_gives_empty_unsynced_lyrics() {
        for input in ["", "\u{feff}", "   \n\t\n", "\r\n"] {
            let lyrics = parse_lrc(input);
            assert!(lyrics.lines.is_empty(), "{input:?}");
            assert!(!lyrics.synced);
            assert!(!lyrics.instrumental);
            assert!(lyrics.source.is_empty());
        }
    }

    // ---- from_plain ------------------------------------------------------------

    #[test]
    fn from_plain_one_line_per_non_empty_line() {
        let lyrics = from_plain("  First line \n\n   \nSecond line\nThird");
        assert_eq!(
            lyrics,
            Lyrics {
                lines: vec![
                    line(0, "First line"),
                    line(0, "Second line"),
                    line(0, "Third")
                ],
                synced: false,
                instrumental: false,
                source: String::new(),
            }
        );
    }

    #[test]
    fn from_plain_handles_bom_crlf_and_unicode() {
        let lyrics = from_plain("\u{feff}ქართული\r\nენა 🎵\r\n");
        assert_eq!(texts(&lyrics), vec!["ქართული", "ენა 🎵"]);
    }

    #[test]
    fn from_plain_keeps_text_verbatim() {
        // Plain text is not LRC: brackets and angle brackets are lyrics here.
        let lyrics = from_plain("[00:01.00]not a stamp\n<00:01.00>");
        assert_eq!(texts(&lyrics), vec!["[00:01.00]not a stamp", "<00:01.00>"]);
    }

    #[test]
    fn from_plain_empty() {
        assert!(from_plain("").lines.is_empty());
        assert!(from_plain("\n\n  \n").lines.is_empty());
    }

    // ---- to_lrc ----------------------------------------------------------------

    #[test]
    fn to_lrc_formats_synced_lines() {
        let lyrics = synced(vec![
            line(0, ""),
            line(12_340, "Hello"),
            line(65_000, "World"),
        ]);
        assert_eq!(
            to_lrc(&lyrics),
            "[00:00.00]\n[00:12.34]Hello\n[01:05.00]World"
        );
    }

    #[test]
    fn to_lrc_long_minutes_are_printed_in_full() {
        let lyrics = synced(vec![line(7_230_500, "a"), line(6_000_000 * 10, "b")]);
        assert_eq!(to_lrc(&lyrics), "[120:30.50]a\n[1000:00.00]b");
    }

    #[test]
    fn to_lrc_keeps_millisecond_precision() {
        let lyrics = synced(vec![line(1_005, "a"), line(1_230, "b"), line(1_999, "c")]);
        assert_eq!(to_lrc(&lyrics), "[00:01.005]a\n[00:01.23]b\n[00:01.999]c");
    }

    #[test]
    fn to_lrc_unsynced_is_plain_text() {
        let lyrics = from_plain("one\ntwo");
        assert_eq!(to_lrc(&lyrics), "one\ntwo");
        assert_eq!(parse_lrc(&to_lrc(&lyrics)), lyrics);
    }

    #[test]
    fn to_lrc_empty() {
        assert_eq!(to_lrc(&Lyrics::default()), "");
        assert_eq!(to_lrc(&synced(Vec::new())), "");
    }

    #[test]
    fn to_lrc_flattens_line_breaks_in_text() {
        let lyrics = synced(vec![line(1_000, "a\nb\r\nc")]);
        assert_eq!(to_lrc(&lyrics), "[00:01.00]a b  c");
        assert_eq!(parse_lrc(&to_lrc(&lyrics)).lines.len(), 1);
    }

    #[test]
    fn round_trip_of_parsed_file() {
        let input = "\u{feff}[ar:X]\r\n[offset:+15]\n[00:00.00]\n[00:12.34]Hello\n[00:10.00][01:20.00]chorus\n\
                     [00:13.00]<00:13.00>word <00:13.50>stamps\n[120:00.5]late\n[00:20.00]ქართული 🎵";
        let parsed = parse_lrc(input);
        assert!(parsed.synced);
        let again = parse_lrc(&to_lrc(&parsed));
        assert_eq!(again, parsed);
    }

    #[test]
    fn round_trip_of_arbitrary_times() {
        let starts = [
            0,
            1,
            9,
            10,
            999,
            1_000,
            59_999,
            60_000,
            5_999_999,
            6_000_000,
            123_456_789,
            u64::MAX - 1,
            u64::MAX,
        ];
        let lyrics = synced(
            starts
                .iter()
                .enumerate()
                .map(|(i, &ms)| line(ms, &format!("line {i}")))
                .collect(),
        );
        assert_eq!(parse_lrc(&to_lrc(&lyrics)), lyrics);
    }

    #[test]
    fn round_trip_with_breaks_and_equal_times() {
        let lyrics = synced(vec![
            line(1_000, "a"),
            line(1_000, "b"),
            line(2_000, ""),
            line(2_000, "c"),
        ]);
        assert_eq!(parse_lrc(&to_lrc(&lyrics)), lyrics);
    }

    // ---- line_index_at -----------------------------------------------------------

    #[test]
    fn line_index_at_boundaries() {
        let lyrics = synced(vec![line(1_000, "a"), line(2_000, "b"), line(3_000, "c")]);
        assert_eq!(lyrics.line_index_at(0), None);
        assert_eq!(lyrics.line_index_at(999), None);
        assert_eq!(lyrics.line_index_at(1_000), Some(0));
        assert_eq!(lyrics.line_index_at(1_999), Some(0));
        assert_eq!(lyrics.line_index_at(2_000), Some(1));
        assert_eq!(lyrics.line_index_at(3_000), Some(2));
        assert_eq!(lyrics.line_index_at(u64::MAX), Some(2));
    }

    #[test]
    fn line_index_at_equal_times_picks_the_last() {
        assert_eq!(sample().line_index_at(20_000), Some(5));
        assert_eq!(sample().line_index_at(19_999), Some(3));
    }

    #[test]
    fn line_index_at_empty_and_zero() {
        assert_eq!(Lyrics::default().line_index_at(0), None);
        assert_eq!(Lyrics::default().line_index_at(u64::MAX), None);
        let lyrics = synced(vec![line(0, "a")]);
        assert_eq!(lyrics.line_index_at(0), Some(0));
    }

    #[test]
    fn line_index_at_large_input() {
        let lyrics = synced((0..10_000).map(|i| line(i * 10, "x")).collect());
        assert_eq!(lyrics.line_index_at(55_555), Some(5_555));
        assert_eq!(lyrics.line_index_at(99_990), Some(9_999));
    }

    // ---- at ------------------------------------------------------------------------

    #[test]
    fn at_walks_through_the_song() {
        let lyrics = sample();
        assert_eq!(lyrics.at(0), LinePosition::Intro);
        assert_eq!(lyrics.at(4_999), LinePosition::Intro);
        assert_eq!(
            lyrics.at(5_000),
            LinePosition::Line {
                index: 1,
                text: "One"
            }
        );
        assert_eq!(
            lyrics.at(12_000),
            LinePosition::Line {
                index: 2,
                text: "Two"
            }
        );
        assert_eq!(lyrics.at(15_000), LinePosition::Break);
        assert_eq!(lyrics.at(19_999), LinePosition::Break);
        assert_eq!(
            lyrics.at(20_000),
            LinePosition::Line {
                index: 5,
                text: "Four"
            }
        );
        assert_eq!(
            lyrics.at(u64::MAX),
            LinePosition::Line {
                index: 5,
                text: "Four"
            }
        );
    }

    #[test]
    fn at_without_lines_or_text_is_intro() {
        assert_eq!(Lyrics::default().at(0), LinePosition::Intro);
        assert_eq!(Lyrics::default().at(u64::MAX), LinePosition::Intro);
        let breaks = synced(vec![line(0, ""), line(1_000, "  ")]);
        assert_eq!(breaks.at(5_000), LinePosition::Intro);
    }

    #[test]
    fn at_before_first_line_is_intro() {
        let lyrics = synced(vec![line(3_000, "a")]);
        assert_eq!(lyrics.at(0), LinePosition::Intro);
        assert_eq!(
            lyrics.at(3_000),
            LinePosition::Line {
                index: 0,
                text: "a"
            }
        );
    }

    #[test]
    fn at_trailing_break_after_last_line() {
        let lyrics = synced(vec![line(1_000, "a"), line(2_000, "")]);
        assert_eq!(
            lyrics.at(1_500),
            LinePosition::Line {
                index: 0,
                text: "a"
            }
        );
        assert_eq!(lyrics.at(2_000), LinePosition::Break);
        assert_eq!(lyrics.at(u64::MAX), LinePosition::Break);
    }

    #[test]
    fn at_break_sharing_a_time_with_the_first_line() {
        // The break comes first in file order, so the text line wins at 1 s.
        let lyrics = synced(vec![line(1_000, ""), line(1_000, "a")]);
        assert_eq!(
            lyrics.at(1_000),
            LinePosition::Line {
                index: 1,
                text: "a"
            }
        );
        // A break after the text at the same time ends it immediately.
        let lyrics = synced(vec![line(1_000, "a"), line(1_000, "")]);
        assert_eq!(lyrics.at(1_000), LinePosition::Break);
    }

    #[test]
    fn at_whitespace_only_line_is_a_break_and_text_is_trimmed() {
        let lyrics = synced(vec![line(1_000, " a "), line(2_000, "   ")]);
        assert_eq!(
            lyrics.at(1_000),
            LinePosition::Line {
                index: 0,
                text: "a"
            }
        );
        assert_eq!(lyrics.at(2_000), LinePosition::Break);
    }

    // ---- next_text ---------------------------------------------------------------

    #[test]
    fn next_text_previews_the_following_line() {
        let lyrics = sample();
        assert_eq!(lyrics.next_text(0), Some("One"));
        assert_eq!(lyrics.next_text(4_999), Some("One"));
        assert_eq!(lyrics.next_text(5_000), Some("Two"));
        // Skips the break at 15 s.
        assert_eq!(lyrics.next_text(10_000), Some("Three"));
        assert_eq!(lyrics.next_text(15_000), Some("Three"));
        // At 20 s the current line is "Four" (last of the equal group).
        assert_eq!(lyrics.next_text(20_000), None);
        assert_eq!(lyrics.next_text(u64::MAX), None);
    }

    #[test]
    fn next_text_before_the_first_line() {
        let lyrics = synced(vec![line(5_000, "a"), line(6_000, "b")]);
        assert_eq!(lyrics.next_text(0), Some("a"));
        assert_eq!(lyrics.next_text(5_000), Some("b"));
    }

    #[test]
    fn next_text_empty_and_only_breaks() {
        assert_eq!(Lyrics::default().next_text(0), None);
        let lyrics = synced(vec![line(0, "a"), line(1_000, ""), line(2_000, " ")]);
        assert_eq!(lyrics.next_text(0), None);
    }

    // ---- next_change_ms ----------------------------------------------------------

    #[test]
    fn next_change_ms_is_strictly_after() {
        let lyrics = sample();
        assert_eq!(lyrics.next_change_ms(0), Some(5_000));
        assert_eq!(lyrics.next_change_ms(4_999), Some(5_000));
        assert_eq!(lyrics.next_change_ms(5_000), Some(10_000));
        assert_eq!(lyrics.next_change_ms(15_000), Some(20_000));
        assert_eq!(lyrics.next_change_ms(19_999), Some(20_000));
        assert_eq!(lyrics.next_change_ms(20_000), None);
        assert_eq!(lyrics.next_change_ms(u64::MAX), None);
    }

    #[test]
    fn next_change_ms_empty_and_first_line_at_zero() {
        assert_eq!(Lyrics::default().next_change_ms(0), None);
        let lyrics = synced(vec![line(0, "a"), line(10, "b")]);
        assert_eq!(lyrics.next_change_ms(0), Some(10));
    }

    // ---- spread_evenly -----------------------------------------------------------

    #[test]
    fn spread_evenly_spaces_lines_from_5_to_90_percent() {
        let mut lyrics = from_plain("a\nb\nc\nd");
        lyrics.spread_evenly(100_000);
        assert_eq!(times(&lyrics), vec![5_000, 33_333, 61_666, 90_000]);
        assert_eq!(texts(&lyrics), vec!["a", "b", "c", "d"]);
        assert!(!lyrics.synced);
    }

    #[test]
    fn spread_evenly_two_lines() {
        let mut lyrics = from_plain("a\nb");
        lyrics.spread_evenly(200_000);
        assert_eq!(times(&lyrics), vec![10_000, 180_000]);
    }

    #[test]
    fn spread_evenly_single_line_starts_at_5_percent() {
        let mut lyrics = from_plain("only");
        lyrics.spread_evenly(200_000);
        assert_eq!(times(&lyrics), vec![10_000]);
        assert!(!lyrics.synced);
    }

    #[test]
    fn spread_evenly_does_nothing_when_it_should_not() {
        let original = synced(vec![line(1_000, "a"), line(2_000, "b")]);
        let mut lyrics = original.clone();
        lyrics.spread_evenly(100_000);
        assert_eq!(lyrics, original);

        let mut empty = Lyrics::default();
        empty.spread_evenly(100_000);
        assert_eq!(empty, Lyrics::default());

        let plain = from_plain("a\nb");
        let mut zero = plain.clone();
        zero.spread_evenly(0);
        assert_eq!(zero, plain);
    }

    #[test]
    fn spread_evenly_is_ordered_and_bounded() {
        let mut lyrics = from_plain(&(0..97).map(|i| format!("line {i}\n")).collect::<String>());
        let duration = 213_457;
        lyrics.spread_evenly(duration);
        let starts = times(&lyrics);
        assert!(starts.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(starts[0], duration * 5 / 100);
        assert!(*starts.last().unwrap() <= duration * 90 / 100);
        assert_eq!(lyrics.lines[96].text, "line 96");
    }

    #[test]
    fn spread_evenly_tiny_and_huge_durations() {
        let mut lyrics = from_plain("a\nb\nc");
        lyrics.spread_evenly(1);
        assert_eq!(times(&lyrics), vec![0, 0, 0]);

        let mut lyrics = from_plain("a\nb\nc");
        lyrics.spread_evenly(u64::MAX);
        let max = u128::from(u64::MAX);
        let first = (max * 5 / 100) as u64;
        let last = (max * 90 / 100) as u64;
        let starts = times(&lyrics);
        assert_eq!(starts[0], first);
        assert_eq!(starts[2], last);
        assert!(starts[0] < starts[1] && starts[1] < starts[2]);
    }

    #[test]
    fn spread_evenly_can_be_repeated() {
        let mut lyrics = from_plain("a\nb\nc");
        lyrics.spread_evenly(100_000);
        let once = lyrics.clone();
        lyrics.spread_evenly(100_000);
        assert_eq!(lyrics, once);
        lyrics.spread_evenly(200_000);
        assert_eq!(times(&lyrics), vec![10_000, 95_000, 180_000]);
    }

    // ---- shifted -----------------------------------------------------------------

    #[test]
    fn shifted_moves_lines_later_or_earlier() {
        let lyrics = synced(vec![line(1_000, "a"), line(2_000, "b")]);
        assert_eq!(times(&lyrics.shifted(500)), vec![1_500, 2_500]);
        assert_eq!(times(&lyrics.shifted(-500)), vec![500, 1_500]);
        assert_eq!(times(&lyrics.shifted(0)), vec![1_000, 2_000]);
        // The original is untouched.
        assert_eq!(times(&lyrics), vec![1_000, 2_000]);
    }

    #[test]
    fn shifted_clamps_at_zero_and_stays_sorted() {
        let lyrics = synced(vec![line(100, "a"), line(1_000, "b"), line(5_000, "c")]);
        let shifted = lyrics.shifted(-2_000);
        assert_eq!(times(&shifted), vec![0, 0, 3_000]);
        assert_eq!(texts(&shifted), vec!["a", "b", "c"]);
    }

    #[test]
    fn shifted_keeps_other_fields() {
        let mut lyrics = synced(vec![line(1_000, "a")]);
        lyrics.source = "lrclib".into();
        let shifted = lyrics.shifted(10);
        assert!(shifted.synced);
        assert!(!shifted.instrumental);
        assert_eq!(shifted.source, "lrclib");
    }

    #[test]
    fn shifted_extremes_saturate() {
        let lyrics = synced(vec![line(0, "a"), line(u64::MAX, "b")]);
        assert_eq!(
            times(&lyrics.shifted(i64::MAX)),
            vec![i64::MAX as u64, u64::MAX]
        );
        assert_eq!(
            times(&lyrics.shifted(i64::MIN)),
            vec![0, u64::MAX - (1u64 << 63)]
        );
        assert!(Lyrics::default().shifted(5).lines.is_empty());
    }

    // ---- has_text ----------------------------------------------------------------

    #[test]
    fn has_text_cases() {
        assert!(!Lyrics::default().has_text());
        assert!(!synced(vec![line(0, ""), line(1, "   ")]).has_text());
        assert!(synced(vec![line(0, ""), line(1, "a")]).has_text());
        assert!(from_plain("x").has_text());
        let instrumental = Lyrics {
            instrumental: true,
            ..Lyrics::default()
        };
        assert!(!instrumental.has_text());
    }

    // ---- realistic example -------------------------------------------------------

    #[test]
    fn realistic_lrclib_style_file() {
        let input = "[ar: Rick Astley]\n[ti: Never Gonna Give You Up]\n[length: 03:33]\n\
                     [00:18.85] We're no strangers to love\n\
                     [00:22.66] You know the rules and so do I\n\
                     [00:26.94] A full commitment's what I'm thinking of\n\
                     [00:31.06] \n\
                     [00:43.05] Never gonna give you up\n";
        let lyrics = parse_lrc(input);
        assert!(lyrics.synced);
        assert_eq!(lyrics.lines.len(), 5);
        assert_eq!(lyrics.at(10_000), LinePosition::Intro);
        assert_eq!(lyrics.next_text(10_000), Some("We're no strangers to love"));
        assert_eq!(
            lyrics.at(23_000),
            LinePosition::Line {
                index: 1,
                text: "You know the rules and so do I"
            }
        );
        assert_eq!(lyrics.at(35_000), LinePosition::Break);
        assert_eq!(lyrics.next_text(35_000), Some("Never gonna give you up"));
        assert_eq!(lyrics.next_change_ms(35_000), Some(43_050));
    }
}
