//! Deciding what the status should say at a given moment. Pure functions only.

use crate::config::Config;
use crate::lrc::LinePosition;
use crate::template::{self, TemplateContext};
use crate::types::{Lyrics, Status, StatusKind, Track};

/// Used when both the template for the moment and `no_lyrics_template` render
/// to nothing.
const LAST_RESORT_TEMPLATE: &str = "{title} · {artist}";

/// Composes what every target should show.
///
/// Returns `None` (meaning "clear the status") when:
/// - `playing` is false and `config.status.show_when_paused` is false,
/// - the cleaned artist is in `config.privacy.blocked_artists`.
///
/// Otherwise:
/// - `config.privacy.title_only`, or `lyrics` is `None`, or the lyrics are
///   instrumental, or they have no text, or `position_ms` is `None`:
///   `no_lyrics_template` (`StatusKind::NoLyrics`). An instrumental song uses
///   `StatusKind::Instrumental` with `no_lyrics_template`.
/// - Otherwise the position, shifted by `-offset_ms` (positive offset shows lines
///   later), is looked up with [`Lyrics::at`]:
///   intro or break → `instrumental_text` (`StatusKind::Instrumental`);
///   a line → `line_template` with `{line}` and `{next}` (`StatusKind::Line`,
///   `line = Some(text)`).
/// - `estimated` is true when the lyrics are not synced.
/// - The profanity filter, when on, is applied to the rendered text and to `line`
///   (using `profanity_words`, or the built-in list when empty).
/// - If the rendered text is empty, fall back to `no_lyrics_template`, and if
///   that is empty too, to `"{title} · {artist}"`.
/// - `started_at_unix_ms` = `now_unix_ms - position_ms` when the position is
///   known and `playing` is true (`None` while paused, see below).
/// - `track` is the given track unchanged.
///
/// Details:
/// - Blocked artists are compared case-insensitively, with whitespace
///   collapsed, against [`crate::matcher::clean_artist`] of the track's artist
///   and against the artist [`crate::matcher::normalize_track`] finds (so a
///   `Artist - Title` video from a channel is blocked too). Blank entries are
///   ignored.
/// - A position before the start of the song once shifted (a positive offset
///   larger than the position) is the intro.
/// - `instrumental_text` is rendered like a template too (so `♪ {title}` or
///   `♪ {next}` work); during the intro `{next}` is the first line.
/// - `estimated` is only set when the lyric timing is used (a line, the intro
///   or a break); statuses from `no_lyrics_template` have `estimated = false`.
/// - A fallback keeps the kind and `line` of the moment; only the text changes.
/// - A `profanity_words` list made only of blank entries counts as empty.
/// - `started_at_unix_ms` uses the reported position (not shifted by the
///   offset), so progress bars follow the player; it saturates at 0.
/// - `started_at_unix_ms` is `None` while not playing (paused with
///   `show_when_paused`): a paused song has no moving start time. Otherwise
///   `now - position` would creep forward with the wall clock, making the
///   same paused line a "new" status every 2 s (resent to every target) and
///   keeping progress bars running.
/// - Unsynced lyrics that were never given timings (every line starts at 0,
///   because the duration was unknown) say nothing about which line is sung,
///   so they are treated like no lyrics (`no_lyrics_template`,
///   `StatusKind::NoLyrics`) instead of showing the last line all song long.
pub fn compose_status(
    config: &Config,
    track: &Track,
    lyrics: Option<&Lyrics>,
    position_ms: Option<u64>,
    playing: bool,
    offset_ms: i64,
    now_unix_ms: u64,
) -> Option<Status> {
    let settings = &config.status;
    if !playing && !settings.show_when_paused {
        return None;
    }
    if artist_is_blocked(&config.privacy.blocked_artists, track) {
        return None;
    }

    let usable = if config.privacy.title_only {
        None
    } else {
        lyrics.filter(|l| !l.instrumental && l.has_text() && has_timing(l))
    };

    let moment = match (usable, position_ms) {
        (Some(lyrics), Some(position)) => {
            let (at, next) = lookup(lyrics, position, offset_ms);
            match at {
                LinePosition::Line { text, .. } => Moment {
                    kind: StatusKind::Line,
                    template: &settings.line_template,
                    line: Some(text),
                    next,
                    estimated: !lyrics.synced,
                },
                LinePosition::Intro | LinePosition::Break => Moment {
                    kind: StatusKind::Instrumental,
                    template: &settings.instrumental_text,
                    line: None,
                    next,
                    estimated: !lyrics.synced,
                },
            }
        }
        _ => Moment {
            kind: if lyrics.is_some_and(|l| l.instrumental) {
                StatusKind::Instrumental
            } else {
                StatusKind::NoLyrics
            },
            template: &settings.no_lyrics_template,
            line: None,
            next: None,
            estimated: false,
        },
    };

    let ctx = TemplateContext {
        line: moment.line,
        next: moment.next,
        title: &track.title,
        artist: &track.artist,
        album: track.album.as_deref(),
    };
    let mut text = template::render(moment.template, &ctx);
    if text.is_empty() {
        text = template::render(&settings.no_lyrics_template, &ctx);
    }
    if text.is_empty() {
        text = template::render(LAST_RESORT_TEMPLATE, &ctx);
    }
    let mut line = moment.line.map(str::to_string);

    if settings.profanity_filter {
        let has_own_words = settings
            .profanity_words
            .iter()
            .any(|w| !w.trim().is_empty());
        let built_in;
        let words: &[String] = if has_own_words {
            &settings.profanity_words
        } else {
            built_in = template::default_profanity_words();
            &built_in
        };
        text = template::filter_profanity(&text, words);
        line = line.map(|l| template::filter_profanity(&l, words));
    }

    Some(Status {
        text,
        kind: moment.kind,
        line,
        track: track.clone(),
        started_at_unix_ms: position_ms
            .filter(|_| playing)
            .map(|p| now_unix_ms.saturating_sub(p)),
        estimated: moment.estimated,
    })
}

/// What the status describes, before rendering.
struct Moment<'a> {
    kind: StatusKind,
    template: &'a str,
    line: Option<&'a str>,
    next: Option<&'a str>,
    estimated: bool,
}

/// Where `position_ms - offset_ms` falls in the lyrics, and the next line's text.
/// A shifted position before 0 is the intro, with the first line coming next.
fn lookup(lyrics: &Lyrics, position_ms: u64, offset_ms: i64) -> (LinePosition<'_>, Option<&str>) {
    let shifted = i128::from(position_ms) - i128::from(offset_ms);
    if shifted < 0 {
        let first = lyrics
            .lines
            .iter()
            .map(|l| l.text.trim())
            .find(|t| !t.is_empty());
        return (LinePosition::Intro, first);
    }
    let at = u64::try_from(shifted).unwrap_or(u64::MAX);
    (lyrics.at(at), lyrics.next_text(at))
}

/// False for unsynced lyrics whose lines all still start at 0 (never spread
/// over the song), whose timing cannot place any line.
fn has_timing(lyrics: &Lyrics) -> bool {
    lyrics.synced || lyrics.lines.iter().any(|l| l.start_ms > 0)
}

/// True when the track's artist is on the blocked list. See [`compose_status`].
fn artist_is_blocked(blocked: &[String], track: &Track) -> bool {
    let mut entries = blocked
        .iter()
        .map(|b| fold(b))
        .filter(|b| !b.is_empty())
        .peekable();
    if entries.peek().is_none() {
        return false;
    }
    let cleaned = fold(&crate::matcher::clean_artist(&track.artist));
    let normalized = fold(&crate::matcher::normalize_track(track).artist);
    entries.any(|b| {
        (!cleaned.is_empty() && b == cleaned) || (!normalized.is_empty() && b == normalized)
    })
}

/// Lowercase with whitespace runs collapsed and trimmed.
fn fold(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LyricLine;

    const NOW: u64 = 1_700_000_000_000;

    fn track() -> Track {
        Track {
            title: "Song".into(),
            artist: "Artist".into(),
            album: Some("Album".into()),
            duration_ms: Some(180_000),
            spotify_id: None,
        }
    }

    fn line(start_ms: u64, text: &str) -> LyricLine {
        LyricLine {
            start_ms,
            text: text.into(),
        }
    }

    /// 0–5 s intro, "First line" at 5 s, "Second line" at 10 s, a break at
    /// 15 s and "Third line" at 20 s.
    fn synced() -> Lyrics {
        Lyrics {
            lines: vec![
                line(5_000, "First line"),
                line(10_000, "Second line"),
                line(15_000, ""),
                line(20_000, "Third line"),
            ],
            synced: true,
            instrumental: false,
            source: "test".into(),
        }
    }

    fn unsynced() -> Lyrics {
        Lyrics {
            synced: false,
            ..synced()
        }
    }

    fn instrumental() -> Lyrics {
        Lyrics {
            lines: Vec::new(),
            synced: false,
            instrumental: true,
            source: "test".into(),
        }
    }

    fn compose(
        config: &Config,
        lyrics: Option<&Lyrics>,
        position: Option<u64>,
        offset: i64,
    ) -> Option<Status> {
        compose_status(config, &track(), lyrics, position, true, offset, NOW)
    }

    fn summary(status: &Status) -> (StatusKind, &str, Option<&str>) {
        (status.kind, status.text.as_str(), status.line.as_deref())
    }

    // ---- line lookup ----

    #[test]
    fn position_picks_the_line_intro_or_break() {
        use StatusKind::*;
        let config = Config::default();
        let lyrics = synced();
        let cases: &[(u64, StatusKind, &str, Option<&str>)] = &[
            (0, Instrumental, "♪", None),
            (4_999, Instrumental, "♪", None),
            (5_000, Line, "🎵 First line", Some("First line")),
            (9_999, Line, "🎵 First line", Some("First line")),
            (10_000, Line, "🎵 Second line", Some("Second line")),
            (15_000, Instrumental, "♪", None),
            (19_999, Instrumental, "♪", None),
            (20_000, Line, "🎵 Third line", Some("Third line")),
            (u64::MAX, Line, "🎵 Third line", Some("Third line")),
        ];
        for &(position, kind, text, line) in cases {
            let status = compose(&config, Some(&lyrics), Some(position), 0).unwrap();
            assert_eq!(summary(&status), (kind, text, line), "position {position}");
            assert!(!status.estimated, "synced lyrics are not estimated");
        }
    }

    #[test]
    fn offset_shifts_the_lookup() {
        use StatusKind::*;
        let config = Config::default();
        let lyrics = synced();
        let cases: &[(u64, i64, StatusKind, Option<&str>)] = &[
            // Positive offset: lines show later.
            (5_000, 1_000, Instrumental, None),
            (5_999, 1_000, Instrumental, None),
            (6_000, 1_000, Line, Some("First line")),
            (11_000, 1_000, Line, Some("Second line")),
            // Negative offset: lines show earlier.
            (3_999, -1_000, Instrumental, None),
            (4_000, -1_000, Line, Some("First line")),
            (9_000, -1_000, Line, Some("Second line")),
            (14_000, -1_000, Instrumental, None),
            // An offset larger than the position is the intro.
            (0, 1, Instrumental, None),
            (60_000, i64::MAX, Instrumental, None),
            // Extreme values never overflow.
            (0, i64::MIN, Line, Some("Third line")),
            (u64::MAX, i64::MIN, Line, Some("Third line")),
            (u64::MAX, i64::MAX, Line, Some("Third line")),
        ];
        for &(position, offset, kind, line) in cases {
            let status = compose(&config, Some(&lyrics), Some(position), offset).unwrap();
            assert_eq!(
                (status.kind, status.line.as_deref()),
                (kind, line),
                "position {position}, offset {offset}"
            );
        }
    }

    #[test]
    fn next_line_skips_breaks_and_the_intro_shows_the_first_line_next() {
        let mut config = Config::default();
        config.status.line_template = "{line} / next: {next}".into();
        config.status.instrumental_text = "♪ next: {next}".into();
        let lyrics = synced();
        let cases: &[(u64, i64, &str)] = &[
            (0, 0, "♪ next: First line"),
            (5_000, 0, "First line / next: Second line"),
            (12_000, 0, "Second line / next: Third line"),
            (16_000, 0, "♪ next: Third line"),
            // No next line: the separator goes with the empty value.
            (25_000, 0, "Third line / next:"),
            // Shifted before 0.
            (500, 1_000, "♪ next: First line"),
        ];
        for &(position, offset, text) in cases {
            let status = compose(&config, Some(&lyrics), Some(position), offset).unwrap();
            assert_eq!(status.text, text, "position {position}, offset {offset}");
        }
    }

    #[test]
    fn templates_get_title_artist_and_album() {
        let mut config = Config::default();
        config.status.line_template = "{line} ({title} by {artist} on {album})".into();
        let status = compose(&config, Some(&synced()), Some(5_000), 0).unwrap();
        assert_eq!(status.text, "First line (Song by Artist on Album)");
    }

    // ---- no-lyrics cases ----

    #[test]
    fn songs_without_usable_lyrics_show_the_no_lyrics_template() {
        let config = Config::default();
        let no_text = Lyrics {
            lines: vec![line(0, ""), line(1_000, "  ")],
            synced: true,
            instrumental: false,
            source: "test".into(),
        };
        let (good, empty) = (synced(), Lyrics::default());
        let cases: Vec<(&str, Option<&Lyrics>, Option<u64>)> = vec![
            ("no lyrics", None, Some(5_000)),
            ("no position", Some(&good), None),
            ("lyrics without text", Some(&no_text), Some(5_000)),
            ("empty lyrics", Some(&empty), Some(5_000)),
        ];
        for (name, lyrics, position) in cases {
            let status = compose(&config, lyrics, position, 0).unwrap();
            assert_eq!(
                summary(&status),
                (StatusKind::NoLyrics, "Song · Artist", None),
                "{name}"
            );
            assert!(!status.estimated, "{name}");
        }
    }

    #[test]
    fn title_only_never_shows_lines() {
        let mut config = Config::default();
        config.privacy.title_only = true;
        for lyrics in [synced(), unsynced()] {
            for position in [0, 5_000, 20_000] {
                let status = compose(&config, Some(&lyrics), Some(position), 0).unwrap();
                assert_eq!(
                    summary(&status),
                    (StatusKind::NoLyrics, "Song · Artist", None)
                );
                assert!(!status.estimated);
            }
        }
    }

    #[test]
    fn instrumental_songs_use_the_no_lyrics_template_with_the_instrumental_kind() {
        let mut config = Config::default();
        let lyrics = instrumental();
        for position in [None, Some(0), Some(60_000)] {
            let status = compose(&config, Some(&lyrics), position, 0).unwrap();
            assert_eq!(
                summary(&status),
                (StatusKind::Instrumental, "Song · Artist", None)
            );
            assert!(!status.estimated);
        }
        // Even an instrumental flag on lyrics that have lines.
        let mut flagged = synced();
        flagged.instrumental = true;
        let status = compose(&config, Some(&flagged), Some(5_000), 0).unwrap();
        assert_eq!(status.kind, StatusKind::Instrumental);
        assert_eq!(status.line, None);
        // Title-only mode keeps the kind.
        config.privacy.title_only = true;
        let status = compose(&config, Some(&lyrics), Some(0), 0).unwrap();
        assert_eq!(status.kind, StatusKind::Instrumental);
    }

    // ---- estimated timing ----

    #[test]
    fn unsynced_lyrics_are_estimated() {
        let config = Config::default();
        let lyrics = unsynced();
        let status = compose(&config, Some(&lyrics), Some(5_000), 0).unwrap();
        assert_eq!(status.kind, StatusKind::Line);
        assert!(status.estimated);
        let intro = compose(&config, Some(&lyrics), Some(0), 0).unwrap();
        assert_eq!(intro.kind, StatusKind::Instrumental);
        assert!(intro.estimated);
        let unknown_position = compose(&config, Some(&lyrics), None, 0).unwrap();
        assert!(!unknown_position.estimated);
    }

    // ---- clearing ----

    #[test]
    fn paused_clears_unless_show_when_paused() {
        let mut config = Config::default();
        let lyrics = synced();
        assert_eq!(
            compose_status(&config, &track(), Some(&lyrics), Some(5_000), false, 0, NOW),
            None
        );
        assert_eq!(
            compose_status(&config, &track(), None, None, false, 0, NOW),
            None
        );
        config.status.show_when_paused = true;
        let status =
            compose_status(&config, &track(), Some(&lyrics), Some(5_000), false, 0, NOW).unwrap();
        assert_eq!(status.line.as_deref(), Some("First line"));
        assert_eq!(
            status.started_at_unix_ms, None,
            "no start time while paused"
        );
    }

    #[test]
    fn blocked_artists_are_cleared() {
        let lyrics = synced();
        let cases: &[(&str, &[&str], bool)] = &[
            ("Artist", &["Artist"], true),
            ("Artist", &["artist"], true),
            ("ARTIST", &["  artist  "], true),
            ("ArtistVEVO", &["Artist"], true),
            ("Artist - Topic", &["artist"], true),
            ("Big  Band", &["big band"], true),
            ("Artist", &["Other", "Artist"], true),
            ("Artist", &["Art"], false),
            ("Artist", &["Artist Band"], false),
            ("Artist feat. Someone", &["Artist"], false),
            ("Artist", &[], false),
            ("Artist", &["", "   "], false),
            ("", &["", "  "], false),
        ];
        for &(artist, blocked, expect_blocked) in cases {
            let mut config = Config::default();
            config.privacy.blocked_artists = blocked.iter().map(|s| s.to_string()).collect();
            let t = Track {
                artist: artist.into(),
                ..track()
            };
            let status = compose_status(&config, &t, Some(&lyrics), Some(5_000), true, 0, NOW);
            assert_eq!(
                status.is_none(),
                expect_blocked,
                "artist {artist:?}, blocked {blocked:?}"
            );
        }
    }

    #[test]
    fn blocked_artist_from_a_video_title_is_cleared() {
        let mut config = Config::default();
        config.privacy.blocked_artists = vec!["Rick Astley".into()];
        let video = Track {
            title: "Rick Astley - Never Gonna Give You Up (Official Video)".into(),
            artist: "RickAstleyVEVO".into(),
            ..Track::default()
        };
        assert_eq!(
            compose_status(&config, &video, None, Some(0), true, 0, NOW),
            None
        );
    }

    // ---- profanity ----

    #[test]
    fn profanity_filter_masks_text_and_line() {
        let mut config = Config::default();
        config.status.profanity_filter = true;
        let lyrics = Lyrics {
            lines: vec![line(1_000, "What the fuck is this")],
            synced: true,
            instrumental: false,
            source: "test".into(),
        };
        let status = compose(&config, Some(&lyrics), Some(1_000), 0).unwrap();
        assert_eq!(status.text, "🎵 What the f*** is this");
        assert_eq!(status.line.as_deref(), Some("What the f*** is this"));

        // Off: untouched.
        config.status.profanity_filter = false;
        let status = compose(&config, Some(&lyrics), Some(1_000), 0).unwrap();
        assert_eq!(status.text, "🎵 What the fuck is this");
        assert_eq!(status.line.as_deref(), Some("What the fuck is this"));
    }

    #[test]
    fn profanity_filter_uses_the_configured_words() {
        let mut config = Config::default();
        config.status.profanity_filter = true;
        config.status.profanity_words = vec!["darn".into()];
        let lyrics = Lyrics {
            lines: vec![line(0, "Darn it, fuck")],
            synced: true,
            instrumental: false,
            source: "test".into(),
        };
        let status = compose(&config, Some(&lyrics), Some(0), 0).unwrap();
        assert_eq!(status.line.as_deref(), Some("D*** it, fuck"));

        // A list of blank entries counts as empty: the built-in list is used.
        config.status.profanity_words = vec!["  ".into(), String::new()];
        let status = compose(&config, Some(&lyrics), Some(0), 0).unwrap();
        assert_eq!(status.line.as_deref(), Some("Darn it, f***"));
    }

    #[test]
    fn profanity_filter_applies_to_titles_too() {
        let mut config = Config::default();
        config.status.profanity_filter = true;
        let t = Track {
            title: "Fuck You".into(),
            ..track()
        };
        let status = compose_status(&config, &t, None, Some(0), true, 0, NOW).unwrap();
        assert_eq!(status.text, "F*** You · Artist");
        assert_eq!(status.track.title, "Fuck You", "the track is unchanged");
    }

    // ---- fallbacks ----

    #[test]
    fn empty_renders_fall_back_to_the_no_lyrics_template_then_title_and_artist() {
        let lyrics = synced();
        let cases: &[(&str, &str, &str, u64, StatusKind, &str)] = &[
            // (line_template, instrumental_text, no_lyrics_template, position, kind, text)
            (
                "",
                "♪",
                "{title} · {artist}",
                5_000,
                StatusKind::Line,
                "Song · Artist",
            ),
            (
                "  ",
                "♪",
                "Now: {title}",
                5_000,
                StatusKind::Line,
                "Now: Song",
            ),
            ("", "♪", "", 5_000, StatusKind::Line, "Song · Artist"),
            ("{next}", "♪", "{title}", 25_000, StatusKind::Line, "Song"),
            (
                "🎵 {line}",
                "",
                "{title}",
                0,
                StatusKind::Instrumental,
                "Song",
            ),
            (
                "🎵 {line}",
                "",
                "",
                0,
                StatusKind::Instrumental,
                "Song · Artist",
            ),
        ];
        for &(line_t, inst_t, none_t, position, kind, text) in cases {
            let mut config = Config::default();
            config.status.line_template = line_t.into();
            config.status.instrumental_text = inst_t.into();
            config.status.no_lyrics_template = none_t.into();
            let status = compose(&config, Some(&lyrics), Some(position), 0).unwrap();
            assert_eq!(
                (status.kind, status.text.as_str()),
                (kind, text),
                "templates {line_t:?} / {inst_t:?} / {none_t:?}"
            );
        }
        // The line is kept even when the text fell back.
        let mut config = Config::default();
        config.status.line_template = String::new();
        let status = compose(&config, Some(&lyrics), Some(5_000), 0).unwrap();
        assert_eq!(status.line.as_deref(), Some("First line"));
    }

    #[test]
    fn no_lyrics_template_falls_back_too() {
        let mut config = Config::default();
        config.status.no_lyrics_template = "{album}".into();
        let t = Track {
            album: None,
            ..track()
        };
        let status = compose_status(&config, &t, None, Some(0), true, 0, NOW).unwrap();
        assert_eq!(status.text, "Song · Artist");

        // Just the title when there is no artist.
        let t = Track {
            artist: String::new(),
            album: None,
            ..track()
        };
        let status = compose_status(&config, &t, None, Some(0), true, 0, NOW).unwrap();
        assert_eq!(status.text, "Song");
    }

    #[test]
    fn nothing_to_show_at_all_gives_empty_text_without_panicking() {
        let mut config = Config::default();
        config.status.no_lyrics_template = String::new();
        let status = compose_status(&config, &Track::default(), None, None, true, 0, NOW).unwrap();
        assert_eq!(status.text, "");
        assert_eq!(status.kind, StatusKind::NoLyrics);
    }

    // ---- other fields ----

    #[test]
    fn started_at_is_now_minus_position() {
        let config = Config::default();
        let cases: &[(Option<u64>, u64, Option<u64>)] = &[
            (Some(250_000), 1_000_000, Some(750_000)),
            (Some(0), 1_000_000, Some(1_000_000)),
            (Some(2_000), 1_000, Some(0)),
            (None, 1_000_000, None),
        ];
        for &(position, now, expected) in cases {
            let status =
                compose_status(&config, &track(), Some(&synced()), position, true, 0, now).unwrap();
            assert_eq!(status.started_at_unix_ms, expected, "position {position:?}");
        }
        // The offset does not move the song start.
        let status = compose_status(
            &config,
            &track(),
            Some(&synced()),
            Some(5_000),
            true,
            3_000,
            NOW,
        )
        .unwrap();
        assert_eq!(status.started_at_unix_ms, Some(NOW - 5_000));
    }

    #[test]
    fn the_track_is_passed_through_unchanged() {
        let config = Config::default();
        let t = Track {
            title: "Song (Remastered 2011)".into(),
            artist: "ArtistVEVO".into(),
            album: None,
            duration_ms: None,
            spotify_id: Some("4uLU6hMCjMI75M1A2tKUQC".into()),
        };
        let status =
            compose_status(&config, &t, Some(&synced()), Some(5_000), true, 0, NOW).unwrap();
        assert_eq!(status.track, t);
        let status = compose_status(&config, &t, None, None, true, 0, NOW).unwrap();
        assert_eq!(status.track, t);
        assert_eq!(status.text, "Song (Remastered 2011) · ArtistVEVO");
    }

    #[test]
    fn lines_are_trimmed() {
        let config = Config::default();
        let lyrics = Lyrics {
            lines: vec![line(0, "   padded line  ")],
            synced: true,
            instrumental: false,
            source: "test".into(),
        };
        let status = compose(&config, Some(&lyrics), Some(0), 0).unwrap();
        assert_eq!(status.line.as_deref(), Some("padded line"));
        assert_eq!(status.text, "🎵 padded line");
    }

    #[test]
    fn a_paused_status_has_no_start_time_and_does_not_change_while_paused() {
        let mut config = Config::default();
        config.status.show_when_paused = true;
        let lyrics = synced();
        let at = |now: u64| {
            compose_status(&config, &track(), Some(&lyrics), Some(5_000), false, 0, now).unwrap()
        };
        let paused = at(NOW);
        assert_eq!(paused.started_at_unix_ms, None);
        assert_eq!(paused.line.as_deref(), Some("First line"));
        // Minutes later the paused song looks exactly the same, so no target
        // is sent the same line again and again.
        assert_eq!(at(NOW + 10 * 60_000), paused);
        // Playing again: the start time is back.
        let playing =
            compose_status(&config, &track(), Some(&lyrics), Some(5_000), true, 0, NOW).unwrap();
        assert_eq!(playing.started_at_unix_ms, Some(NOW - 5_000));
    }

    #[test]
    fn unsynced_lyrics_without_timings_show_the_song() {
        // Plain lyrics that could not be spread (the duration is unknown):
        // every line starts at 0, so the timing says nothing about which
        // line is sung. Showing the last line all song long would be wrong.
        let config = Config::default();
        let plain = crate::lrc::from_plain("First\nSecond\nLast");
        for position in [0, 5_000, 200_000] {
            let status = compose(&config, Some(&plain), Some(position), 0).unwrap();
            assert_eq!(
                summary(&status),
                (StatusKind::NoLyrics, "Song · Artist", None),
                "position {position}"
            );
            assert!(!status.estimated);
        }
        // Once spread over the song they are used, marked as estimated.
        let mut spread = plain.clone();
        spread.spread_evenly(180_000);
        let status = compose(&config, Some(&spread), Some(9_000), 0).unwrap();
        assert_eq!(status.line.as_deref(), Some("First"));
        assert!(status.estimated);
        // Synced lyrics whose lines all start at 0 are still used as timed.
        let synced_at_zero = Lyrics {
            synced: true,
            ..plain
        };
        let status = compose(&config, Some(&synced_at_zero), Some(0), 0).unwrap();
        assert_eq!(status.line.as_deref(), Some("Last"));
    }

    #[test]
    fn instrumental_text_is_a_template_and_falls_back_when_empty() {
        let mut config = Config::default();
        config.status.instrumental_text = "♪ {title} by {artist}".into();
        let status = compose(&config, Some(&synced()), Some(16_000), 0).unwrap();
        assert_eq!(status.kind, StatusKind::Instrumental);
        assert_eq!(status.text, "♪ Song by Artist");
        assert_eq!(status.line, None);
        assert!(!status.estimated);
    }

    #[test]
    fn blocked_artists_are_cleared_even_when_paused_or_title_only() {
        let mut config = Config::default();
        config.privacy.blocked_artists = vec!["artist".into()];
        config.privacy.title_only = true;
        config.status.show_when_paused = true;
        for playing in [true, false] {
            assert_eq!(
                compose_status(&config, &track(), None, Some(0), playing, 0, NOW),
                None
            );
        }
    }

    #[test]
    fn fold_lowercases_and_collapses_whitespace() {
        assert_eq!(fold("  Big   Band\tX "), "big band x");
        assert_eq!(fold("ÉMILIE"), "émilie");
        assert_eq!(fold("   "), "");
    }
}
