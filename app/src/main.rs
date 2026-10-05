//! The `lyrix` command.
//!
//! ```text
//! lyrix [--config <file>] [-v…] [COMMAND]
//!
//! run (default)            Show lyrics as your status until Ctrl+C.
//!     --no-discord         Don't use Discord Rich Presence this time.
//!     --quiet              Don't print statuses in the terminal.
//! now                      Print what is playing, once.
//! lyrics                   Look up lyrics and print them as LRC.
//!     --artist <a> --title <t> [--album <al>] [--duration <seconds>]
//!     (without --artist/--title: the song playing now)
//! config init [--force]    Write a config file with every setting and its default.
//! config path              Print where the config file is.
//! config show              Print the settings in use.
//! config check             Report problems with the settings.
//! pause / resume           Clear the status and stop / start updating a running lyrix.
//! offset <ms>              Nudge the song playing now: positive shows lines later.
//! offset reset             Remove the nudge for the song playing now.
//! cache clear              Delete cached lyrics.
//! ```
//!
//! Advanced mode options print `BAN_WARNING` on `run` and `config check`. In this
//! build their connectors are not included yet, so `run` says so and continues
//! without them.

fn main() {
    todo!()
}
