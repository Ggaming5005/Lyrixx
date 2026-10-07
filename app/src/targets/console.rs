//! Prints each status change to the terminal.

use super::{StatusTarget, TargetError};
use crate::types::{Status, Track};
use async_trait::async_trait;
use std::io::Write;
use std::time::Duration;

/// Writes one line per update to the given writer (stdout by default):
/// `♫ <text>` for a status, with ` (estimated timing)` appended when
/// `status.estimated`, and `■ cleared` when cleared. A new song first prints a
/// header line `▶ <title> — <artist>` (just `▶ <title>` when the artist is
/// empty), once per track change (a different title, artist, album or
/// duration, as for [`crate::clock::ClockEvent::TrackChanged`]; clearing does
/// not forget the track). Control characters (line breaks, tabs, terminal
/// escape codes) in the text, title and artist are printed as spaces, so every
/// update stays on one line and cannot restyle the terminal. Write errors are
/// returned as [`TargetError::Other`].
pub struct ConsoleTarget<W: Write + Send = std::io::Stdout> {
    out: W,
    /// The track whose header was printed last, so the header is printed once
    /// per track change.
    last_track: Option<Track>,
}

impl ConsoleTarget<std::io::Stdout> {
    pub fn stdout() -> Self {
        Self::new(std::io::stdout())
    }
}

impl<W: Write + Send> ConsoleTarget<W> {
    pub fn new(out: W) -> Self {
        Self {
            out,
            last_track: None,
        }
    }

    /// The writer, for tests.
    pub fn into_inner(self) -> W {
        self.out
    }

    /// Writes `line` plus a newline and flushes, mapping IO errors.
    fn write_line(&mut self, line: &str) -> Result<(), TargetError> {
        let result = writeln!(self.out, "{line}").and_then(|()| self.out.flush());
        result.map_err(|e| {
            TargetError::Other(anyhow::Error::new(e).context("could not write to the terminal"))
        })
    }
}

/// Replaces line breaks and other control characters with spaces, so one
/// update is always one line.
fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Whether `a` and `b` are the same song, compared like
/// [`crate::clock::ClockEvent::TrackChanged`] does: title, artist, album and
/// duration (a Spotify id appearing mid-song is not a new song).
fn same_track(a: &Track, b: &Track) -> bool {
    a.title == b.title
        && a.artist == b.artist
        && a.album == b.album
        && a.duration_ms == b.duration_ms
}

/// `▶ <title> — <artist>`, or `▶ <title>` without an artist.
fn header_line(track: &Track) -> String {
    let title = one_line(&track.title);
    let title = title.trim();
    let artist = one_line(&track.artist);
    let artist = artist.trim();
    if artist.is_empty() {
        format!("▶ {title}")
    } else {
        format!("▶ {title} — {artist}")
    }
}

/// `♫ <text>`, plus ` (estimated timing)` when the timing is estimated.
fn status_line(status: &Status) -> String {
    let mut line = format!("♫ {}", one_line(&status.text));
    if status.estimated {
        line.push_str(" (estimated timing)");
    }
    line
}

#[async_trait]
impl<W: Write + Send> StatusTarget for ConsoleTarget<W> {
    fn name(&self) -> &'static str {
        "console"
    }

    fn min_interval(&self) -> Duration {
        Duration::ZERO
    }

    async fn set(&mut self, status: &Status) -> Result<(), TargetError> {
        let same = self
            .last_track
            .as_ref()
            .is_some_and(|last| same_track(last, &status.track));
        if !same {
            self.write_line(&header_line(&status.track))?;
            // Remember the track only once its header was written, so a failed
            // write prints the header again next time.
            self.last_track = Some(status.track.clone());
        }
        self.write_line(&status_line(status))
    }

    async fn clear(&mut self) -> Result<(), TargetError> {
        self.write_line("■ cleared")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::StatusKind;

    fn track(title: &str, artist: &str) -> Track {
        Track {
            title: title.into(),
            artist: artist.into(),
            album: Some("Album".into()),
            duration_ms: Some(200_000),
            spotify_id: None,
        }
    }

    fn status(text: &str, track: Track) -> Status {
        Status {
            text: text.into(),
            kind: StatusKind::Line,
            line: Some(text.into()),
            track,
            started_at_unix_ms: Some(1_700_000_000_000),
            estimated: false,
        }
    }

    fn output(target: ConsoleTarget<Vec<u8>>) -> String {
        String::from_utf8(target.into_inner()).expect("console output is UTF-8")
    }

    /// A writer that fails every write or flush.
    struct FailingWriter {
        fail_on_flush_only: bool,
        written: Vec<u8>,
    }

    impl Write for FailingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.fail_on_flush_only {
                self.written.extend_from_slice(buf);
                Ok(buf.len())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "pipe closed",
                ))
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            if self.fail_on_flush_only {
                Err(std::io::Error::other("flush failed"))
            } else {
                Ok(())
            }
        }
    }

    /// A writer that fails until `allow` is set.
    struct ToggleWriter {
        allow: bool,
        written: Vec<u8>,
    }

    impl Write for ToggleWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.allow {
                self.written.extend_from_slice(buf);
                Ok(buf.len())
            } else {
                Err(std::io::Error::other("not yet"))
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn name_and_interval() {
        let target = ConsoleTarget::new(Vec::new());
        assert_eq!(target.name(), "console");
        assert_eq!(target.min_interval(), Duration::ZERO);
        let stdout = ConsoleTarget::stdout();
        assert_eq!(stdout.name(), "console");
    }

    #[tokio::test]
    async fn first_status_prints_header_then_line() {
        let mut target = ConsoleTarget::new(Vec::new());
        let s = status(
            "Never gonna give you up",
            track("Never Gonna Give You Up", "Rick Astley"),
        );
        target.set(&s).await.unwrap();
        assert_eq!(
            output(target),
            "▶ Never Gonna Give You Up — Rick Astley\n♫ Never gonna give you up\n"
        );
    }

    #[tokio::test]
    async fn header_is_printed_once_per_track() {
        let mut target = ConsoleTarget::new(Vec::new());
        let t = track("Song", "Artist");
        target.set(&status("line one", t.clone())).await.unwrap();
        target.set(&status("line two", t.clone())).await.unwrap();
        target.set(&status("line two", t)).await.unwrap();
        assert_eq!(
            output(target),
            "▶ Song — Artist\n♫ line one\n♫ line two\n♫ line two\n"
        );
    }

    #[tokio::test]
    async fn track_change_prints_a_new_header() {
        let mut target = ConsoleTarget::new(Vec::new());
        let a = track("First", "A");
        let b = track("Second", "B");
        target.set(&status("a1", a.clone())).await.unwrap();
        target.set(&status("b1", b)).await.unwrap();
        target.set(&status("a2", a)).await.unwrap();
        assert_eq!(
            output(target),
            "▶ First — A\n♫ a1\n▶ Second — B\n♫ b1\n▶ First — A\n♫ a2\n"
        );
    }

    #[tokio::test]
    async fn any_track_field_change_counts_as_a_new_track() {
        let mut target = ConsoleTarget::new(Vec::new());
        let a = track("Same", "Same");
        let mut b = a.clone();
        b.album = Some("Deluxe".into());
        target.set(&status("x", a)).await.unwrap();
        target.set(&status("y", b)).await.unwrap();
        assert_eq!(output(target), "▶ Same — Same\n♫ x\n▶ Same — Same\n♫ y\n");
    }

    /// A track change is what the clock calls one: title, artist, album or
    /// duration. A Spotify id showing up mid-song is the same song.
    #[tokio::test]
    async fn spotify_id_alone_is_not_a_track_change() {
        let mut target = ConsoleTarget::new(Vec::new());
        let a = track("Same", "Same");
        let mut b = a.clone();
        b.spotify_id = Some("4cOdK2wGLETKBW3PvgPWqT".into());
        let mut c = b.clone();
        c.duration_ms = Some(201_000);
        target.set(&status("x", a)).await.unwrap();
        target.set(&status("y", b)).await.unwrap();
        target.set(&status("z", c)).await.unwrap();
        assert_eq!(
            output(target),
            "▶ Same — Same\n♫ x\n♫ y\n▶ Same — Same\n♫ z\n"
        );
    }

    #[tokio::test]
    async fn terminal_escape_codes_are_neutralized() {
        let mut target = ConsoleTarget::new(Vec::new());
        let s = status(
            "\u{1b}[2J\u{1b}[31mred\u{7}\u{0}",
            track("T\u{1b}]0;title\u{7}", "A\u{85}B"),
        );
        target.set(&s).await.unwrap();
        let out = output(target);
        assert!(!out.chars().any(|c| c.is_control() && c != '\n'), "{out:?}");
        assert_eq!(out, "▶ T ]0;title — A B\n♫  [2J [31mred  \n");
    }

    #[tokio::test]
    async fn estimated_timing_is_marked() {
        let mut target = ConsoleTarget::new(Vec::new());
        let mut s = status("a plain line", track("T", "A"));
        s.estimated = true;
        target.set(&s).await.unwrap();
        assert_eq!(
            output(target),
            "▶ T — A\n♫ a plain line (estimated timing)\n"
        );
    }

    #[tokio::test]
    async fn clear_prints_cleared() {
        let mut target = ConsoleTarget::new(Vec::new());
        target.clear().await.unwrap();
        assert_eq!(output(target), "■ cleared\n");
    }

    #[tokio::test]
    async fn clear_does_not_repeat_the_header_for_the_same_track() {
        let mut target = ConsoleTarget::new(Vec::new());
        let t = track("Song", "Artist");
        target.set(&status("one", t.clone())).await.unwrap();
        target.clear().await.unwrap();
        target.set(&status("two", t)).await.unwrap();
        assert_eq!(output(target), "▶ Song — Artist\n♫ one\n■ cleared\n♫ two\n");
    }

    #[tokio::test]
    async fn empty_artist_prints_only_the_title() {
        let mut target = ConsoleTarget::new(Vec::new());
        target
            .set(&status("hi", track("Lonely Title", "")))
            .await
            .unwrap();
        assert_eq!(output(target), "▶ Lonely Title\n♫ hi\n");

        let mut target = ConsoleTarget::new(Vec::new());
        target
            .set(&status("hi", track("Title", "   ")))
            .await
            .unwrap();
        assert_eq!(output(target), "▶ Title\n♫ hi\n");
    }

    #[tokio::test]
    async fn empty_text_and_empty_track() {
        let mut target = ConsoleTarget::new(Vec::new());
        target.set(&status("", Track::default())).await.unwrap();
        assert_eq!(output(target), "▶ \n♫ \n");
    }

    #[tokio::test]
    async fn non_line_kinds_print_their_text() {
        let mut target = ConsoleTarget::new(Vec::new());
        let mut s = status("♪", track("T", "A"));
        s.kind = StatusKind::Instrumental;
        s.line = None;
        target.set(&s).await.unwrap();
        let mut s2 = status("T · A", track("T", "A"));
        s2.kind = StatusKind::NoLyrics;
        s2.line = None;
        target.set(&s2).await.unwrap();
        assert_eq!(output(target), "▶ T — A\n♫ ♪\n♫ T · A\n");
    }

    #[tokio::test]
    async fn unicode_is_printed_unchanged() {
        let mut target = ConsoleTarget::new(Vec::new());
        let s = status(
            "🎵 გაზაფხული მოვიდა 春が来た",
            track("ქართული", "アーティスト"),
        );
        target.set(&s).await.unwrap();
        assert_eq!(
            output(target),
            "▶ ქართული — アーティスト\n♫ 🎵 გაზაფხული მოვიდა 春が来た\n"
        );
    }

    #[tokio::test]
    async fn line_breaks_never_split_an_update() {
        let mut target = ConsoleTarget::new(Vec::new());
        let s = status("one\ntwo\r\nthree\tfour", track("Multi\nLine", "Art\rist"));
        target.set(&s).await.unwrap();
        let out = output(target);
        assert_eq!(out, "▶ Multi Line — Art ist\n♫ one two  three four\n");
        assert_eq!(out.lines().count(), 2);
    }

    #[tokio::test]
    async fn very_long_text_is_not_truncated() {
        let mut target = ConsoleTarget::new(Vec::new());
        let long = "la ".repeat(5_000);
        target.set(&status(&long, track("T", "A"))).await.unwrap();
        let out = output(target);
        assert!(out.contains(&long));
    }

    #[tokio::test]
    async fn write_errors_are_other() {
        let mut target = ConsoleTarget::new(FailingWriter {
            fail_on_flush_only: false,
            written: Vec::new(),
        });
        let err = target.set(&status("x", track("T", "A"))).await.unwrap_err();
        assert!(matches!(err, TargetError::Other(_)), "{err:?}");
        let err = target.clear().await.unwrap_err();
        assert!(matches!(err, TargetError::Other(_)), "{err:?}");
        assert!(err.to_string().contains("terminal"), "{err}");
    }

    #[tokio::test]
    async fn flush_errors_are_other() {
        let mut target = ConsoleTarget::new(FailingWriter {
            fail_on_flush_only: true,
            written: Vec::new(),
        });
        let err = target.clear().await.unwrap_err();
        assert!(matches!(err, TargetError::Other(_)), "{err:?}");
        assert_eq!(target.into_inner().written, "■ cleared\n".as_bytes());
    }

    #[tokio::test]
    async fn header_is_retried_after_a_failed_write() {
        let mut target = ConsoleTarget::new(ToggleWriter {
            allow: false,
            written: Vec::new(),
        });
        let s = status("x", track("T", "A"));
        assert!(target.set(&s).await.is_err());
        target.out.allow = true;
        target.set(&s).await.unwrap();
        let written = String::from_utf8(target.into_inner().written).unwrap();
        assert_eq!(written, "▶ T — A\n♫ x\n");
    }
}
