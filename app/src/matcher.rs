//! Cleaning up what players report so lyric lookups hit, and scoring search results.
//!
//! Players report titles like `Song (Remastered 2011)`, `Song - 2011 Remaster`,
//! `Song (feat. Someone)`, `Artist - Song (Official Video)` with the artist set to
//! a YouTube channel such as `ArtistVEVO` or `Artist - Topic`. Lyrics databases
//! store `Song` by `Artist`.

use crate::types::Track;
use std::collections::BTreeSet;

/// Removes noise from a title while keeping version markers that change the
/// lyrics or timing.
///
/// Removed, in parentheses or brackets or after ` - `: remaster/remastered (with
/// or without a year), `feat.`/`ft.`/`featuring` credits, `Official Video`,
/// `Official Music Video`, `Official Audio`, `Official Lyric Video`, `Lyric Video`,
/// `Lyrics`, `Audio`, `Video`, `Visualizer`, `HD`, `HQ`, `4K`, `Explicit`, `Clean`,
/// `Radio Edit`, `Single Version`, `Album Version`, `Mono`, `Stereo`, `Bonus Track`.
/// Combinations of these words count too (`Official HD Video`,
/// `2009 Remastered Version`, `50th Anniversary Remaster`). Full-width and
/// lenticular brackets (`（…）`, `［…］`, `【MV】`) are treated like brackets, and
/// en/em dashes (` – `, ` — `) like ` - `. A bare `feat.`/`ft.`/`featuring`
/// credit after the title (`Song ft. Someone`) is removed as well. When a
/// credit after ` - ` is followed by kept groups, only the credit goes:
/// `Song - feat. Someone (Remix)` → `Song (Remix)`.
///
/// Kept: `Remix`, `Live`, `Acoustic`, `Instrumental`, `Sped Up`, `Slowed`,
/// `Demo`, `Version` with a name (e.g. `Taylor's Version`), anything else.
/// Only the parts after the first ` - ` can be dropped as a whole, so songs
/// called `Video` or `Clean` keep their title.
///
/// Also trims whitespace and collapses repeated spaces. When everything would
/// be removed, the (trimmed and collapsed) input is returned instead.
pub fn clean_title(title: &str) -> String {
    let collapsed = collapse_whitespace(title);
    let pieces = parse_pieces(&collapsed)
        .into_iter()
        .filter(|piece| !matches!(piece, Piece::Group { inner, .. } if is_noise(inner)));

    // Split into `first - second - third` segments at top-level separators.
    let mut segments: Vec<(char, Vec<Piece>)> = vec![('-', Vec::new())];
    for piece in pieces {
        match piece {
            Piece::Sep(dash) => segments.push((dash, Vec::new())),
            other => {
                if let Some((_, last)) = segments.last_mut() {
                    last.push(other);
                }
            }
        }
    }

    let mut out = String::new();
    for (index, (dash, segment)) in segments.into_iter().enumerate() {
        let rendered = collapse_whitespace(&render(&strip_bare_feat(segment.clone())));
        if index > 0 && starts_with_feat(&rendered) {
            // `Song - feat. X (Remix)`: the credit goes, kept groups after it
            // stay with the title.
            let groups = render_groups(&segment);
            if !groups.is_empty() {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&groups);
            }
            continue;
        }
        if rendered.is_empty() || (index > 0 && is_noise(&rendered)) {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
            out.push(dash);
            out.push(' ');
        }
        out.push_str(&rendered);
    }

    if out != collapsed {
        // Removing a group can leave a dangling separator: `Song (Video) -`.
        out = out
            .trim_matches(|c: char| c == ' ' || c == '|' || c == ':' || DASHES.contains(&c))
            .to_string();
    }
    if out.is_empty() {
        collapsed
    } else {
        out
    }
}

/// Removes channel noise from an artist name: a trailing ` - Topic`, a trailing
/// `VEVO` (`TaylorSwiftVEVO` → `TaylorSwift`), and `Official` suffixes like
/// ` Official`, ` Official Channel` and ` Official Artist Channel` (all
/// case-insensitive). Trims whitespace and collapses repeated spaces. A suffix
/// is only removed when something is left (`VEVO` alone stays `VEVO`).
pub fn clean_artist(artist: &str) -> String {
    let mut current = collapse_whitespace(artist);
    while let Some(next) = strip_channel_suffix(&current) {
        if next.is_empty() || next == current {
            break;
        }
        current = next;
    }
    current
}

/// The first credited artist: the text before the first `, `, ` & `, ` x `,
/// ` X `, ` feat. `, ` ft. `, ` featuring `, ` with `, ` and `, `; ` or ` / `.
/// Returns the trimmed input when there is no separator.
///
/// Word separators match in any letter case (` Feat. `, ` AND `). A separator
/// with nothing before it (`, Someone`) is skipped.
pub fn primary_artist(artist: &str) -> String {
    let trimmed = artist.trim();
    // ASCII lowercasing keeps every byte offset, so offsets found in `lower`
    // are valid in `trimmed`.
    let lower = trimmed.to_ascii_lowercase();
    let mut cut: Option<usize> = None;
    for separator in ARTIST_SEPARATORS {
        let found = lower
            .match_indices(separator)
            .map(|(index, _)| index)
            .find(|&index| {
                trimmed
                    .get(..index)
                    .is_some_and(|head| !head.trim().is_empty())
            });
        if let Some(index) = found {
            cut = Some(match cut {
                Some(previous) => previous.min(index),
                None => index,
            });
        }
    }
    match cut.and_then(|index| trimmed.get(..index)) {
        Some(head) => head.trim().to_string(),
        None => trimmed.to_string(),
    }
}

/// Splits `Artist - Title` (also ` – ` and ` — `) at the first separator.
/// Returns `None` when there is no separator or either side is empty.
/// Both sides are trimmed.
pub fn split_artist_title(s: &str) -> Option<(String, String)> {
    let (index, len) = TITLE_SEPARATORS
        .iter()
        .filter_map(|separator| s.find(separator).map(|index| (index, separator.len())))
        .min_by_key(|&(index, _)| index)?;
    let left = s.get(..index)?.trim();
    let right = s.get(index.saturating_add(len)..)?.trim();
    if left.is_empty() || right.is_empty() {
        None
    } else {
        Some((left.to_string(), right.to_string()))
    }
}

/// Applies all cleanup to a track for lookups.
///
/// When the artist is empty, or looks like a video channel (ends with ` - Topic`
/// or `VEVO`, or equals a known site name such as `YouTube`), and the title
/// contains an `Artist - Title` separator, the artist and title are taken from
/// the title. Then [`clean_title`] and [`clean_artist`] are applied. Album,
/// duration and Spotify id are kept as they are.
///
/// Details: an `Official`-style suffix (` Official`, ` Official Channel`) and
/// placeholders (`Unknown Artist`, `Various Artists`) also count as a channel.
/// The title is split the same way when the text before the separator is the
/// artist itself (a channel named `Rick Astley` posting `Rick Astley - Never
/// Gonna Give You Up`). Only a separator outside brackets counts, and not one
/// that merely starts a noise suffix (`Song - Remastered 2011` stays one
/// title). Noise groups before the artist (`【MV】Artist - Song`) are dropped,
/// and text before the separator that is only noise (`(Official Video) -
/// Song`) is never taken as the artist. An artist that is only `VEVO` counts
/// as a channel too.
pub fn normalize_track(track: &Track) -> Track {
    let channel = looks_like_channel(&track.artist);
    let (artist, title) = match split_channel_title(&track.title) {
        Some((left, right)) if channel => (left, right),
        Some((left, right)) if same_artist(&left, &track.artist) => (track.artist.clone(), right),
        _ => (track.artist.clone(), track.title.clone()),
    };
    Track {
        title: clean_title(&title),
        artist: clean_artist(&artist),
        album: track.album.clone(),
        duration_ms: track.duration_ms,
        spotify_id: track.spotify_id.clone(),
    }
}

/// A comparison key: lowercase, accents removed for Latin letters (é → e,
/// ü → u, ß → ss, ø → o, å → a, æ → ae, œ → oe, ñ → n, ç → c, and so on),
/// `&` read as `and`, every non-alphanumeric character dropped, then runs of
/// whitespace collapsed to one space and trimmed. Non-Latin scripts (Cyrillic,
/// Greek, CJK, Arabic …) are kept, lowercased where they have case.
///
/// Full-width forms (`ＬＯＶＥ`) are read as their ASCII letters and digits,
/// and the Greek final sigma `ς` as `σ`, so differently typed copies of the
/// same name get the same key.
pub fn normalize_key(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let c = fullwidth_to_ascii(c).unwrap_or(c);
        for lower in c.to_lowercase() {
            if lower == '&' {
                out.push_str(" and ");
            } else if lower.is_whitespace() {
                out.push(' ');
            } else if let Some(folded) = fold_latin(lower) {
                out.push_str(folded);
            } else if lower == 'ς' {
                out.push('σ');
            } else if lower.is_alphanumeric() {
                out.push(lower);
            }
        }
    }
    collapse_whitespace(&out)
}

/// Weight of the title similarity in [`score_candidate`].
const W_TITLE: f64 = 0.5;
/// Weight of the artist similarity in [`score_candidate`].
const W_ARTIST: f64 = 0.35;
/// Weight of the duration similarity in [`score_candidate`].
const W_DURATION: f64 = 0.15;
/// Highest score for a result whose title does not contain the wanted title.
const NOT_CONTAINED_CAP: f64 = 0.49;
/// Longest key (in characters) compared with Levenshtein distance; longer keys
/// are cut so scoring stays fast on absurd input.
const MAX_LEVENSHTEIN_CHARS: usize = 256;

/// How well a search result matches the wanted track, from 0.0 to 1.0.
///
/// Title similarity weighs most, then artist similarity (compared on
/// [`normalize_key`] of the cleaned values; containment of the primary artist
/// counts as a strong match), then duration: within 2 s is a full match,
/// within 5 s partial, more than 10 s apart scores the duration part 0. When
/// either duration is unknown the duration part is neutral. A result whose
/// cleaned title key does not match or contain the wanted title key at all
/// scores below 0.5.
///
/// The score is `0.5 × title + 0.35 × artist + 0.15 × duration`, each part
/// from 0.0 to 1.0. Both titles go through [`clean_title`] first.
/// - Title: equal keys 1.0; equal once spaces are removed 0.95; the result's
///   key contains the wanted key as whole words (`Song` in `Song Live`)
///   0.30–0.49 depending on how many extra words it has, so a longer title only
///   passes a 0.6 threshold when artist and duration back it up; otherwise
///   the whole score is capped at 0.49. Titles made only of symbols are
///   compared as written.
/// - Artist (after [`clean_artist`]): equal keys 1.0; equal without spaces
///   (`RickAstley`) 0.95; one side containing the other's primary artist as
///   whole words 0.9; otherwise `0.85 × t²` with `t = max(0, 2 × (s − 0.5))`,
///   where `s` is the best of normalized Levenshtein similarity and word
///   overlap, so names less than half alike score 0. An unknown wanted artist
///   (empty, a site name such as `YouTube`, or `Unknown Artist`) is neutral: 0.5.
/// - Duration: 1.0 within 2 s, falling linearly to 0.5 at 5 s and to 0 at
///   10 s. Unknown (or 0) on either side is neutral: 0.5.
pub fn score_candidate(
    wanted: &Track,
    cand_title: &str,
    cand_artist: &str,
    cand_duration_ms: Option<u64>,
) -> f64 {
    let (title, contained) =
        title_similarity(&clean_title(&wanted.title), &clean_title(cand_title));
    let artist = artist_similarity(&wanted.artist, cand_artist);
    let duration = duration_similarity(wanted.duration_ms, cand_duration_ms);
    let score = W_TITLE * title + W_ARTIST * artist + W_DURATION * duration;
    let score = if contained {
        score
    } else {
        score.min(NOT_CONTAINED_CAP)
    };
    if score.is_nan() {
        0.0
    } else {
        score.clamp(0.0, 1.0)
    }
}

/// A stable key for one song, used for per-song timing offsets and caching:
/// `normalize_key(primary_artist(clean_artist(artist))) + " - " +
/// normalize_key(clean_title(title))`.
pub fn song_key(track: &Track) -> String {
    let artist = normalize_key(&primary_artist(&clean_artist(&track.artist)));
    let title = normalize_key(&clean_title(&track.title));
    format!("{artist} - {title}")
}

/// True when one of the `candidate` texts (a search result's title, aliases,
/// album) names a recording without the singing and none of the `wanted`
/// texts does, so `Song (Instrumental)` is not used for `Song`, but is for
/// `Song (Karaoke)`.
///
/// Such recordings are named by the words `Instrumental(s)`, `Karaoke` and
/// `Inst`, the word pairs `Off Vocal`, `Backing Track` and `Minus One`
/// (compared on [`normalize_key`], so letter case and punctuation do not
/// matter), or anywhere in the text by `伴奏`, `纯音乐`, `純音樂`, `カラオケ`
/// and `オフボーカル`.
pub fn is_other_version(wanted: &[&str], candidate: &[&str]) -> bool {
    let without_singing = |texts: &[&str]| texts.iter().any(|text| names_no_singing(text));
    without_singing(candidate) && !without_singing(wanted)
}

/// The items scoring at least `min_score`, best first, at most `max` of
/// them. Ties keep their order, and a NaN score never passes.
pub fn best_scored<T>(scored: Vec<(f64, T)>, min_score: f64, max: usize) -> Vec<T> {
    let mut passing: Vec<(f64, T)> = scored
        .into_iter()
        .filter(|(score, _)| *score >= min_score)
        .collect();
    // `sort_by` is stable, so equal scores keep their order.
    passing.sort_by(|a, b| b.0.total_cmp(&a.0));
    passing
        .into_iter()
        .take(max)
        .map(|(_, item)| item)
        .collect()
}

/// See [`is_other_version`].
fn names_no_singing(text: &str) -> bool {
    let key = normalize_key(text);
    let words: Vec<&str> = key.split(' ').collect();
    NO_SINGING_WORDS.iter().any(|word| words.contains(word))
        || words.windows(2).any(|pair| {
            NO_SINGING_PAIRS
                .iter()
                .any(|&(first, second)| pair[0] == first && pair[1] == second)
        })
        || NO_SINGING_MARKERS.iter().any(|marker| key.contains(marker))
}

// ---------------------------------------------------------------------------
// Title parsing
// ---------------------------------------------------------------------------

/// Opening and closing characters of groups that may hold noise.
const BRACKETS: [(char, char); 5] = [
    ('(', ')'),
    ('[', ']'),
    ('（', '）'),
    ('［', '］'),
    ('【', '】'),
];

/// Dashes that separate parts of a title when they stand between spaces.
const DASHES: [char; 3] = ['-', '–', '—'];

/// Separators accepted by [`split_artist_title`].
const TITLE_SEPARATORS: [&str; 3] = [" - ", " – ", " — "];

/// Separators accepted by [`primary_artist`], lowercase.
const ARTIST_SEPARATORS: [&str; 10] = [
    ", ",
    " & ",
    " x ",
    " feat. ",
    " ft. ",
    " featuring ",
    " with ",
    " and ",
    "; ",
    " / ",
];

/// Words that may make up a noise group, as long as one anchor is present.
const NOISE_WORDS: [&str; 30] = [
    "official",
    "video",
    "music",
    "audio",
    "lyric",
    "lyrics",
    "visualizer",
    "visualiser",
    "hd",
    "hq",
    "4k",
    "1080p",
    "720p",
    "explicit",
    "clean",
    "radio",
    "edit",
    "single",
    "album",
    "version",
    "mono",
    "stereo",
    "bonus",
    "track",
    "mv",
    "digital",
    "digitally",
    "anniversary",
    "edition",
    "in",
];

/// Words that make a group of [`NOISE_WORDS`] noise on their own. Any word
/// starting with `remaster` is an anchor too.
const NOISE_ANCHORS: [&str; 17] = [
    "official",
    "video",
    "audio",
    "lyric",
    "lyrics",
    "visualizer",
    "visualiser",
    "hd",
    "hq",
    "4k",
    "1080p",
    "720p",
    "explicit",
    "clean",
    "mono",
    "stereo",
    "mv",
];

/// Word pairs that are noise although neither word is an anchor alone.
const NOISE_PAIRS: [(&str, &str); 6] = [
    ("radio", "edit"),
    ("radio", "version"),
    ("single", "version"),
    ("single", "edit"),
    ("album", "version"),
    ("bonus", "track"),
];

/// First words of a featured-artist credit inside a group.
const FEAT_WORDS: [&str; 3] = ["feat", "ft", "featuring"];

/// A featured-artist credit standing in the title itself (`Song ft. Someone`).
const BARE_FEAT_WORDS: [&str; 3] = ["feat.", "ft.", "featuring"];

/// Words that name a recording without the singing, for [`is_other_version`].
const NO_SINGING_WORDS: [&str; 4] = ["instrumental", "instrumentals", "karaoke", "inst"];

/// Word pairs that name a recording without the singing.
const NO_SINGING_PAIRS: [(&str, &str); 3] =
    [("off", "vocal"), ("backing", "track"), ("minus", "one")];

/// Chinese and Japanese text that names a recording without the singing:
/// accompaniment, pure music (simplified and traditional), karaoke and off
/// vocal.
const NO_SINGING_MARKERS: [&str; 5] = ["伴奏", "纯音乐", "純音樂", "カラオケ", "オフボーカル"];

/// Artist names that say nothing about the artist.
const PLACEHOLDER_ARTISTS: [&str; 10] = [
    "youtube",
    "youtube music",
    "soundcloud",
    "vimeo",
    "dailymotion",
    "twitch",
    "bandcamp",
    "tiktok",
    "unknown artist",
    "various artists",
];

/// One part of a title.
#[derive(Debug, Clone, PartialEq)]
enum Piece {
    /// Plain text between groups and separators.
    Text(String),
    /// A complete bracket group, e.g. `(Official Video)`.
    Group {
        open: char,
        inner: String,
        close: char,
    },
    /// A ` - ` separator outside any group; holds the dash used.
    Sep(char),
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn closer_for(c: char) -> Option<char> {
    BRACKETS
        .iter()
        .find(|(open, _)| *open == c)
        .map(|(_, close)| *close)
}

/// For every opening bracket, the index of its closing bracket when there is
/// one. Closers that do not match the innermost open group are ignored, and
/// groups that are never closed stay plain text.
fn match_brackets(chars: &[char]) -> Vec<Option<usize>> {
    let mut matches = vec![None; chars.len()];
    let mut stack: Vec<(char, usize)> = Vec::new();
    for (index, &c) in chars.iter().enumerate() {
        if let Some(close) = closer_for(c) {
            stack.push((close, index));
        } else if let Some(&(close, open)) = stack.last() {
            if c == close {
                stack.pop();
                if let Some(slot) = matches.get_mut(open) {
                    *slot = Some(index);
                }
            }
        }
    }
    matches
}

/// Splits a whitespace-collapsed title into text, top-level bracket groups and
/// top-level ` - ` separators. Rendering the pieces gives the input back.
fn parse_pieces(s: &str) -> Vec<Piece> {
    let chars: Vec<char> = s.chars().collect();
    let matches = match_brackets(&chars);
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut index = 0;
    while let Some(&c) = chars.get(index) {
        if let Some(end) = matches.get(index).copied().flatten() {
            if !text.is_empty() {
                pieces.push(Piece::Text(std::mem::take(&mut text)));
            }
            let inner: String = chars
                .get(index.saturating_add(1)..end)
                .unwrap_or_default()
                .iter()
                .collect();
            let close = chars.get(end).copied().unwrap_or(c);
            pieces.push(Piece::Group {
                open: c,
                inner,
                close,
            });
            index = end.saturating_add(1);
            continue;
        }
        let next = chars.get(index.saturating_add(1)).copied();
        let after = chars.get(index.saturating_add(2)).copied();
        if let (' ', Some(dash), Some(' ')) = (c, next, after) {
            if DASHES.contains(&dash) {
                if !text.is_empty() {
                    pieces.push(Piece::Text(std::mem::take(&mut text)));
                }
                pieces.push(Piece::Sep(dash));
                // The space after the dash starts the next text piece.
                index = index.saturating_add(2);
                continue;
            }
        }
        text.push(c);
        index = index.saturating_add(1);
    }
    if !text.is_empty() {
        pieces.push(Piece::Text(text));
    }
    pieces
}

fn render(pieces: &[Piece]) -> String {
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Text(text) => out.push_str(text),
            Piece::Group { open, inner, close } => {
                out.push(*open);
                out.push_str(inner);
                out.push(*close);
            }
            Piece::Sep(dash) => {
                out.push(' ');
                out.push(*dash);
            }
        }
    }
    out
}

/// The bracket groups of a segment, separated by single spaces.
fn render_groups(pieces: &[Piece]) -> String {
    let groups: Vec<String> = pieces
        .iter()
        .filter(|piece| matches!(piece, Piece::Group { .. }))
        .map(|piece| render(std::slice::from_ref(piece)))
        .collect();
    groups.join(" ")
}

/// The first word of `text` is `feat`, `ft` or `featuring` (any case).
fn starts_with_feat(text: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric())
        .find(|word| !word.is_empty())
        .is_some_and(|word| FEAT_WORDS.contains(&word.to_lowercase().as_str()))
}

/// True for numbers, years, ordinals and decades: `2011`, `50th`, `80s`.
fn is_number_like(word: &str) -> bool {
    let digits = word.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let suffix = word.get(digits.len()..).unwrap_or("");
    !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
        && matches!(suffix, "" | "st" | "nd" | "rd" | "th" | "s")
}

/// Whether the text of a bracket group or ` - ` suffix is noise for lyric
/// lookups (see [`clean_title`]). Empty or symbol-only text is noise.
fn is_noise(content: &str) -> bool {
    let lower = content.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let Some(first) = words.first() else {
        return true;
    };
    if FEAT_WORDS.contains(first) {
        return true;
    }
    let all_known = words.iter().all(|word| {
        NOISE_WORDS.contains(word) || word.starts_with("remaster") || is_number_like(word)
    });
    if !all_known {
        return false;
    }
    let has_anchor = words
        .iter()
        .any(|word| NOISE_ANCHORS.contains(word) || word.starts_with("remaster"));
    let has_pair = words
        .windows(2)
        .any(|pair| matches!(pair, [a, b] if NOISE_PAIRS.contains(&(*a, *b))));
    has_anchor || has_pair
}

/// Byte offset of a bare featured-artist credit in `text`: a `feat.`, `ft.` or
/// `featuring` word with title text before it (in `text` or, when
/// `seen_before`, earlier in the segment) and a name after it.
fn find_bare_feat(text: &str, seen_before: bool) -> Option<usize> {
    let mut seen = seen_before;
    let mut offset = 0usize;
    for word in text.split(' ') {
        if !word.is_empty() {
            let rest = text.get(offset.saturating_add(word.len())..).unwrap_or("");
            let lower = word.to_lowercase();
            if seen && BARE_FEAT_WORDS.contains(&lower.as_str()) && !rest.trim().is_empty() {
                return Some(offset);
            }
            seen = true;
        }
        offset = offset.saturating_add(word.len()).saturating_add(1);
    }
    None
}

/// Cuts a bare `ft. Someone` credit out of one title segment. Text after the
/// credit is dropped; bracket groups after it (`(Remix)`) are kept.
fn strip_bare_feat(segment: Vec<Piece>) -> Vec<Piece> {
    let mut out = Vec::with_capacity(segment.len());
    let mut seen_content = false;
    let mut cut = false;
    for piece in segment {
        if cut {
            if matches!(piece, Piece::Group { .. }) {
                out.push(piece);
            }
            continue;
        }
        match piece {
            Piece::Text(text) => match find_bare_feat(&text, seen_content) {
                Some(offset) => {
                    out.push(Piece::Text(text.get(..offset).unwrap_or("").to_string()));
                    cut = true;
                }
                None => {
                    if !text.trim().is_empty() {
                        seen_content = true;
                    }
                    out.push(Piece::Text(text));
                }
            },
            other => {
                seen_content = true;
                out.push(other);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Artist helpers
// ---------------------------------------------------------------------------

/// `s` without `suffix` (ASCII, compared case-insensitively), or `None`.
fn strip_suffix_ascii_ci<'a>(s: &'a str, suffix: &str) -> Option<&'a str> {
    let start = s.len().checked_sub(suffix.len())?;
    let tail = s.get(start..)?;
    if tail.eq_ignore_ascii_case(suffix) {
        s.get(..start)
    } else {
        None
    }
}

/// Removes one channel suffix, or `None` when there is none.
fn strip_channel_suffix(s: &str) -> Option<String> {
    const SUFFIXES: [&str; 5] = [
        " - topic",
        " official artist channel",
        " official channel",
        " official",
        "vevo",
    ];
    SUFFIXES
        .iter()
        .find_map(|suffix| strip_suffix_ascii_ci(s, suffix))
        .map(|rest| rest.trim_end().to_string())
}

fn is_placeholder_artist(artist: &str) -> bool {
    let lower = collapse_whitespace(artist).to_lowercase();
    PLACEHOLDER_ARTISTS.contains(&lower.as_str())
}

/// Empty, a placeholder, or a video channel (` - Topic`, `…VEVO`, `… Official`).
/// A name that is only `VEVO` counts too: it ends with `VEVO`, although
/// [`clean_artist`] keeps it because nothing would be left.
fn looks_like_channel(artist: &str) -> bool {
    let collapsed = collapse_whitespace(artist);
    collapsed.is_empty()
        || strip_channel_suffix(&collapsed).is_some()
        || is_placeholder_artist(&collapsed)
}

/// Both names have the same non-empty key once channel noise is removed.
fn same_artist(a: &str, b: &str) -> bool {
    let key = normalize_key(&clean_artist(a));
    !key.is_empty() && key == normalize_key(&clean_artist(b))
}

/// Splits a raw `Artist - Title (…)` video title at its first separator
/// outside brackets; noise groups are removed from the artist side. A side
/// that is only noise (`Song - Remastered 2011`, `(Official Video) - Song`)
/// does not count; then the cleaned title is tried instead.
fn split_channel_title(title: &str) -> Option<(String, String)> {
    if let Some((artist, rest)) = split_top_level(title) {
        if !is_noise(&rest) && !is_noise(&artist) {
            return Some((clean_title(&artist), rest));
        }
    }
    split_top_level(&clean_title(title))
}

/// Like [`split_artist_title`], but ignores separators inside brackets.
fn split_top_level(s: &str) -> Option<(String, String)> {
    let pieces = parse_pieces(&collapse_whitespace(s));
    let position = pieces
        .iter()
        .position(|piece| matches!(piece, Piece::Sep(_)))?;
    let left = collapse_whitespace(&render(pieces.get(..position)?));
    let right = collapse_whitespace(&render(pieces.get(position.saturating_add(1)..)?));
    if left.is_empty() || right.is_empty() {
        None
    } else {
        Some((left, right))
    }
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

/// Full-width ASCII forms (`Ａ`, `１`, `＆`) as plain ASCII.
fn fullwidth_to_ascii(c: char) -> Option<char> {
    let code = u32::from(c);
    if (0xFF01..=0xFF5E).contains(&code) {
        char::from_u32(code - 0xFEE0)
    } else {
        None
    }
}

/// ASCII spelling of a lowercase accented Latin letter.
fn fold_latin(c: char) -> Option<&'static str> {
    let folded = match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'ǎ' | 'ǻ' | 'ȁ' | 'ȃ' | 'ạ' | 'ả'
        | 'ấ' | 'ầ' | 'ẩ' | 'ẫ' | 'ậ' | 'ắ' | 'ằ' | 'ẳ' | 'ẵ' | 'ặ' => "a",
        'æ' | 'ǽ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' | 'đ' | 'ð' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' | 'ȅ' | 'ȇ' | 'ẹ' | 'ẻ' | 'ẽ' | 'ế'
        | 'ề' | 'ể' | 'ễ' | 'ệ' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' | 'ǧ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' | 'ǐ' | 'ȉ' | 'ȋ' | 'ỉ' | 'ị' => {
            "i"
        }
        'ĳ' => "ij",
        'ĵ' => "j",
        'ķ' | 'ǩ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' | 'ŉ' | 'ŋ' | 'ǹ' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' | 'ơ' | 'ǒ' | 'ǿ' | 'ȍ' | 'ȏ' | 'ọ'
        | 'ỏ' | 'ố' | 'ồ' | 'ổ' | 'ỗ' | 'ộ' | 'ớ' | 'ờ' | 'ở' | 'ỡ' | 'ợ' => {
            "o"
        }
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' | 'ȑ' | 'ȓ' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' | 'ſ' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' | 'ț' => "t",
        'þ' => "th",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' | 'ư' | 'ǔ' | 'ǖ' | 'ǘ' | 'ǚ'
        | 'ǜ' | 'ȕ' | 'ȗ' | 'ụ' | 'ủ' | 'ứ' | 'ừ' | 'ử' | 'ữ' | 'ự' => "u",
        'ŵ' | 'ẁ' | 'ẃ' | 'ẅ' => "w",
        'ý' | 'ÿ' | 'ŷ' | 'ỳ' | 'ỵ' | 'ỷ' | 'ỹ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => return None,
    };
    Some(folded)
}

// ---------------------------------------------------------------------------
// Similarity
// ---------------------------------------------------------------------------

fn compact(key: &str) -> String {
    key.chars().filter(|c| *c != ' ').collect()
}

/// `needle` appears in `haystack` as whole words (both are keys).
fn word_contains(haystack: &str, needle: &str) -> bool {
    !needle.is_empty() && format!(" {haystack} ").contains(&format!(" {needle} "))
}

/// Overlap of the word sets of two keys (Sørensen–Dice), 0.0 to 1.0.
fn token_dice(a: &str, b: &str) -> f64 {
    let left: BTreeSet<&str> = a.split(' ').filter(|w| !w.is_empty()).collect();
    let right: BTreeSet<&str> = b.split(' ').filter(|w| !w.is_empty()).collect();
    let total = left.len().saturating_add(right.len());
    if total == 0 {
        return 1.0;
    }
    let common = left.intersection(&right).count();
    (2.0 * common as f64) / total as f64
}

/// `1 - distance / longer length`, on at most [`MAX_LEVENSHTEIN_CHARS`] chars.
fn levenshtein_ratio(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().take(MAX_LEVENSHTEIN_CHARS).collect();
    let b: Vec<char> = b.chars().take(MAX_LEVENSHTEIN_CHARS).collect();
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 1.0;
    }
    // One row of the edit-distance table; `row[j]` is the distance between the
    // first `i` chars of `a` and the first `j` chars of `b`.
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diagonal = i;
        let mut left = i.saturating_add(1);
        if let Some(first) = row.first_mut() {
            *first = left;
        }
        for (cell, cb) in row.iter_mut().skip(1).zip(b.iter()) {
            let up = *cell;
            let value = up
                .saturating_add(1)
                .min(left.saturating_add(1))
                .min(diagonal.saturating_add(usize::from(ca != cb)));
            diagonal = up;
            *cell = value;
            left = value;
        }
    }
    let distance = row.last().copied().unwrap_or(longest).min(longest);
    1.0 - distance as f64 / longest as f64
}

fn fuzzy_similarity(a: &str, b: &str) -> f64 {
    levenshtein_ratio(a, b).max(token_dice(a, b))
}

/// Title similarity of two cleaned titles, and whether the candidate matches
/// or contains the wanted title.
fn title_similarity(wanted: &str, candidate: &str) -> (f64, bool) {
    let wanted_key = normalize_key(wanted);
    let candidate_key = normalize_key(candidate);
    if wanted_key.is_empty() || candidate_key.is_empty() {
        // Titles made only of symbols ("?", "!!!") are compared as written.
        let wanted_raw = collapse_whitespace(wanted).to_lowercase();
        let candidate_raw = collapse_whitespace(candidate).to_lowercase();
        let same = !wanted_raw.is_empty() && wanted_raw == candidate_raw;
        return if same { (1.0, true) } else { (0.0, false) };
    }
    if wanted_key == candidate_key {
        return (1.0, true);
    }
    if compact(&wanted_key) == compact(&candidate_key) {
        return (0.95, true);
    }
    if word_contains(&candidate_key, &wanted_key) {
        return (0.3 + 0.19 * token_dice(&wanted_key, &candidate_key), true);
    }
    (0.45 * fuzzy_similarity(&wanted_key, &candidate_key), false)
}

fn artist_similarity(wanted: &str, candidate: &str) -> f64 {
    let wanted_clean = clean_artist(wanted);
    let candidate_clean = clean_artist(candidate);
    let wanted_key = normalize_key(&wanted_clean);
    let candidate_key = normalize_key(&candidate_clean);
    if wanted_key.is_empty() || is_placeholder_artist(&wanted_clean) {
        return 0.5;
    }
    if candidate_key.is_empty() {
        return 0.0;
    }
    if wanted_key == candidate_key {
        return 1.0;
    }
    if compact(&wanted_key) == compact(&candidate_key) {
        return 0.95;
    }
    let wanted_primary = normalize_key(&primary_artist(&wanted_clean));
    let candidate_primary = normalize_key(&primary_artist(&candidate_clean));
    let strong = word_contains(&candidate_key, &wanted_primary)
        || word_contains(&wanted_key, &candidate_primary)
        || (!wanted_primary.is_empty() && compact(&wanted_primary) == compact(&candidate_primary));
    if strong {
        return 0.9;
    }
    // Names less than half alike are simply different artists.
    let closeness = ((fuzzy_similarity(&wanted_key, &candidate_key) - 0.5) * 2.0).max(0.0);
    0.85 * closeness * closeness
}

fn duration_similarity(wanted: Option<u64>, candidate: Option<u64>) -> f64 {
    match (wanted.filter(|&d| d > 0), candidate.filter(|&d| d > 0)) {
        (Some(a), Some(b)) => {
            let diff = a.abs_diff(b);
            if diff <= 2_000 {
                1.0
            } else if diff <= 5_000 {
                1.0 - 0.5 * (diff - 2_000) as f64 / 3_000.0
            } else if diff < 10_000 {
                0.5 * (10_000 - diff) as f64 / 5_000.0
            } else {
                0.0
            }
        }
        _ => 0.5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str, artist: &str) -> Track {
        Track {
            title: title.to_string(),
            artist: artist.to_string(),
            ..Track::default()
        }
    }

    fn track_with_duration(title: &str, artist: &str, duration_ms: u64) -> Track {
        Track {
            duration_ms: Some(duration_ms),
            ..track(title, artist)
        }
    }

    // ----- clean_title ---------------------------------------------------

    #[test]
    fn clean_title_removes_remaster_suffixes() {
        assert_eq!(
            clean_title("Bohemian Rhapsody - Remastered 2011"),
            "Bohemian Rhapsody"
        );
        assert_eq!(
            clean_title("Bohemian Rhapsody (2011 Remaster)"),
            "Bohemian Rhapsody"
        );
        assert_eq!(
            clean_title("Bohemian Rhapsody - 2011 Remaster"),
            "Bohemian Rhapsody"
        );
        assert_eq!(clean_title("Help! (Remastered 2009)"), "Help!");
        assert_eq!(clean_title("Help! [Remastered]"), "Help!");
        assert_eq!(clean_title("Help! - Remastered"), "Help!");
        assert_eq!(clean_title("Let It Be (Remaster)"), "Let It Be");
        assert_eq!(
            clean_title("Yesterday (2009 Remastered Version)"),
            "Yesterday"
        );
        assert_eq!(clean_title("Hey Jude - Remastered Version"), "Hey Jude");
        assert_eq!(clean_title("Imagine (Digitally Remastered)"), "Imagine");
        assert_eq!(clean_title("Imagine (Digital Remaster 2010)"), "Imagine");
        assert_eq!(
            clean_title("Paint It Black (50th Anniversary Remaster)"),
            "Paint It Black"
        );
        assert_eq!(clean_title("Song (Remastered in HD)"), "Song");
        assert_eq!(clean_title("Song (80s Remaster)"), "Song");
    }

    #[test]
    fn clean_title_removes_video_and_audio_noise() {
        assert_eq!(
            clean_title("Mr. Brightside (Official Music Video)"),
            "Mr. Brightside"
        );
        assert_eq!(
            clean_title("Mr. Brightside (Official Video)"),
            "Mr. Brightside"
        );
        assert_eq!(clean_title("Song [Official Audio]"), "Song");
        assert_eq!(clean_title("Song (Official Lyric Video)"), "Song");
        assert_eq!(clean_title("Song (Lyric Video)"), "Song");
        assert_eq!(clean_title("Song (Lyrics)"), "Song");
        assert_eq!(clean_title("Song [Lyrics]"), "Song");
        assert_eq!(clean_title("Song (Audio)"), "Song");
        assert_eq!(clean_title("Song (Video)"), "Song");
        assert_eq!(clean_title("Song (Visualizer)"), "Song");
        assert_eq!(clean_title("Song (Official Visualiser)"), "Song");
        assert_eq!(clean_title("Song [HD]"), "Song");
        assert_eq!(clean_title("Song (HQ)"), "Song");
        assert_eq!(clean_title("Song (4K)"), "Song");
        assert_eq!(clean_title("Song (Official HD Video)"), "Song");
        assert_eq!(clean_title("Song (Official Video) [4K]"), "Song");
        assert_eq!(clean_title("Song (Music Video)"), "Song");
        assert_eq!(clean_title("Song (Official)"), "Song");
        assert_eq!(clean_title("Song (Official MV)"), "Song");
    }

    #[test]
    fn clean_title_removes_version_noise() {
        assert_eq!(clean_title("Song (Explicit)"), "Song");
        assert_eq!(clean_title("Song (Clean)"), "Song");
        assert_eq!(clean_title("Song (Clean Version)"), "Song");
        assert_eq!(clean_title("Song - Radio Edit"), "Song");
        assert_eq!(clean_title("Song (Radio Edit)"), "Song");
        assert_eq!(clean_title("Song - Single Version"), "Song");
        assert_eq!(clean_title("Song (Album Version)"), "Song");
        assert_eq!(clean_title("Song (Mono)"), "Song");
        assert_eq!(clean_title("Song - Mono Version"), "Song");
        assert_eq!(clean_title("Song (Stereo)"), "Song");
        assert_eq!(clean_title("Song [Bonus Track]"), "Song");
        assert_eq!(clean_title("Song - Bonus Track"), "Song");
    }

    #[test]
    fn clean_title_removes_featured_artists() {
        assert_eq!(
            clean_title("Blinding Lights (feat. X) [Official Audio]"),
            "Blinding Lights"
        );
        assert_eq!(clean_title("Song (ft. Someone)"), "Song");
        assert_eq!(clean_title("Song (Ft. Someone & Another)"), "Song");
        assert_eq!(clean_title("Song [featuring Someone]"), "Song");
        assert_eq!(clean_title("Song (feat Someone)"), "Song");
        assert_eq!(clean_title("Song - feat. Someone"), "Song");
        assert_eq!(clean_title("Song ft. Someone"), "Song");
        assert_eq!(clean_title("Song feat. Someone, Another"), "Song");
        assert_eq!(clean_title("Song Featuring Someone"), "Song");
        assert_eq!(clean_title("Song ft. Someone (Remix)"), "Song (Remix)");
        assert_eq!(clean_title("Song (Remix) ft. Someone"), "Song (Remix)");
        assert_eq!(
            clean_title("This Is What You Came For (Official Video) ft. Rihanna"),
            "This Is What You Came For"
        );
    }

    #[test]
    fn clean_title_keeps_markers_after_a_dash_feat_credit() {
        // The credit goes, the kept marker that follows it stays.
        assert_eq!(clean_title("Song - feat. Someone (Remix)"), "Song (Remix)");
        assert_eq!(clean_title("Song - ft. Someone [Live]"), "Song [Live]");
        assert_eq!(
            clean_title("Song - Featuring A & B (Acoustic) (Official Video)"),
            "Song (Acoustic)"
        );
        assert_eq!(
            clean_title("Song - feat. Someone (Remix) - Live"),
            "Song (Remix) - Live"
        );
        assert_eq!(clean_title("Song - feat. Someone"), "Song");
        assert_eq!(clean_title("Song - ft. Someone (Official Video)"), "Song");
    }

    #[test]
    fn clean_title_keeps_version_markers() {
        assert_eq!(clean_title("Song (Remix)"), "Song (Remix)");
        assert_eq!(
            clean_title("Song (Live at Wembley)"),
            "Song (Live at Wembley)"
        );
        assert_eq!(
            clean_title("All Too Well (Taylor's Version)"),
            "All Too Well (Taylor's Version)"
        );
        assert_eq!(clean_title("Song (Acoustic)"), "Song (Acoustic)");
        assert_eq!(
            clean_title("Song (Acoustic Version)"),
            "Song (Acoustic Version)"
        );
        assert_eq!(clean_title("Song (Instrumental)"), "Song (Instrumental)");
        assert_eq!(clean_title("Song (Sped Up)"), "Song (Sped Up)");
        assert_eq!(
            clean_title("Song (Slowed + Reverb)"),
            "Song (Slowed + Reverb)"
        );
        assert_eq!(clean_title("Song (Demo)"), "Song (Demo)");
        assert_eq!(clean_title("Song - Live"), "Song - Live");
        assert_eq!(
            clean_title("Song - Live at Wembley"),
            "Song - Live at Wembley"
        );
        assert_eq!(clean_title("Song (Extended Mix)"), "Song (Extended Mix)");
        assert_eq!(
            clean_title("Song (Remastered Live Version)"),
            "Song (Remastered Live Version)"
        );
        assert_eq!(clean_title("Song (with Someone)"), "Song (with Someone)");
        assert_eq!(clean_title("Song (2011)"), "Song (2011)");
        assert_eq!(clean_title("Song (Edit)"), "Song (Edit)");
        assert_eq!(clean_title("Song (Part 2)"), "Song (Part 2)");
        assert_eq!(
            clean_title("(I Can't Get No) Satisfaction"),
            "(I Can't Get No) Satisfaction"
        );
        assert_eq!(
            clean_title("Song (From \"The Movie\")"),
            "Song (From \"The Movie\")"
        );
    }

    #[test]
    fn clean_title_mixes_kept_and_removed_groups() {
        assert_eq!(clean_title("Song (Remix) [Official Video]"), "Song (Remix)");
        assert_eq!(clean_title("Song (Live) (Remastered)"), "Song (Live)");
        assert_eq!(
            clean_title("Song (feat. X) (Acoustic) [HD]"),
            "Song (Acoustic)"
        );
        assert_eq!(
            clean_title("Song - Live at Wembley - 2011 Remaster"),
            "Song - Live at Wembley"
        );
        assert_eq!(
            clean_title("Song (Live - Remastered 2011)"),
            "Song (Live - Remastered 2011)"
        );
        assert_eq!(
            clean_title("Song - Remastered 2011 (Live)"),
            "Song - Remastered 2011 (Live)"
        );
    }

    #[test]
    fn clean_title_never_removes_the_first_segment() {
        assert_eq!(clean_title("Video"), "Video");
        assert_eq!(clean_title("Clean"), "Clean");
        assert_eq!(clean_title("Lyrics"), "Lyrics");
        assert_eq!(clean_title("Remastered"), "Remastered");
        assert_eq!(clean_title("India.Arie - Video"), "India.Arie");
        assert_eq!(clean_title("Video (Official Video)"), "Video");
    }

    #[test]
    fn clean_title_keeps_artist_prefix_and_strips_noise() {
        assert_eq!(
            clean_title("Rick Astley - Never Gonna Give You Up (Official Video)"),
            "Rick Astley - Never Gonna Give You Up"
        );
        assert_eq!(
            clean_title("Rick Astley - Never Gonna Give You Up - Official Video"),
            "Rick Astley - Never Gonna Give You Up"
        );
        assert_eq!(
            clean_title("Artist – Song (Official Video)"),
            "Artist – Song"
        );
        assert_eq!(clean_title("Artist — Song — Remastered"), "Artist — Song");
    }

    #[test]
    fn clean_title_handles_whitespace() {
        assert_eq!(clean_title("  Song   Title  "), "Song Title");
        assert_eq!(clean_title("Song\t(Official Video)\n"), "Song");
        assert_eq!(clean_title("Song  (Remix)   [HD]  "), "Song (Remix)");
        assert_eq!(clean_title("Song\u{a0}Title"), "Song Title");
        assert_eq!(clean_title(""), "");
        assert_eq!(clean_title("   "), "");
    }

    #[test]
    fn clean_title_handles_odd_brackets() {
        assert_eq!(clean_title("Song ()"), "Song");
        assert_eq!(clean_title("Song (Official Video"), "Song (Official Video");
        assert_eq!(clean_title("Song Official Video)"), "Song Official Video)");
        assert_eq!(clean_title("Song ((Official Video))"), "Song");
        assert_eq!(clean_title("Song (Live [HD])"), "Song (Live [HD])");
        assert_eq!(clean_title("Song (a ] b) [HD]"), "Song (a ] b)");
        assert_eq!(
            clean_title("Song (unclosed [Official Video]"),
            "Song (unclosed"
        );
        assert_eq!(clean_title("Song (Official Video) -"), "Song");
        assert_eq!(clean_title("(Official Video) - Song"), "Song");
        assert_eq!(clean_title("((((("), "(((((");
        assert_eq!(clean_title(")))))"), ")))))");
        assert_eq!(clean_title("-"), "-");
        assert_eq!(clean_title(" - "), "-");
        assert_eq!(clean_title("Song - "), "Song -");
        assert_eq!(clean_title("A - - B"), "A - B");
    }

    #[test]
    fn clean_title_returns_input_when_everything_is_noise() {
        assert_eq!(clean_title("(Official Video)"), "(Official Video)");
        assert_eq!(clean_title("[HD] (Lyrics)"), "[HD] (Lyrics)");
        assert_eq!(clean_title("(feat. Someone)"), "(feat. Someone)");
    }

    #[test]
    fn clean_title_handles_unicode_titles() {
        assert_eq!(
            clean_title("夜に駆ける (Official Music Video)"),
            "夜に駆ける"
        );
        assert_eq!(clean_title("【MV】夜に駆ける"), "夜に駆ける");
        assert_eq!(
            clean_title("夜に駆ける【Official Music Video】"),
            "夜に駆ける"
        );
        assert_eq!(clean_title("Песня (Official Video)"), "Песня");
        assert_eq!(clean_title("Песня (Ремикс)"), "Песня (Ремикс)");
        assert_eq!(clean_title("Canción（Remastered）"), "Canción");
        assert_eq!(clean_title("ქართული სიმღერა [Lyrics]"), "ქართული სიმღერა");
        assert_eq!(clean_title("🎵 Song 🎵 (Official Video)"), "🎵 Song 🎵");
        assert_eq!(clean_title("Song (Vídeo Oficial)"), "Song (Vídeo Oficial)");
    }

    #[test]
    fn clean_title_is_idempotent_on_examples() {
        for title in [
            "Bohemian Rhapsody - Remastered 2011",
            "Blinding Lights (feat. X) [Official Audio]",
            "Song (Remix) ft. Someone",
            "Song - Live at Wembley - 2011 Remaster",
            "Rick Astley - Never Gonna Give You Up (Official Video)",
            "Song (Official Video) -",
            "Song - feat. Someone (Remix)",
            "Song - ft. Someone [Live] (Official Video) - Remastered",
        ] {
            let once = clean_title(title);
            assert_eq!(clean_title(&once), once, "{title}");
        }
    }

    #[test]
    fn clean_title_handles_long_input() {
        let long = "(".repeat(50_000) + &")".repeat(10);
        let cleaned = clean_title(&long);
        assert!(!cleaned.is_empty());
        let words = "word ".repeat(20_000) + "(Official Video)";
        assert_eq!(clean_title(&words), "word ".repeat(20_000).trim_end());
    }

    // ----- clean_artist --------------------------------------------------

    #[test]
    fn clean_artist_removes_channel_suffixes() {
        assert_eq!(clean_artist("Rick Astley - Topic"), "Rick Astley");
        assert_eq!(clean_artist("Rick Astley - topic"), "Rick Astley");
        assert_eq!(clean_artist("RickAstleyVEVO"), "RickAstley");
        assert_eq!(clean_artist("TaylorSwiftVEVO"), "TaylorSwift");
        assert_eq!(clean_artist("ArianaGrandeVevo"), "ArianaGrande");
        assert_eq!(clean_artist("Taylor Swift VEVO"), "Taylor Swift");
        assert_eq!(clean_artist("Rick Astley Official"), "Rick Astley");
        assert_eq!(clean_artist("BLACKPINK OFFICIAL"), "BLACKPINK");
        assert_eq!(clean_artist("Artist Official Channel"), "Artist");
        assert_eq!(clean_artist("Artist Official Artist Channel"), "Artist");
        assert_eq!(clean_artist("ArtistVEVO - Topic"), "Artist");
    }

    #[test]
    fn clean_artist_keeps_real_names() {
        assert_eq!(clean_artist("Rick Astley"), "Rick Astley");
        assert_eq!(clean_artist("  Beyoncé  "), "Beyoncé");
        assert_eq!(clean_artist("Simon  &  Garfunkel"), "Simon & Garfunkel");
        assert_eq!(clean_artist("Topic"), "Topic");
        assert_eq!(clean_artist("VEVO"), "VEVO");
        assert_eq!(clean_artist("Official"), "Official");
        assert_eq!(clean_artist("Officially"), "Officially");
        assert_eq!(clean_artist("Сплин"), "Сплин");
        assert_eq!(clean_artist("米津玄師"), "米津玄師");
        assert_eq!(clean_artist(""), "");
        assert_eq!(clean_artist("   "), "");
    }

    #[test]
    fn clean_artist_handles_multibyte_endings() {
        assert_eq!(clean_artist("Ёё"), "Ёё");
        assert_eq!(clean_artist("ąvevo"), "ą");
        assert_eq!(clean_artist("é"), "é");
        assert_eq!(clean_artist("🎵VEVO"), "🎵");
    }

    // ----- primary_artist ------------------------------------------------

    #[test]
    fn primary_artist_takes_the_first_credit() {
        assert_eq!(primary_artist("Calvin Harris, Rihanna"), "Calvin Harris");
        assert_eq!(primary_artist("Simon & Garfunkel"), "Simon");
        assert_eq!(primary_artist("Skrillex x Diplo"), "Skrillex");
        assert_eq!(primary_artist("Skrillex X Diplo"), "Skrillex");
        assert_eq!(primary_artist("Artist feat. Other"), "Artist");
        assert_eq!(primary_artist("Artist Feat. Other"), "Artist");
        assert_eq!(primary_artist("Artist ft. Other"), "Artist");
        assert_eq!(primary_artist("Artist featuring Other"), "Artist");
        assert_eq!(primary_artist("Artist with Other"), "Artist");
        assert_eq!(primary_artist("Artist and Other"), "Artist");
        assert_eq!(primary_artist("Artist; Other"), "Artist");
        assert_eq!(primary_artist("Artist / Other"), "Artist");
    }

    #[test]
    fn primary_artist_uses_the_earliest_separator() {
        assert_eq!(primary_artist("A & B, C"), "A");
        assert_eq!(primary_artist("A, B & C"), "A");
        assert_eq!(primary_artist("Earth, Wind & Fire"), "Earth");
        assert_eq!(primary_artist("A feat. B, C"), "A");
    }

    #[test]
    fn primary_artist_without_separator_is_trimmed_input() {
        assert_eq!(primary_artist("  Rick Astley  "), "Rick Astley");
        assert_eq!(primary_artist("AC/DC"), "AC/DC");
        assert_eq!(primary_artist("Xzibit"), "Xzibit");
        assert_eq!(primary_artist("Max"), "Max");
        assert_eq!(primary_artist("Sandy"), "Sandy");
        assert_eq!(primary_artist("Andy"), "Andy");
        assert_eq!(primary_artist(""), "");
        assert_eq!(primary_artist("Сплин"), "Сплин");
    }

    #[test]
    fn primary_artist_skips_separators_with_nothing_before() {
        assert_eq!(primary_artist(", Someone"), ", Someone");
        assert_eq!(primary_artist(", A, B"), ", A");
        assert_eq!(primary_artist("Бумбокс & Сплин"), "Бумбокс");
        assert_eq!(primary_artist("Beyoncé, JAY-Z"), "Beyoncé");
    }

    // ----- split_artist_title --------------------------------------------

    #[test]
    fn split_artist_title_splits_at_the_first_separator() {
        assert_eq!(
            split_artist_title("Rick Astley - Never Gonna Give You Up (Official Video)"),
            Some((
                "Rick Astley".to_string(),
                "Never Gonna Give You Up (Official Video)".to_string()
            ))
        );
        assert_eq!(
            split_artist_title("A - B - C"),
            Some(("A".to_string(), "B - C".to_string()))
        );
        assert_eq!(
            split_artist_title("Artist – Song"),
            Some(("Artist".to_string(), "Song".to_string()))
        );
        assert_eq!(
            split_artist_title("Artist — Song"),
            Some(("Artist".to_string(), "Song".to_string()))
        );
        assert_eq!(
            split_artist_title("A — B - C"),
            Some(("A".to_string(), "B - C".to_string()))
        );
        assert_eq!(
            split_artist_title("Сплин - Выхода нет"),
            Some(("Сплин".to_string(), "Выхода нет".to_string()))
        );
    }

    #[test]
    fn split_artist_title_rejects_missing_parts() {
        assert_eq!(split_artist_title("Song"), None);
        assert_eq!(split_artist_title("Jay-Z"), None);
        assert_eq!(split_artist_title("A-B"), None);
        assert_eq!(split_artist_title(" - Song"), None);
        assert_eq!(split_artist_title("Artist - "), None);
        assert_eq!(split_artist_title(" - "), None);
        assert_eq!(split_artist_title(""), None);
    }

    // ----- normalize_track -----------------------------------------------

    #[test]
    fn normalize_track_splits_youtube_titles() {
        let title = "Rick Astley - Never Gonna Give You Up (Official Video)";
        for channel in ["RickAstleyVEVO", "Rick Astley - Topic", "", "YouTube", "  "] {
            let normalized = normalize_track(&track(title, channel));
            assert_eq!(normalized.artist, "Rick Astley", "channel {channel:?}");
            assert_eq!(
                normalized.title, "Never Gonna Give You Up",
                "channel {channel:?}"
            );
        }
    }

    #[test]
    fn normalize_track_cleans_topic_channels_without_split() {
        let normalized = normalize_track(&track("Never Gonna Give You Up", "Rick Astley - Topic"));
        assert_eq!(normalized.artist, "Rick Astley");
        assert_eq!(normalized.title, "Never Gonna Give You Up");
        let normalized = normalize_track(&track("Never Gonna Give You Up", "RickAstleyVEVO"));
        assert_eq!(normalized.artist, "RickAstley");
    }

    #[test]
    fn normalize_track_keeps_real_artists() {
        let normalized = normalize_track(&track("Artist - Song", "Real Band"));
        assert_eq!(normalized.artist, "Real Band");
        assert_eq!(normalized.title, "Artist - Song");
        let normalized = normalize_track(&track("Bohemian Rhapsody - Remastered 2011", "Queen"));
        assert_eq!(normalized.artist, "Queen");
        assert_eq!(normalized.title, "Bohemian Rhapsody");
    }

    #[test]
    fn normalize_track_does_not_split_noise_suffixes() {
        let normalized = normalize_track(&track("Bohemian Rhapsody - Remastered 2011", ""));
        assert_eq!(normalized.artist, "");
        assert_eq!(normalized.title, "Bohemian Rhapsody");
        let normalized = normalize_track(&track("Song - Radio Edit", "Various Artists"));
        assert_eq!(normalized.artist, "Various Artists");
        assert_eq!(normalized.title, "Song");
    }

    #[test]
    fn normalize_track_handles_placeholder_artists() {
        let normalized = normalize_track(&track("Artist - Song", "Various Artists"));
        assert_eq!(normalized.artist, "Artist");
        assert_eq!(normalized.title, "Song");
        let normalized = normalize_track(&track("Artist - Song", "Unknown Artist"));
        assert_eq!(normalized.artist, "Artist");
        let normalized = normalize_track(&track("Artist - Song", "SoundCloud"));
        assert_eq!(normalized.artist, "Artist");
    }

    #[test]
    fn normalize_track_ignores_separators_inside_brackets() {
        let normalized = normalize_track(&track("Song (Live - 2011)", ""));
        assert_eq!(normalized.artist, "");
        assert_eq!(normalized.title, "Song (Live - 2011)");
        let normalized = normalize_track(&track(
            "Artist - Song (Live - Wembley) [Official Video]",
            "ArtistVEVO",
        ));
        assert_eq!(normalized.artist, "Artist");
        assert_eq!(normalized.title, "Song (Live - Wembley)");
    }

    #[test]
    fn normalize_track_handles_messy_video_titles() {
        let normalized = normalize_track(&track(
            "Calvin Harris - This Is What You Came For (Official Video) ft. Rihanna",
            "CalvinHarrisVEVO",
        ));
        assert_eq!(normalized.artist, "Calvin Harris");
        assert_eq!(normalized.title, "This Is What You Came For");

        let normalized = normalize_track(&track("【MV】YOASOBI - 夜に駆ける", ""));
        assert_eq!(normalized.artist, "YOASOBI");
        assert_eq!(normalized.title, "夜に駆ける");

        let normalized = normalize_track(&track(
            "The Killers - Mr. Brightside - Official Video",
            "YouTube",
        ));
        assert_eq!(normalized.artist, "The Killers");
        assert_eq!(normalized.title, "Mr. Brightside");
    }

    #[test]
    fn normalize_track_drops_an_artist_prefix_that_is_the_artist() {
        let normalized = normalize_track(&track(
            "Rick Astley - Never Gonna Give You Up (Official Video)",
            "Rick Astley",
        ));
        assert_eq!(normalized.artist, "Rick Astley");
        assert_eq!(normalized.title, "Never Gonna Give You Up");

        let normalized = normalize_track(&track(
            "Queen - Bohemian Rhapsody (Official Video Remastered)",
            "Queen Official",
        ));
        assert_eq!(normalized.artist, "Queen");
        assert_eq!(normalized.title, "Bohemian Rhapsody");

        let normalized = normalize_track(&track(
            "Calvin Harris ft. Rihanna - This Is What You Came For",
            "Calvin Harris",
        ));
        assert_eq!(normalized.artist, "Calvin Harris");
        assert_eq!(normalized.title, "This Is What You Came For");

        // The reported spelling of the artist is kept.
        let normalized = normalize_track(&track("Beyonce - Halo", "Beyoncé"));
        assert_eq!(normalized.artist, "Beyoncé");
        assert_eq!(normalized.title, "Halo");

        // A different name before the dash is part of the title.
        let normalized = normalize_track(&track("Bohemian Rhapsody - Live", "Queen"));
        assert_eq!(normalized.artist, "Queen");
        assert_eq!(normalized.title, "Bohemian Rhapsody - Live");
    }

    #[test]
    fn normalize_track_never_takes_a_noise_group_as_the_artist() {
        // Text before the separator that is only noise is not an artist.
        for title in [
            "(Official Video) - Song",
            "[Lyrics] - Song",
            "【MV】 - Song",
        ] {
            let normalized = normalize_track(&track(title, ""));
            assert_eq!(normalized.artist, "", "{title}");
            assert_eq!(normalized.title, "Song", "{title}");
        }
        let normalized = normalize_track(&track("【MV】 - 夜に駆ける", "YOASOBI - Topic"));
        assert_eq!(normalized.artist, "YOASOBI");
        assert_eq!(normalized.title, "夜に駆ける");
        // A real name made of noise words still splits.
        let normalized = normalize_track(&track("Clean - Song", ""));
        assert_eq!(normalized.artist, "Clean");
        assert_eq!(normalized.title, "Song");
    }

    #[test]
    fn normalize_track_treats_a_bare_vevo_artist_as_a_channel() {
        // "VEVO" ends with VEVO, so it is a channel (the contract's rule), even
        // though clean_artist keeps it as it is.
        for channel in ["VEVO", "Vevo", " vevo "] {
            let normalized = normalize_track(&track("Artist – Song (Official Video)", channel));
            assert_eq!(normalized.artist, "Artist", "{channel:?}");
            assert_eq!(normalized.title, "Song", "{channel:?}");
        }
        // Without a separator there is nothing better than the channel name.
        let normalized = normalize_track(&track("Song", "VEVO"));
        assert_eq!(normalized.artist, "VEVO");
        assert_eq!(normalized.title, "Song");
    }

    #[test]
    fn normalize_track_keeps_other_fields() {
        let original = Track {
            title: "Song (Official Video)".to_string(),
            artist: "ArtistVEVO".to_string(),
            album: Some("Album (Deluxe)".to_string()),
            duration_ms: Some(212_000),
            spotify_id: Some("4uLU6hMCjMI75M1A2tKUQC".to_string()),
        };
        let normalized = normalize_track(&original);
        assert_eq!(normalized.title, "Song");
        assert_eq!(normalized.artist, "Artist");
        assert_eq!(normalized.album, original.album);
        assert_eq!(normalized.duration_ms, original.duration_ms);
        assert_eq!(normalized.spotify_id, original.spotify_id);
    }

    #[test]
    fn normalize_track_of_empty_track_is_empty() {
        assert_eq!(normalize_track(&Track::default()), Track::default());
    }

    // ----- normalize_key -------------------------------------------------

    #[test]
    fn normalize_key_folds_accents() {
        assert_eq!(normalize_key("Beyoncé"), "beyonce");
        assert_eq!(normalize_key("Sigur Rós"), "sigur ros");
        assert_eq!(normalize_key("Mötley Crüe"), "motley crue");
        assert_eq!(normalize_key("Straße"), "strasse");
        assert_eq!(normalize_key("STRAẞE"), "strasse");
        assert_eq!(normalize_key("Røyksopp"), "royksopp");
        assert_eq!(normalize_key("Åge Aleksandersen"), "age aleksandersen");
        assert_eq!(normalize_key("Æther"), "aether");
        assert_eq!(normalize_key("Œuvre"), "oeuvre");
        assert_eq!(normalize_key("Mañana"), "manana");
        assert_eq!(normalize_key("Façade"), "facade");
        assert_eq!(normalize_key("Łódź"), "lodz");
        assert_eq!(normalize_key("Ðoðo"), "dodo");
        assert_eq!(normalize_key("Þór"), "thor");
        assert_eq!(normalize_key("Şarkı"), "sarki");
        assert_eq!(normalize_key("İstanbul"), "istanbul");
        assert_eq!(normalize_key("Sơn Tùng M-TP"), "son tung mtp");
        assert_eq!(normalize_key("Dvořák"), "dvorak");
    }

    #[test]
    fn normalize_key_handles_decomposed_accents() {
        // "e" followed by a combining acute accent.
        assert_eq!(normalize_key("Beyonce\u{301}"), "beyonce");
        assert_eq!(normalize_key("Beyonce\u{301}"), normalize_key("Beyoncé"));
    }

    #[test]
    fn normalize_key_drops_punctuation_and_reads_ampersand() {
        assert_eq!(normalize_key("AC/DC"), "acdc");
        assert_eq!(normalize_key("Simon & Garfunkel"), "simon and garfunkel");
        assert_eq!(normalize_key("Simon&Garfunkel"), "simon and garfunkel");
        assert_eq!(normalize_key("Mr. Brightside"), "mr brightside");
        assert_eq!(normalize_key("Don't Stop Me Now!"), "dont stop me now");
        assert_eq!(normalize_key("Jay-Z"), "jayz");
        assert_eq!(normalize_key("P!nk"), "pnk");
        assert_eq!(normalize_key("  Hello,   World  "), "hello world");
        assert_eq!(normalize_key("Song (Remix)"), "song remix");
        assert_eq!(normalize_key("a\tb\nc"), "a b c");
        assert_eq!(normalize_key("🎵 Song 🎵"), "song");
    }

    #[test]
    fn normalize_key_keeps_other_scripts() {
        assert_eq!(normalize_key("Сплин"), "сплин");
        assert_eq!(normalize_key("ВЫХОДА НЕТ"), "выхода нет");
        assert_eq!(normalize_key("米津玄師 - Lemon"), "米津玄師 lemon");
        assert_eq!(normalize_key("夜に駆ける"), "夜に駆ける");
        assert_eq!(normalize_key("방탄소년단"), "방탄소년단");
        assert_eq!(normalize_key("ΣΟΦΙΑΣ"), "σοφιασ");
        assert_eq!(normalize_key("σοφιας"), "σοφιασ");
        assert_eq!(normalize_key("عمرو دياب"), "عمرو دياب");
        assert_eq!(normalize_key("ქართული"), "ქართული");
    }

    #[test]
    fn normalize_key_reads_fullwidth_forms() {
        assert_eq!(normalize_key("ＬＯＶＥ"), "love");
        assert_eq!(normalize_key("Ｔｏｋｙｏ　２０２０"), "tokyo 2020");
        assert_eq!(normalize_key("Ａ＆Ｂ"), "a and b");
    }

    #[test]
    fn normalize_key_of_empty_or_symbols_is_empty() {
        assert_eq!(normalize_key(""), "");
        assert_eq!(normalize_key("   "), "");
        assert_eq!(normalize_key("!!!"), "");
        assert_eq!(normalize_key("/-/"), "");
        assert_eq!(normalize_key("&"), "and");
    }

    // ----- song_key ------------------------------------------------------

    #[test]
    fn song_key_combines_primary_artist_and_clean_title() {
        assert_eq!(
            song_key(&track("Bohemian Rhapsody - Remastered 2011", "Queen")),
            "queen - bohemian rhapsody"
        );
        assert_eq!(
            song_key(&track(
                "This Is What You Came For",
                "Calvin Harris, Rihanna"
            )),
            "calvin harris - this is what you came for"
        );
        assert_eq!(
            song_key(&track("Halo (Official Video)", "BeyoncéVEVO")),
            "beyonce - halo"
        );
        assert_eq!(song_key(&track("Song (Remix)", "A & B")), "a - song remix");
        assert_eq!(
            song_key(&track("Выхода нет", "Сплин")),
            "сплин - выхода нет"
        );
        assert_eq!(song_key(&Track::default()), " - ");
    }

    #[test]
    fn song_key_is_stable_across_reported_variants() {
        let spotify = song_key(&track("Mr. Brightside", "The Killers"));
        let youtube = song_key(&track(
            "Mr. Brightside (Official Music Video)",
            "The Killers",
        ));
        let other = song_key(&track("MR BRIGHTSIDE", "the killers"));
        assert_eq!(spotify, youtube);
        assert_eq!(spotify, other);
    }

    #[test]
    fn song_key_ignores_album_duration_and_id() {
        let a = Track {
            album: Some("A".into()),
            duration_ms: Some(1),
            spotify_id: Some("x".into()),
            ..track("Song", "Artist")
        };
        assert_eq!(song_key(&a), song_key(&track("Song", "Artist")));
    }

    // ----- score_candidate -----------------------------------------------

    #[test]
    fn exact_match_scores_above_point_nine() {
        let wanted = track_with_duration("Bohemian Rhapsody", "Queen", 354_000);
        let score = score_candidate(&wanted, "Bohemian Rhapsody", "Queen", Some(354_000));
        assert!(score > 0.9, "{score}");
        assert!((score - 1.0).abs() < 1e-9, "{score}");
    }

    #[test]
    fn exact_match_with_noise_and_case_scores_above_point_nine() {
        let wanted = track_with_duration("Bohemian Rhapsody - Remastered 2011", "Queen", 354_320);
        let score = score_candidate(
            &wanted,
            "BOHEMIAN RHAPSODY (2011 Remaster)",
            "queen",
            Some(355_000),
        );
        assert!(score > 0.9, "{score}");
        let wanted = track_with_duration("Halo", "Beyonce", 261_000);
        assert!(score_candidate(&wanted, "Halo", "Beyoncé", Some(261_000)) > 0.9);
    }

    #[test]
    fn exact_match_with_unknown_duration_scores_above_point_nine() {
        let wanted = track("Blinding Lights", "The Weeknd");
        let score = score_candidate(&wanted, "Blinding Lights", "The Weeknd", None);
        assert!(score > 0.9, "{score}");
        let wanted = track_with_duration("Blinding Lights", "The Weeknd", 200_000);
        assert!(score_candidate(&wanted, "Blinding Lights", "The Weeknd", None) > 0.9);
    }

    #[test]
    fn wrong_artist_scores_clearly_lower() {
        let wanted = track_with_duration("Hello", "Adele", 295_000);
        let right = score_candidate(&wanted, "Hello", "Adele", Some(295_000));
        let wrong = score_candidate(&wanted, "Hello", "Lionel Richie", Some(295_000));
        assert!(right - wrong > 0.25, "right {right} wrong {wrong}");
        // Without a duration to back it up, a wrong artist fails a 0.6 bar.
        let wanted = track("Hello", "Adele");
        assert!(score_candidate(&wanted, "Hello", "Lionel Richie", None) < 0.6);
    }

    #[test]
    fn far_duration_scores_lower_than_close_duration() {
        let wanted = track_with_duration("Song", "Artist", 200_000);
        let close = score_candidate(&wanted, "Song", "Artist", Some(201_000));
        let far = score_candidate(&wanted, "Song", "Artist", Some(230_000));
        assert!(close > far, "close {close} far {far}");
        assert!(close > 0.9);
    }

    #[test]
    fn duration_part_steps_down_with_distance() {
        let wanted = track_with_duration("Song", "Artist", 200_000);
        let at = |ms: u64| score_candidate(&wanted, "Song", "Artist", Some(ms));
        assert_eq!(at(200_000), at(202_000));
        assert_eq!(at(200_000), at(198_000));
        assert!(at(202_000) > at(204_000));
        assert!(at(204_000) > at(205_000));
        assert!(at(205_000) > at(208_000));
        assert!(at(208_000) > at(210_000));
        assert_eq!(at(210_000), at(240_000));
        assert_eq!(at(210_000), at(1_000_000));
        // Unknown durations are neutral: between a match and a miss.
        let unknown = score_candidate(&wanted, "Song", "Artist", None);
        assert!(unknown < at(200_000) && unknown > at(240_000));
        let zero = score_candidate(&wanted, "Song", "Artist", Some(0));
        assert_eq!(zero, unknown);
    }

    #[test]
    fn unrelated_title_scores_below_half() {
        let wanted = track_with_duration("Bohemian Rhapsody", "Queen", 354_000);
        let score = score_candidate(
            &wanted,
            "Another One Bites the Dust",
            "Queen",
            Some(354_000),
        );
        assert!(score < 0.5, "{score}");
        let score = score_candidate(&wanted, "Bohemian Rhapsodi", "Queen", Some(354_000));
        assert!(score < 0.5, "{score}");
        let score = score_candidate(&wanted, "Rhapsody", "Queen", Some(354_000));
        assert!(score < 0.5, "{score}");
    }

    #[test]
    fn containing_title_needs_support_from_artist_and_duration() {
        let wanted = track_with_duration("Song", "Artist", 200_000);
        let exact = score_candidate(&wanted, "Song", "Artist", Some(200_000));
        let live_close = score_candidate(&wanted, "Song (Live)", "Artist", Some(200_500));
        let live_far = score_candidate(&wanted, "Song (Live)", "Artist", Some(260_000));
        assert!(exact > live_close);
        assert!(live_close >= 0.6, "{live_close}");
        assert!(live_far < 0.6, "{live_far}");
        assert!(live_far >= 0.5, "{live_far}");
        // Words must be whole: "Love" is not contained in "Lovely".
        let wanted = track_with_duration("Love", "Artist", 200_000);
        assert!(score_candidate(&wanted, "Lovely", "Artist", Some(200_000)) < 0.5);
    }

    #[test]
    fn kept_version_markers_affect_matching() {
        let wanted = track_with_duration("Song (Remix)", "Artist", 200_000);
        assert!(score_candidate(&wanted, "Song", "Artist", Some(200_000)) < 0.5);
        assert!(score_candidate(&wanted, "Song (Remix)", "Artist", Some(200_000)) > 0.9);
        let wanted =
            track_with_duration("All Too Well (Taylor's Version)", "Taylor Swift", 329_000);
        assert!(
            score_candidate(
                &wanted,
                "All Too Well (Taylor's Version)",
                "Taylor Swift",
                Some(329_000)
            ) > 0.9
        );
        assert!(score_candidate(&wanted, "All Too Well", "Taylor Swift", Some(329_000)) < 0.5);
    }

    #[test]
    fn primary_artist_containment_is_a_strong_match() {
        let wanted = track_with_duration("This Is What You Came For", "Calvin Harris", 222_000);
        let score = score_candidate(
            &wanted,
            "This Is What You Came For",
            "Calvin Harris feat. Rihanna",
            Some(222_000),
        );
        assert!(score > 0.9, "{score}");
        let wanted = track_with_duration(
            "This Is What You Came For",
            "Calvin Harris, Rihanna",
            222_000,
        );
        let score = score_candidate(
            &wanted,
            "This Is What You Came For",
            "Calvin Harris",
            Some(222_000),
        );
        assert!(score > 0.9, "{score}");
        let wanted = track_with_duration("Under Pressure", "David Bowie", 248_000);
        let score = score_candidate(
            &wanted,
            "Under Pressure",
            "Queen & David Bowie",
            Some(248_000),
        );
        assert!(score > 0.9, "{score}");
    }

    #[test]
    fn channel_style_artists_still_match() {
        let wanted = track_with_duration("Never Gonna Give You Up", "RickAstleyVEVO", 213_000);
        let score = score_candidate(
            &wanted,
            "Never Gonna Give You Up",
            "Rick Astley",
            Some(213_000),
        );
        assert!(score > 0.9, "{score}");
        let wanted = track_with_duration("Never Gonna Give You Up", "Rick Astley - Topic", 213_000);
        let score = score_candidate(
            &wanted,
            "Never Gonna Give You Up",
            "Rick Astley",
            Some(213_000),
        );
        assert!(score > 0.9, "{score}");
    }

    #[test]
    fn unknown_wanted_artist_is_neutral() {
        let wanted = track_with_duration("Never Gonna Give You Up", "", 213_000);
        let score = score_candidate(
            &wanted,
            "Never Gonna Give You Up",
            "Rick Astley",
            Some(213_000),
        );
        assert!(score > 0.6 && score < 0.9, "{score}");
        let youtube = track_with_duration("Never Gonna Give You Up", "YouTube", 213_000);
        assert_eq!(
            score_candidate(
                &youtube,
                "Never Gonna Give You Up",
                "Rick Astley",
                Some(213_000)
            ),
            score
        );
    }

    #[test]
    fn missing_candidate_artist_scores_artist_part_zero() {
        let wanted = track_with_duration("Song", "Artist", 200_000);
        let score = score_candidate(&wanted, "Song", "", Some(200_000));
        assert!((score - 0.65).abs() < 1e-9, "{score}");
    }

    #[test]
    fn similar_artist_spelling_scores_between() {
        let wanted = track_with_duration("Song", "Taylor Swift", 200_000);
        let right = score_candidate(&wanted, "Song", "Taylor Swift", Some(200_000));
        let typo = score_candidate(&wanted, "Song", "Taylor Swiift", Some(200_000));
        let wrong = score_candidate(&wanted, "Song", "Metallica", Some(200_000));
        assert!(right > typo && typo > wrong, "{right} {typo} {wrong}");
        // Spaces alone do not matter much.
        let spaced = score_candidate(&wanted, "Song", "TaylorSwift", Some(200_000));
        assert!(spaced > 0.95, "{spaced}");
    }

    #[test]
    fn symbol_only_titles_compare_as_written() {
        let wanted = track_with_duration("?", "Artist", 200_000);
        assert!(score_candidate(&wanted, "?", "Artist", Some(200_000)) > 0.9);
        assert!(score_candidate(&wanted, "!!!", "Artist", Some(200_000)) < 0.5);
        assert!(score_candidate(&wanted, "Song", "Artist", Some(200_000)) < 0.5);
        let empty = track("", "");
        assert!(score_candidate(&empty, "", "", None) < 0.5);
    }

    #[test]
    fn unicode_titles_score() {
        let wanted = track_with_duration("Выхода нет", "Сплин", 250_000);
        assert!(score_candidate(&wanted, "ВЫХОДА НЕТ", "Сплин", Some(250_000)) > 0.9);
        assert!(score_candidate(&wanted, "Орбит без сахара", "Сплин", Some(250_000)) < 0.5);
        let wanted = track_with_duration("夜に駆ける", "YOASOBI", 261_000);
        assert!(score_candidate(&wanted, "夜に駆ける", "YOASOBI", Some(261_000)) > 0.9);
    }

    #[test]
    fn scores_stay_in_range_for_extreme_input() {
        let wanted = Track {
            title: "x".repeat(10_000),
            artist: "y".repeat(10_000),
            album: None,
            duration_ms: Some(u64::MAX),
            spotify_id: None,
        };
        for (title, artist, duration) in [
            ("x".repeat(10_000), "y".repeat(10_000), Some(0)),
            (String::new(), String::new(), Some(u64::MAX)),
            ("(((((".to_string(), "&&&".to_string(), Some(1)),
            ("🎵".repeat(1000), "é".repeat(1000), None),
        ] {
            let score = score_candidate(&wanted, &title, &artist, duration);
            assert!((0.0..=1.0).contains(&score), "{score}");
        }
        let score = score_candidate(&Track::default(), "", "", None);
        assert!((0.0..=1.0).contains(&score));
    }

    #[test]
    fn best_candidate_wins_among_realistic_results() {
        let wanted = normalize_track(&track_with_duration(
            "Queen - Bohemian Rhapsody (Official Video Remastered)",
            "Queen Official",
            359_000,
        ));
        assert_eq!(wanted.title, "Bohemian Rhapsody");
        assert_eq!(wanted.artist, "Queen");
        let candidates = [
            ("Bohemian Rhapsody", "Queen", Some(354_000)),
            (
                "Bohemian Rhapsody - Remastered 2011",
                "Queen",
                Some(358_000),
            ),
            ("Bohemian Rhapsody (Live Aid)", "Queen", Some(370_000)),
            ("Bohemian Rhapsody", "Panic! At The Disco", Some(370_000)),
            ("Killer Queen", "Queen", Some(180_000)),
        ];
        let scores: Vec<f64> = candidates
            .iter()
            .map(|(t, a, d)| score_candidate(&wanted, t, a, *d))
            .collect();
        let best = scores
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(index, _)| index);
        assert_eq!(best, Some(1), "{scores:?}");
        assert!(scores.get(4).is_some_and(|s| *s < 0.5), "{scores:?}");
    }

    // ----- helpers -------------------------------------------------------

    #[test]
    fn levenshtein_ratio_examples() {
        assert_eq!(levenshtein_ratio("", ""), 1.0);
        assert_eq!(levenshtein_ratio("abc", "abc"), 1.0);
        assert_eq!(levenshtein_ratio("abc", ""), 0.0);
        assert!((levenshtein_ratio("kitten", "sitting") - (1.0 - 3.0 / 7.0)).abs() < 1e-9);
        assert!((levenshtein_ratio("été", "ete") - (1.0 - 2.0 / 3.0)).abs() < 1e-9);
        let long = "a".repeat(10_000);
        assert_eq!(levenshtein_ratio(&long, &long), 1.0);
    }

    #[test]
    fn token_dice_examples() {
        assert_eq!(token_dice("a b", "a b"), 1.0);
        assert_eq!(token_dice("a b", "c d"), 0.0);
        assert!((token_dice("a", "a b") - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(token_dice("", ""), 1.0);
        assert_eq!(token_dice("a", ""), 0.0);
    }

    #[test]
    fn word_contains_needs_whole_words() {
        assert!(word_contains("bohemian rhapsody live", "bohemian rhapsody"));
        assert!(word_contains("song", "song"));
        assert!(!word_contains("lovely day", "love"));
        assert!(!word_contains("song", ""));
    }

    #[test]
    fn is_noise_examples() {
        assert!(is_noise("Official Video"));
        assert!(is_noise("2011 Remaster"));
        assert!(is_noise(""));
        assert!(is_noise("!"));
        assert!(!is_noise("Remix"));
        assert!(!is_noise("Taylor's Version"));
        assert!(!is_noise("Radio"));
        assert!(!is_noise("2011"));
    }

    #[test]
    fn is_number_like_examples() {
        assert!(is_number_like("2011"));
        assert!(is_number_like("50th"));
        assert!(is_number_like("1st"));
        assert!(is_number_like("80s"));
        assert!(!is_number_like("4k"));
        assert!(!is_number_like("th"));
        assert!(!is_number_like(""));
        assert!(!is_number_like("٢٠١١"));
    }

    #[test]
    fn versions_without_singing_are_told_apart() {
        for candidate in [
            "Yellow (Instrumental)",
            "Yellow - Instrumental Version",
            "Yellow (Karaoke Version)",
            "Yellow (Inst.)",
            "Yellow [Off Vocal]",
            "Yellow (Backing Track)",
            "Yellow (Minus One)",
            "晴天 (伴奏)",
            "晴天(伴奏版)",
            "晴天 纯音乐版",
            "夜に駆ける (カラオケ)",
            "夜に駆ける -オフボーカル-",
        ] {
            assert!(is_other_version(&["Yellow"], &[candidate]), "{candidate}");
        }
        // The album counts as much as the title.
        assert!(is_other_version(
            &["Yellow", "Parachutes"],
            &["Yellow", "Parachutes (Instrumentals)"]
        ));
        for candidate in [
            "Yellow",
            "Yellow (Live)",
            "Yellow (Acoustic)",
            "Mr. Brightside",
            "Instrumentality",
            "Install",
        ] {
            assert!(!is_other_version(&["Yellow"], &[candidate]), "{candidate}");
        }
        // Wanted on purpose.
        assert!(!is_other_version(
            &["Yellow (Karaoke)"],
            &["Yellow (Instrumental)"]
        ));
        assert!(!is_other_version(
            &["Song", "Karaoke Hits"],
            &["Song (Karaoke)"]
        ));
        assert!(!is_other_version(&[], &[]));
    }

    #[test]
    fn best_scored_keeps_the_best_in_order() {
        let scored = vec![
            (0.65, "a"),
            (0.9, "b"),
            (0.7, "c"),
            (f64::NAN, "nan"),
            (0.9, "d"),
            (1.0, "e"),
        ];
        assert_eq!(
            best_scored(scored.clone(), 0.7, 10),
            vec!["e", "b", "d", "c"]
        );
        assert_eq!(best_scored(scored.clone(), 0.7, 2), vec!["e", "b"]);
        assert_eq!(best_scored(scored, 0.95, 0), Vec::<&str>::new());
        assert_eq!(
            best_scored(Vec::<(f64, u8)>::new(), 0.0, 3),
            Vec::<u8>::new()
        );
    }
}
