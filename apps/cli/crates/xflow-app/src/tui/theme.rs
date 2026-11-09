use anyhow::{bail, Result};
use ratatui::style::{Color, Modifier, Style};
use std::collections::BTreeMap;

pub const BUILTINS: &[&str] = &[
    "paper",
    "midnight",
    "catppuccin",
    "nord",
    "gruvbox",
    "high-contrast",
];
#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub muted: Color,
    pub accent: Color,
    pub error: Color,
    pub selection: Color,
}
impl Theme {
    pub fn resolve(
        name: &str,
        custom: &BTreeMap<String, BTreeMap<String, String>>,
        colors: ColorMode,
    ) -> Result<Self> {
        let hex = match name {
            "paper" => [0xf7f6f3, 0x181818, 0x62615d, 0x315a50, 0x9f3030, 0xe2e0d9],
            "midnight" => [0x101724, 0xe6edf3, 0x8996ac, 0x7dcfff, 0xff757f, 0x28354d],
            "catppuccin" => [0x1e1e2e, 0xcdd6f4, 0xa6adc8, 0xcba6f7, 0xf38ba8, 0x45475a],
            "nord" => [0x2e3440, 0xeceff4, 0xa8b1c2, 0x88c0d0, 0xe5949b, 0x4c566a],
            "gruvbox" => [0x282828, 0xebdbb2, 0xbdae93, 0xb8bb26, 0xfb6a5a, 0x504945],
            "high-contrast" => [0x000000, 0xffffff, 0xffffff, 0xffff00, 0xff5555, 0x444444],
            _ if custom.contains_key(name) => {
                [0xf7f6f3, 0x181818, 0x62615d, 0x315a50, 0x9f3030, 0xe2e0d9]
            }
            _ => bail!("Unknown theme {name}; choose a built-in or define ui.themes.{name}"),
        };
        let [bg, fg, muted, accent, error, selection] = hex.map(rgb);
        let mut theme = Self {
            bg,
            fg,
            muted,
            accent,
            error,
            selection,
        };
        if let Some(slots) = custom.get(name) {
            for (slot, value) in slots {
                let target = match slot.as_str() {
                    "bg" => &mut theme.bg,
                    "fg" => &mut theme.fg,
                    "muted" => &mut theme.muted,
                    "accent" => &mut theme.accent,
                    "error" => &mut theme.error,
                    "selection" => &mut theme.selection,
                    _ => bail!(
                        "Unknown theme slot {slot}; use bg, fg, muted, accent, error, selection"
                    ),
                };
                *target = parse_color(value)?;
            }
        }
        match colors {
            ColorMode::None => {
                theme = Self {
                    bg: Color::Reset,
                    fg: Color::Reset,
                    muted: Color::Reset,
                    accent: Color::Reset,
                    error: Color::Reset,
                    selection: Color::Reset,
                }
            }
            ColorMode::Ansi => {
                let light = name == "paper" || custom.contains_key(name);
                theme = Self {
                    bg: if light { Color::White } else { Color::Black },
                    fg: if light { Color::Black } else { Color::White },
                    muted: if light { Color::DarkGray } else { Color::Gray },
                    accent: if light { Color::Blue } else { Color::LightCyan },
                    error: Color::Red,
                    selection: if light { Color::Gray } else { Color::DarkGray },
                };
            }
            ColorMode::TrueColor => (),
        }
        Ok(theme)
    }
    pub fn base(self) -> Style {
        Style::default().fg(self.fg).bg(self.bg)
    }
    pub fn selected(self) -> Style {
        self.base()
            .bg(self.selection)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED)
    }
}
#[derive(Clone, Copy)]
pub enum ColorMode {
    None,
    Ansi,
    TrueColor,
}
impl ColorMode {
    pub fn detect() -> Self {
        Self::from_env(&env_var)
    }
    fn from_env(var: &dyn Fn(&str) -> Option<String>) -> Self {
        if no_color(var) || dumb_term(var) {
            Self::None
        } else if var("COLORTERM").is_some_and(|v| v == "truecolor" || v == "24bit") {
            Self::TrueColor
        } else {
            Self::Ansi
        }
    }
}
pub fn env_var(key: &str) -> Option<String> {
    std::env::var_os(key).map(|v| v.to_string_lossy().into_owned())
}
/// no-color.org: only a non-empty NO_COLOR disables colour.
pub fn no_color(var: &dyn Fn(&str) -> Option<String>) -> bool {
    var("NO_COLOR").is_some_and(|v| !v.is_empty())
}
pub fn dumb_term(var: &dyn Fn(&str) -> Option<String>) -> bool {
    var("TERM").as_deref() == Some("dumb")
}
fn rgb(n: u32) -> Color {
    Color::Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8)
}
fn parse_color(text: &str) -> Result<Color> {
    if text.len() == 7 && text.starts_with('#') {
        if let Ok(n) = u32::from_str_radix(&text[1..], 16) {
            return Ok(rgb(n));
        }
    }
    match text {
        "black" => Ok(Color::Black),
        "white" => Ok(Color::White),
        "red" => Ok(Color::Red),
        "green" => Ok(Color::Green),
        "blue" => Ok(Color::Blue),
        "yellow" => Ok(Color::Yellow),
        "cyan" => Ok(Color::Cyan),
        "magenta" => Ok(Color::Magenta),
        "gray" => Ok(Color::Gray),
        "reset" => Ok(Color::Reset),
        _ => bail!("Invalid color {text}; use #RRGGBB or an ANSI color name"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(color: Color) -> f64 {
        let Color::Rgb(r, g, b) = color else {
            panic!("truecolor themes are RGB, got {color:?}")
        };
        let lin = |c: u8| {
            let c = c as f64 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    }
    fn contrast(a: Color, b: Color) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn every_builtin_theme_passes_wcag_aa_for_text() {
        for name in BUILTINS {
            let t = Theme::resolve(name, &BTreeMap::new(), ColorMode::TrueColor).unwrap();
            // Selected rows are REVERSED: selection-coloured text on fg.
            for (slot, fg, bg) in [
                ("fg", t.fg, t.bg),
                ("muted", t.muted, t.bg),
                ("accent", t.accent, t.bg),
                ("error", t.error, t.bg),
                ("selection", t.selection, t.fg),
            ] {
                let ratio = contrast(fg, bg);
                assert!(
                    ratio >= 4.5,
                    "{name} {slot} is {ratio:.2}:1, below WCAG AA 4.5:1"
                );
            }
        }
    }

    #[test]
    fn color_mode_follows_no_color_and_dumb_terminals() {
        let mode = |vars: &[(&str, &str)]| {
            let vars = vars.to_vec();
            ColorMode::from_env(&move |key| {
                vars.iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_string())
            })
        };
        let truecolor = [("COLORTERM", "truecolor")];
        assert!(matches!(mode(&truecolor), ColorMode::TrueColor));
        assert!(matches!(mode(&[]), ColorMode::Ansi));
        assert!(matches!(
            mode(&[("NO_COLOR", "1"), truecolor[0]]),
            ColorMode::None
        ));
        assert!(matches!(
            mode(&[("NO_COLOR", ""), truecolor[0]]),
            ColorMode::TrueColor
        ));
        assert!(matches!(
            mode(&[("TERM", "dumb"), truecolor[0]]),
            ColorMode::None
        ));
        assert!(matches!(
            mode(&[("TERM", "xterm-256color")]),
            ColorMode::Ansi
        ));
    }
}
