//! Removing the credits some lyrics databases put around the lyrics.
//!
//! NetEase and Kugou keep a song's credits as timed lines before and after
//! the words (`[00:00.00] 作词 : Someone`, `[00:08.39]Lyrics by:Someone`, and
//! at the end `[04:21.77] 混音 : Someone`), and Kugou often starts with a
//! `Title - Artist` line. Left in, a status would show them as lyrics. Songs
//! without words can come as a single "pure music, enjoy" notice
//! (`纯音乐，请欣赏`) instead of being marked instrumental.

use crate::matcher::normalize_key;
use crate::types::Lyrics;

/// Longest credit label, in characters (`Mastering Engineer`, `母带工程师`).
const MAX_LABEL_CHARS: usize = 24;

/// Words that make a Chinese, Japanese or Korean label a credit: lyrics,
/// composition, arrangement, production, mixing, mastering, recording,
/// instruments, vocals, publishing and design, in simplified and
/// traditional Chinese, Japanese and Korean.
const CJK_CREDIT_WORDS: &[&str] = &[
    "词",
    "詞",
    "曲",
    "编",
    "編",
    "制作",
    "製作",
    "监制",
    "監製",
    "混音",
    "混缩",
    "缩混",
    "录音",
    "錄音",
    "母带",
    "母帶",
    "吉他",
    "贝斯",
    "貝斯",
    "鼓",
    "和声",
    "和聲",
    "合声",
    "合聲",
    "人声",
    "人聲",
    "弦乐",
    "弦樂",
    "键盘",
    "鍵盤",
    "钢琴",
    "鋼琴",
    "合成器",
    "编程",
    "編程",
    "工程",
    "出品",
    "发行",
    "發行",
    "企划",
    "企劃",
    "统筹",
    "統籌",
    "策划",
    "策劃",
    "宣传",
    "宣傳",
    "营销",
    "營銷",
    "推广",
    "推廣",
    "演唱",
    "原唱",
    "翻唱",
    "歌手",
    "版权",
    "版權",
    "封面",
    "设计",
    "設計",
    "视觉",
    "視覺",
    "助理",
    "演奏",
    "乐队",
    "樂隊",
    "乐器",
    "樂器",
    "配唱",
    "翻译",
    "翻譯",
    "监唱",
    "監唱",
    "音乐总监",
    "音樂總監",
    "작사",
    "작곡",
    "편곡",
    "프로듀서",
    "프로듀싱",
    "노래",
    "보컬",
    "코러스",
    "기타",
    "베이스",
    "드럼",
    "피아노",
    "키보드",
    "믹싱",
    "마스터링",
    "레코딩",
    "녹음",
    "엔지니어",
    "디렉터",
    "디렉팅",
];

/// English labels that mark a credit, as [`normalize_key`] writes them.
const ENGLISH_LABELS: &[&str] = &[
    "lyrics",
    "lyric",
    "lyrics by",
    "lyric by",
    "lyricist",
    "lyricists",
    "words",
    "words by",
    "written by",
    "writer",
    "writers",
    "songwriter",
    "songwriters",
    "composer",
    "composers",
    "composed by",
    "composition",
    "music",
    "music by",
    "arranger",
    "arranged by",
    "arrangement",
    "producer",
    "producers",
    "produced by",
    "production",
    "coproducer",
    "coproduced by",
    "executive producer",
    "vocal producer",
    "vocal production",
    "mixer",
    "mix",
    "mixed by",
    "mixing",
    "mix engineer",
    "mixing engineer",
    "master",
    "mastered by",
    "mastering",
    "mastering engineer",
    "recorded by",
    "recording",
    "recording engineer",
    "engineer",
    "engineered by",
    "vocal",
    "vocals",
    "lead vocal",
    "lead vocals",
    "backing vocal",
    "backing vocals",
    "background vocals",
    "chorus vocals",
    "guitar",
    "guitars",
    "electric guitar",
    "acoustic guitar",
    "bass",
    "drums",
    "keyboard",
    "keyboards",
    "piano",
    "strings",
    "synth",
    "synthesizer",
    "programming",
    "programmed by",
    "op",
    "sp",
    "publisher",
    "published by",
    "original singer",
    "singer",
];

/// "Pure music" in simplified and traditional Chinese, as in `纯音乐，请欣赏`
/// ("pure music, please enjoy").
const PURE_MUSIC_NOTICES: &[&str] = &["纯音乐", "純音樂"];

/// Removes the credits around lyrics and turns a "pure music" notice into an
/// instrumental song. `song` and `singer` are the names the database gave the
/// song, used to spot a `song - singer` header.
///
/// - At the start: lines that are credits ([`is_credit_line`]), the header
///   (`song - singer` or `singer - song`, with anything after it such as
///   `(Live)`) or empty are dropped, as long as at least one of them is a
///   credit or the header. A start without either stays as it is.
/// - At the end: credit lines, and empty lines among them, are cut. In synced
///   lyrics the first of them becomes an empty line, so the last words stop
///   showing when the credits start.
/// - When every line with text left is a "pure music" notice, the result is
///   instrumental, without lines.
///
/// Credits between lyric lines stay. `synced` and `source` are kept.
pub fn strip_credits(mut lyrics: Lyrics, song: &str, singer: &str) -> Lyrics {
    let song = normalize_key(song);
    let singer = normalize_key(singer);

    // The start: credits, the header and empty lines before the first words.
    let mut lead = 0;
    let mut lead_has_credits = false;
    for line in &lyrics.lines {
        let text = line.text.trim();
        if text.is_empty() {
            lead += 1;
        } else if is_credit_line(text) || is_header_line(text, &song, &singer) {
            lead += 1;
            lead_has_credits = true;
        } else {
            break;
        }
    }
    if lead_has_credits {
        lyrics.lines.drain(..lead);
    }

    // The end: credits and empty lines after the last words.
    let mut tail = lyrics.lines.len();
    let mut tail_has_credits = false;
    while let Some(line) = tail.checked_sub(1).and_then(|i| lyrics.lines.get(i)) {
        let text = line.text.trim();
        if text.is_empty() {
            tail -= 1;
        } else if is_credit_line(text) {
            tail -= 1;
            tail_has_credits = true;
        } else {
            break;
        }
    }
    if tail_has_credits {
        if lyrics.synced {
            if let Some(first) = lyrics.lines.get_mut(tail) {
                first.text.clear();
            }
            lyrics.lines.truncate(tail.saturating_add(1));
        } else {
            lyrics.lines.truncate(tail);
        }
    }

    let mut texts = lyrics
        .lines
        .iter()
        .map(|line| line.text.trim())
        .filter(|text| !text.is_empty())
        .peekable();
    let pure_music = texts.peek().is_some()
        && texts.all(|text| {
            PURE_MUSIC_NOTICES
                .iter()
                .any(|notice| text.contains(notice))
        });
    if pure_music {
        lyrics.lines.clear();
        lyrics.synced = false;
        lyrics.instrumental = true;
    }
    lyrics
}

/// `label : names`, also with `:` without spaces or the full-width `：`,
/// where the label has at most [`MAX_LABEL_CHARS`] characters and is either
/// Chinese, Japanese or Korean text holding one of [`CJK_CREDIT_WORDS`]
/// (`作词`, `混音工程师`, `작곡`) or one of the [`ENGLISH_LABELS`]
/// (`Lyrics by`, `Composer`, `Mixing Engineer`), in any letter case.
pub fn is_credit_line(text: &str) -> bool {
    let Some((label, _)) = text.split_once([':', '：']) else {
        return false;
    };
    let label = label.trim();
    if label.is_empty() || label.chars().count() > MAX_LABEL_CHARS {
        return false;
    }
    if label.chars().any(is_cjk) {
        CJK_CREDIT_WORDS.iter().any(|word| label.contains(word))
    } else {
        ENGLISH_LABELS.contains(&normalize_key(label).as_str())
    }
}

/// `song - singer` or `singer - song`, compared as [`normalize_key`] keys
/// (which `song` and `singer` already are). Words may follow the second part
/// (`Yellow - Coldplay (Live)`), and every ` - ` in the line is tried, so a
/// title with a dash in it still matches.
fn is_header_line(text: &str, song: &str, singer: &str) -> bool {
    if song.is_empty() || singer.is_empty() {
        return false;
    }
    let starts_with_words = |key: &str, words: &str| {
        key == words
            || key
                .strip_prefix(words)
                .is_some_and(|rest| rest.starts_with(' '))
    };
    text.match_indices(" - ").any(|(index, separator)| {
        let left = normalize_key(&text[..index]);
        let right = normalize_key(&text[index + separator.len()..]);
        (left == song && starts_with_words(&right, singer))
            || (left == singer && starts_with_words(&right, song))
    })
}

/// Chinese characters, Japanese kana and Korean Hangul.
fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{1100}'..='\u{11FF}'
            | '\u{3040}'..='\u{30FF}'
            | '\u{3130}'..='\u{318F}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{AC00}'..='\u{D7AF}'
            | '\u{F900}'..='\u{FAFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lrc::{from_plain, parse_lrc};

    fn texts(lyrics: &Lyrics) -> Vec<&str> {
        lyrics.lines.iter().map(|l| l.text.as_str()).collect()
    }

    fn starts(lyrics: &Lyrics) -> Vec<u64> {
        lyrics.lines.iter().map(|l| l.start_ms).collect()
    }

    /// The start and end of NetEase's lyrics for Coldplay's Yellow.
    const NETEASE_YELLOW: &str =
        "[00:00.000] 作词 : Guy Berryman/Jonny Buckland/Chris Martin/Will Champion\n\
        [00:01.000] 作曲 : Guy Berryman/Jonny Buckland/Chris Martin/Will Champion\n\
        [00:33.790]Look at the stars\n\
        [00:37.660]Look how they shine for you\n\
        [04:10.120]It was all yellow\n\
        [04:21.773] 电吉他 : Jonny Buckland\n\
        [04:24.000] 混音工程师 : Michael H. Brauer\n\
        [04:26.313] 母带工程师 : Ted Jensen\n";

    /// The start of Kugou's lyrics for the same song.
    const KUGOU_YELLOW: &str =
        "[ti:Yellow]\r\n[ar:Coldplay]\r\n[al:The Singles 1999-2006]\r\n[by:]\r\n[offset:0]\r\n\
        [00:00.00]Yellow - Coldplay (中文版本)\r\n\
        [00:08.39]Lyrics by:Guy Berryman/Jonny Buckland/Chris Martin/Will Champion\r\n\
        [00:16.79]Composed by:Guy Berryman/Jonny Buckland/Chris Martin/Will Champion\r\n\
        [00:25.18]Produced by:Ken Nelson/Coldplay\r\n\
        [00:33.58]Look at the stars\r\n\
        [00:37.43]Look how they shine for you\r\n";

    #[test]
    fn netease_credits_go_and_the_last_line_ends_where_they_started() {
        let lyrics = strip_credits(parse_lrc(NETEASE_YELLOW), "Yellow", "Coldplay");
        assert_eq!(
            texts(&lyrics),
            vec![
                "Look at the stars",
                "Look how they shine for you",
                "It was all yellow",
                ""
            ]
        );
        assert_eq!(starts(&lyrics), vec![33_790, 37_660, 250_120, 261_773]);
        assert!(lyrics.synced);
        assert!(!lyrics.instrumental);
    }

    #[test]
    fn kugou_header_and_credits_go() {
        let lyrics = strip_credits(parse_lrc(KUGOU_YELLOW), "Yellow", "Coldplay");
        assert_eq!(
            texts(&lyrics),
            vec!["Look at the stars", "Look how they shine for you"]
        );
        assert_eq!(lyrics.lines[0].start_ms, 33_580);
    }

    #[test]
    fn lyrics_without_credits_are_unchanged() {
        let input = "[00:00.00]\n[00:05.00]Look at the stars\n[00:09.00]\n[00:12.00]Fine: a line with a colon\n[00:20.00]\n";
        let lyrics = parse_lrc(input);
        assert_eq!(strip_credits(lyrics.clone(), "Yellow", "Coldplay"), lyrics);
        let plain = from_plain("Love: it is all I need\nLook at the stars");
        assert_eq!(strip_credits(plain.clone(), "Yellow", "Coldplay"), plain);
    }

    #[test]
    fn credits_in_the_middle_stay() {
        let input =
            "[00:01.00]作词 : Someone\n[00:10.00]First line\n[00:20.00]Producer: told you so\n[00:30.00]Last line\n";
        let lyrics = strip_credits(parse_lrc(input), "Song", "Singer");
        assert_eq!(
            texts(&lyrics),
            vec!["First line", "Producer: told you so", "Last line"]
        );
    }

    #[test]
    fn empty_lines_at_the_start_go_only_with_credits() {
        let input = "[00:00.00]\n[00:01.00]作曲 : Someone\n[00:02.00]\n[00:10.00]Words\n";
        let lyrics = strip_credits(parse_lrc(input), "Song", "Singer");
        assert_eq!(texts(&lyrics), vec!["Words"]);
        assert_eq!(starts(&lyrics), vec![10_000]);
    }

    #[test]
    fn an_empty_line_before_the_end_credits_is_kept_as_the_break() {
        let input = "[00:10.00]Words\n[00:20.00]\n[00:25.00]混音 : Someone\n[00:26.00]\n[00:27.00]母带 : Someone\n";
        let lyrics = strip_credits(parse_lrc(input), "Song", "Singer");
        assert_eq!(texts(&lyrics), vec!["Words", ""]);
        assert_eq!(starts(&lyrics), vec![10_000, 20_000]);
    }

    #[test]
    fn plain_lyrics_lose_their_credits_without_a_break() {
        let plain = from_plain(
            "作词 : Someone\nFirst line\nLast line\nMixed by: Someone\nMastered by: Someone",
        );
        let lyrics = strip_credits(plain, "Song", "Singer");
        assert_eq!(texts(&lyrics), vec!["First line", "Last line"]);
        assert!(!lyrics.synced);
    }

    #[test]
    fn only_credits_leave_nothing() {
        let input = "[00:00.00] 作词 : Someone\n[00:01.00] 作曲 : Someone\n";
        let lyrics = strip_credits(parse_lrc(input), "Song", "Singer");
        assert!(!lyrics.has_text());
        assert!(!lyrics.instrumental);
    }

    #[test]
    fn a_pure_music_notice_is_instrumental() {
        for input in [
            "[00:00.00] 纯音乐，请欣赏\n",
            "[00:00.00] 作曲 : Someone\n[00:01.00] 此歌曲为没有填词的纯音乐，请您欣赏\n",
            "[00:00.00]純音樂，請欣賞\n[00:05.00]\n",
        ] {
            let lyrics = strip_credits(parse_lrc(input), "Song", "Singer");
            assert!(lyrics.instrumental, "{input}");
            assert!(lyrics.lines.is_empty(), "{input}");
            assert!(!lyrics.synced, "{input}");
        }
        // Words besides the notice are lyrics.
        let input = "[00:00.00] 纯音乐，请欣赏\n[00:10.00]But here are words\n";
        let lyrics = strip_credits(parse_lrc(input), "Song", "Singer");
        assert!(!lyrics.instrumental);
        assert_eq!(lyrics.lines.len(), 2);
    }

    #[test]
    fn credit_lines_are_recognised() {
        for line in [
            "作词 : 方文山",
            "作曲：周杰伦",
            "编曲 : Someone",
            "制作人 : Someone",
            "混音工程师 : Someone",
            "母带工程师 : Someone",
            "电吉他 : Someone",
            "和声 : Someone",
            "作詞：Someone",
            "編曲:Someone",
            "작사 : 누군가",
            "작곡 : Someone",
            "프로듀서 : Someone",
            "Lyrics by:Guy Berryman",
            "Composed by: Someone",
            "Produced by:Ken Nelson/Coldplay",
            "Mixing Engineer : Someone",
            "Co-Producer: Someone",
            "OP : Sony Music Publishing",
            "Backing Vocals: Someone",
            "LYRICS BY: SOMEONE",
        ] {
            assert!(is_credit_line(line), "{line}");
        }
        for line in [
            "Look at the stars",
            "Love: it's all I need",
            "Baby: come home",
            "我爱你 : 一万年",
            "Chorus:",
            ": no label",
            "A very long label that is clearly a sentence: and more",
            "12:30 in the morning",
            "",
        ] {
            assert!(!is_credit_line(line), "{line}");
        }
    }

    #[test]
    fn header_lines_are_recognised() {
        let (song, singer) = (normalize_key("Yellow"), normalize_key("Coldplay"));
        for line in [
            "Yellow - Coldplay",
            "Coldplay - Yellow",
            "Yellow - Coldplay (中文版本)",
            "YELLOW - coldplay",
        ] {
            assert!(is_header_line(line, &song, &singer), "{line}");
        }
        for line in [
            "Yellow",
            "Yellow - Coldplays",
            "Yellow Coldplay",
            "Look at the stars - Coldplay",
        ] {
            assert!(!is_header_line(line, &song, &singer), "{line}");
        }
        // A dash inside the title.
        let (song, singer) = (normalize_key("Up - Down"), normalize_key("Band"));
        assert!(is_header_line("Up - Down - Band", &song, &singer));
        // Nothing to compare with.
        assert!(!is_header_line("Yellow - Coldplay", "", &singer));
    }
}
