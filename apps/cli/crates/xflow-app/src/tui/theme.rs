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
            "nord" => [0x2e3440, 0xeceff4, 0xa8b1c2, 0x88c0d0, 0xbf616a, 0x4c566a],
            "gruvbox" => [0x282828, 0xebdbb2, 0xbdae93, 0xb8bb26, 0xfb4934, 0x504945],
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
        if std::env::var_os("NO_COLOR").is_some() {
            Self::None
        } else if std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit") {
            Self::TrueColor
        } else {
            Self::Ansi
        }
    }
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
