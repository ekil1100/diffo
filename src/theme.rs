use crate::{Error, Result};
use std::{fs::File, io::Read, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub hex: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeTokens {
    pub name: &'static str,
    pub bg_default: Color,
    pub bg_panel: Color,
    pub bg_selected: Color,
    pub fg_default: Color,
    pub fg_muted: Color,
    pub fg_accent: Color,
    pub diff_add_bg: Color,
    pub diff_add_fg: Color,
    pub diff_del_bg: Color,
    pub diff_del_fg: Color,
    pub diff_context_fg: Color,
    pub border: Color,
    pub warning: Color,
    pub error_color: Color,
    pub comment_badge: Color,
    pub reviewed_badge: Color,
    pub unreviewed_badge: Color,
    pub syntax_keyword: Color,
    pub syntax_string: Color,
    pub syntax_comment: Color,
    pub syntax_type: Color,
    pub syntax_function: Color,
    pub syntax_number: Color,
    pub syntax_operator: Color,
    pub syntax_plain: Color,
}

pub fn catppuccin_mocha() -> ThemeTokens {
    ThemeTokens {
        name: "catppuccin-mocha",
        bg_default: Color { hex: "#1e1e2e" },
        bg_panel: Color { hex: "#181825" },
        bg_selected: Color { hex: "#313244" },
        fg_default: Color { hex: "#cdd6f4" },
        fg_muted: Color { hex: "#9399b2" },
        fg_accent: Color { hex: "#89b4fa" },
        diff_add_bg: Color { hex: "#1e3a2f" },
        diff_add_fg: Color { hex: "#a6e3a1" },
        diff_del_bg: Color { hex: "#3b2228" },
        diff_del_fg: Color { hex: "#f38ba8" },
        diff_context_fg: Color { hex: "#bac2de" },
        border: Color { hex: "#45475a" },
        warning: Color { hex: "#f9e2af" },
        error_color: Color { hex: "#f38ba8" },
        comment_badge: Color { hex: "#f5c2e7" },
        reviewed_badge: Color { hex: "#a6e3a1" },
        unreviewed_badge: Color { hex: "#fab387" },
        syntax_keyword: Color { hex: "#cba6f7" },
        syntax_string: Color { hex: "#a6e3a1" },
        syntax_comment: Color { hex: "#6c7086" },
        syntax_type: Color { hex: "#f9e2af" },
        syntax_function: Color { hex: "#89b4fa" },
        syntax_number: Color { hex: "#fab387" },
        syntax_operator: Color { hex: "#94e2d5" },
        syntax_plain: Color { hex: "#cdd6f4" },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ansi {
    pub enabled: bool,
    pub true_color: bool,
}

impl Ansi {
    pub fn init(force: bool) -> Self {
        let no_color = std::env::var_os("NO_COLOR").is_some();
        let colorterm = std::env::var_os("COLORTERM");
        Self::from_environment(
            force,
            no_color,
            colorterm
                .as_ref()
                .map(|value| value.to_string_lossy())
                .as_deref(),
        )
    }

    fn from_environment(force: bool, no_color: bool, colorterm: Option<&str>) -> Self {
        if !force && no_color {
            return Self {
                enabled: false,
                true_color: false,
            };
        }
        Self {
            enabled: true,
            true_color: colorterm
                .is_some_and(|value| value.contains("truecolor") || value.contains("24bit")),
        }
    }

    pub fn fg(self, color: Color) -> String {
        self.sgr_color(38, color)
    }

    pub fn bg(self, color: Color) -> String {
        self.sgr_color(48, color)
    }

    pub fn reset(self) -> &'static str {
        if self.enabled { "\x1b[0m" } else { "" }
    }

    fn sgr_color(self, plane: u8, color: Color) -> String {
        if !self.enabled || !self.true_color {
            return String::new();
        }
        match parse_hex(color.hex.as_bytes()) {
            Some([r, g, b]) => format!("\x1b[{plane};2;{r};{g};{b}m"),
            None => String::new(),
        }
    }
}

fn parse_hex(hex: &[u8]) -> Option<[u8; 3]> {
    let raw = hex.strip_prefix(b"#").unwrap_or(hex);
    if raw.len() != 6 || !raw.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let digit = |byte: u8| {
        if byte.is_ascii_digit() {
            byte - b'0'
        } else {
            byte.to_ascii_lowercase() - b'a' + 10
        }
    };
    Some([
        digit(raw[0]) * 16 + digit(raw[1]),
        digit(raw[2]) * 16 + digit(raw[3]),
        digit(raw[4]) * 16 + digit(raw[5]),
    ])
}

pub fn list_builtins() -> String {
    "catppuccin-mocha  built-in default\nbase16/base24     supported via diffo themes validate <file>\n".into()
}

const MAX_THEME_BYTES: u64 = 4 * 1024 * 1024;

pub fn validate_base_theme_file(path: impl AsRef<Path>) -> Result<String> {
    let file = File::open(path).map_err(|_| Error::ThemeInvalid)?;
    let mut content = Vec::new();
    file.take(MAX_THEME_BYTES + 1)
        .read_to_end(&mut content)
        .map_err(|_| Error::ThemeInvalid)?;
    if content.len() as u64 > MAX_THEME_BYTES {
        return Err(Error::ThemeInvalid);
    }
    validate_base_theme(&content)
}

fn validate_base_theme(content: &[u8]) -> Result<String> {
    let mut base16_count = 0;
    let mut base24_count = 0;
    for index in 0..24 {
        let key = format!("base{index:02x}");
        if key_has_hex_color(content, key.as_bytes()) {
            if index < 16 {
                base16_count += 1;
            } else {
                base24_count += 1;
            }
        }
    }
    if base16_count < 16 {
        return Err(Error::ThemeInvalid);
    }
    Ok(format!(
        "valid Base{} theme ({} color slots)",
        if base24_count >= 8 { "24" } else { "16" },
        base16_count + base24_count
    ))
}

fn key_has_hex_color(content: &[u8], key: &[u8]) -> bool {
    // Match the original format-agnostic validator: a baseNN occurrence must
    // have a standalone six-hex value later on the same line. This is not YAML parsing.
    content
        .windows(key.len())
        .enumerate()
        .any(|(index, window)| {
            if window != key {
                return false;
            }
            let after = &content[index + key.len()..];
            let line_end = after
                .iter()
                .position(|byte| *byte == b'\n')
                .unwrap_or(after.len());
            line_contains_hex_color(&after[..line_end])
        })
}

fn line_contains_hex_color(line: &[u8]) -> bool {
    line.windows(6).enumerate().any(|(index, hex)| {
        let before_ok = index == 0 || !line[index - 1].is_ascii_hexdigit();
        let after_ok = index + 6 == line.len() || !line[index + 6].is_ascii_hexdigit();
        before_ok && after_ok && parse_hex(hex).is_some()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn theme_content(slots: usize, value: &str) -> String {
        (0..slots)
            .map(|index| format!("base{index:02x}: \"{value}\"\n"))
            .collect()
    }

    #[test]
    fn base16_validator_accepts_keys() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(theme_content(16, "ffffff").as_bytes())
            .unwrap();
        assert_eq!(
            validate_base_theme_file(file.path()).unwrap(),
            "valid Base16 theme (16 color slots)"
        );
    }

    #[test]
    fn validator_recognizes_base24_and_partial_extra_slots() {
        for (slots, name) in [(16, "16"), (19, "16"), (24, "24")] {
            let content = theme_content(slots, "#Aa09Ff");
            assert_eq!(
                validate_base_theme(content.as_bytes()).unwrap(),
                format!("valid Base{name} theme ({slots} color slots)")
            );
        }
    }

    #[test]
    fn validator_rejects_missing_invalid_long_and_cross_line_colors() {
        for content in [
            String::new(),
            theme_content(15, "ffffff"),
            theme_content(16, "fffff"),
            theme_content(16, "fffffff"),
            theme_content(16, "gggggg"),
            theme_content(16, ""),
            theme_content(16, "\nffffff"),
            "base00: ffffff\n".repeat(16),
        ] {
            assert!(
                matches!(
                    validate_base_theme(content.as_bytes()),
                    Err(Error::ThemeInvalid)
                ),
                "{content:?}"
            );
        }
    }

    #[test]
    fn validator_accepts_later_valid_duplicate_and_crlf() {
        let content = format!(
            "{}{}",
            theme_content(16, "invalid"),
            theme_content(16, "123ABC").replace('\n', "\r\n")
        );
        assert_eq!(
            validate_base_theme(content.as_bytes()).unwrap(),
            "valid Base16 theme (16 color slots)"
        );
    }

    #[test]
    fn validator_rejects_unreadable_and_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            validate_base_theme_file(dir.path().join("missing.yaml")),
            Err(Error::ThemeInvalid)
        ));
        let mut file = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        file.write_all(theme_content(16, "ffffff").as_bytes())
            .unwrap();
        file.as_file().set_len(MAX_THEME_BYTES + 1).unwrap();
        assert!(matches!(
            validate_base_theme_file(file.path()),
            Err(Error::ThemeInvalid)
        ));
    }

    #[test]
    fn parse_hex_accepts_optional_hash_and_rejects_malformed_values() {
        assert_eq!(parse_hex(b"#1e1e2e"), Some([30, 30, 46]));
        assert_eq!(parse_hex(b"Aa00FF"), Some([170, 0, 255]));
        for invalid in ["", "#fff", "0000000", "gg0000", "++0000", "架架"] {
            assert_eq!(parse_hex(invalid.as_bytes()), None);
        }
    }

    #[test]
    fn ansi_obeys_no_color_force_and_truecolor_detection_without_global_mutation() {
        assert_eq!(
            Ansi::from_environment(false, true, Some("truecolor")),
            Ansi {
                enabled: false,
                true_color: false
            }
        );
        assert_eq!(
            Ansi::from_environment(true, true, Some("truecolor")),
            Ansi {
                enabled: true,
                true_color: true
            }
        );
        assert_eq!(
            Ansi::from_environment(false, false, None),
            Ansi {
                enabled: true,
                true_color: false
            }
        );
        for value in ["truecolor", "24bit", "supports-truecolor"] {
            assert!(Ansi::from_environment(false, false, Some(value)).true_color);
        }
        assert!(!Ansi::from_environment(false, false, Some("TRUECOLOR")).true_color);
    }

    #[test]
    fn ansi_encodes_rgb_and_handles_disabled_or_invalid_colors() {
        let ansi = Ansi {
            enabled: true,
            true_color: true,
        };
        let color = Color { hex: "#1e1e2e" };
        assert_eq!(ansi.fg(color), "\x1b[38;2;30;30;46m");
        assert_eq!(ansi.bg(color), "\x1b[48;2;30;30;46m");
        assert_eq!(ansi.reset(), "\x1b[0m");
        assert_eq!(ansi.fg(Color { hex: "invalid" }), "");
        for disabled in [
            Ansi {
                enabled: false,
                true_color: true,
            },
            Ansi {
                enabled: true,
                true_color: false,
            },
        ] {
            assert_eq!(disabled.fg(color), "");
            assert_eq!(disabled.bg(color), "");
        }
        assert_eq!(
            Ansi {
                enabled: false,
                true_color: false
            }
            .reset(),
            ""
        );
    }

    #[test]
    fn default_palette_and_builtin_listing_match_original() {
        let palette = catppuccin_mocha();
        assert_eq!(palette.name, "catppuccin-mocha");
        assert_eq!(palette.bg_default.hex, "#1e1e2e");
        assert_eq!(palette.syntax_plain, palette.fg_default);
        assert_eq!(palette.syntax_function, palette.fg_accent);
        assert_eq!(palette.reviewed_badge, palette.diff_add_fg);
        assert_eq!(palette.error_color, palette.diff_del_fg);
        assert_eq!(
            list_builtins(),
            "catppuccin-mocha  built-in default\nbase16/base24     supported via diffo themes validate <file>\n"
        );
    }
}
