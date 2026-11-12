// SPDX-License-Identifier: MIT
//! Text that came from somewhere else, made fit to print.
//!
//! A phone's answers, a server's, an HTTP client's error and the name of a
//! file on the owner's disk all end up in sentences a front end prints, and
//! the command-line tool prints them on a terminal. A terminal acts on
//! control characters: an escape sequence clears the screen, moves the
//! cursor or sets the window's title, a carriage return overwrites the line
//! that was just written. So no such character leaves the core:
//!
//! - what a phone says is passed through [`plain`] where it is read
//!   (`fastboot`), before it is compared, kept or put in an error;
//! - every string of a [`crate::flow::Screen`] is passed through it where
//!   the screen is made, whatever it was built from.
//!
//! Nothing is dropped silently. A tab or a line break becomes a space, and
//! every other such character is spelled out where it stood (`\u{1b}` for
//! ESC), so the text still shows that something odd was there and two
//! answers that differ only by one of them still differ.

/// Whether `c` is acted on rather than shown: the C0 and C1 control
/// characters and DEL, and the marks that change the reading order of the
/// text around them or hide in it (zero-width and direction marks, the
/// embeddings, overrides and isolates, the byte-order mark).
fn acted_on(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}')
}

/// `text` with nothing in it that a terminal acts on. Text without such
/// characters comes back as it is.
pub fn plain(text: &str) -> String {
    if !text.chars().any(acted_on) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '\t' | '\n' | '\r' => out.push(' '),
            c if acted_on(c) => out.extend(c.escape_unicode()),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_text_is_left_as_it_is() {
        for text in [
            "",
            "cheetah",
            "Pixel 7 Pro",
            "cloudripper-14.5-11677881",
            "g5300q-230626-230818-B-10679446",
            "It's \"quoted\", 100 % and a back\\slash: \\u{1b} typed out",
            "Zürich, 東京, Ελληνικά, emoji 🙂",
        ] {
            assert_eq!(plain(text), text);
        }
    }

    #[test]
    fn what_a_terminal_acts_on_is_spelled_out() {
        // Clear the screen, set the window title, ring the bell
        assert_eq!(
            plain("a\u{1b}[2J\u{1b}]0;title\u{7}b"),
            "a\\u{1b}[2J\\u{1b}]0;title\\u{7}b"
        );
        // An 8-bit CSI, DEL, a NUL, a backspace
        assert_eq!(plain("\u{9b}31m"), "\\u{9b}31m");
        assert_eq!(plain("x\u{7f}\0\u{8}"), "x\\u{7f}\\u{0}\\u{8}");
        // A carriage return can't overwrite what came before it
        assert_eq!(plain("failed\rall is well"), "failed all is well");
        assert_eq!(plain("one\ntwo\tthree\r\n"), "one two three  ");
        // The reading order can't be turned round, and nothing hides
        assert_eq!(plain("abc\u{202e}fdp.exe"), "abc\\u{202e}fdp.exe");
        assert_eq!(plain("che\u{200b}etah"), "che\\u{200b}etah");
        assert_eq!(
            plain("\u{feff}x\u{2066}y\u{2069}"),
            "\\u{feff}x\\u{2066}y\\u{2069}"
        );
    }

    #[test]
    fn nothing_acted_on_is_left_and_twice_is_the_same_as_once() {
        let every: String = (0u32..0x3000).filter_map(char::from_u32).collect();
        let once = plain(&every);
        assert!(!once.chars().any(acted_on));
        assert_eq!(plain(&once), once);
    }

    #[test]
    fn answers_that_differ_by_such_a_character_still_differ() {
        assert_ne!(plain("cheetah\0"), plain("cheetah"));
        assert_ne!(plain("cheetah\u{1b}"), plain("cheetah"));
        assert_ne!(plain("SERIAL\u{1}"), plain("SERIAL\u{2}"));
    }
}
