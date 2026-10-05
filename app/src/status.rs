//! Deciding what the status should say at a given moment. Pure functions only.

use crate::config::Config;
use crate::types::{Lyrics, Status, Track};

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
/// - `started_at_unix_ms` = `now_unix_ms - position_ms` when the position is known.
/// - `track` is the given track unchanged.
pub fn compose_status(
    config: &Config,
    track: &Track,
    lyrics: Option<&Lyrics>,
    position_ms: Option<u64>,
    playing: bool,
    offset_ms: i64,
    now_unix_ms: u64,
) -> Option<Status> {
    let _ = (
        config,
        track,
        lyrics,
        position_ms,
        playing,
        offset_ms,
        now_unix_ms,
    );
    todo!()
}
