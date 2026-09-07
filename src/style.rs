//! Tiny ANSI styling helper, gated on a resolved on/off decision.

use crate::config::ColorChoice;

/// A styler that either wraps text in ANSI escapes or returns it unchanged.
pub struct Style {
    color: bool,
}

impl Style {
    pub fn new(color: bool) -> Self {
        Self { color }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    pub fn bold(&self, text: &str) -> String {
        self.paint("1", text)
    }

    pub fn green(&self, text: &str) -> String {
        self.paint("32", text)
    }

    pub fn cyan(&self, text: &str) -> String {
        self.paint("36", text)
    }

    pub fn red(&self, text: &str) -> String {
        self.paint("31", text)
    }
}

/// Resolve a [`ColorChoice`] into an on/off decision for a stream.
///
/// `Auto` colorizes only when the stream is a terminal and `NO_COLOR` is unset
/// (or empty), per <https://no-color.org/>.
pub fn resolve(choice: ColorChoice, stream_is_tty: bool) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => {
            stream_is_tty && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_and_never_ignore_tty() {
        assert!(resolve(ColorChoice::Always, false));
        assert!(!resolve(ColorChoice::Never, true));
    }

    #[test]
    fn plain_when_disabled() {
        let s = Style::new(false);
        assert_eq!(s.bold("hi"), "hi");
    }

    #[test]
    fn escapes_when_enabled() {
        let s = Style::new(true);
        assert_eq!(s.green("ok"), "\x1b[32mok\x1b[0m");
    }
}
