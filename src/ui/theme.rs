//! Chrome colors from the active Omarchy theme, with an Adwaita-dark fallback.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Clone)]
pub struct ThemeColors {
    pub background: String,
    pub darker_background: String,
    pub dark_background: String,
    pub lighter_background: String,
    pub foreground: String,
    pub dark_foreground: String,
    pub accent: String,
    pub selection: String,
    pub muted: String,
    pub accent_rgb: (f32, f32, f32),
    pub background_rgb: (f32, f32, f32),
}

impl ThemeColors {
    pub fn load() -> Self {
        let fallback = Self::fallback();
        let Some(path) = colors_path() else {
            return fallback;
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return fallback;
        };
        let Ok(raw) = toml::from_str::<Raw>(&text) else {
            return fallback;
        };
        let pick = |value: Option<String>, fallback: &str| {
            value
                .filter(|color| parse_hex(color).is_some())
                .unwrap_or_else(|| fallback.to_string())
        };
        let background = pick(raw.background, &fallback.background);
        let accent = pick(raw.accent, &fallback.accent);
        Self {
            darker_background: pick(raw.darker_background, &fallback.darker_background),
            dark_background: pick(raw.dark_background, &fallback.dark_background),
            lighter_background: pick(raw.lighter_background, &fallback.lighter_background),
            foreground: pick(raw.foreground, &fallback.foreground),
            dark_foreground: pick(raw.dark_foreground, &fallback.dark_foreground),
            selection: pick(raw.selection, &fallback.selection),
            muted: pick(raw.muted, &fallback.muted),
            accent_rgb: parse_hex(&accent).unwrap_or(fallback.accent_rgb),
            background_rgb: parse_hex(&background).unwrap_or(fallback.background_rgb),
            background,
            accent,
        }
    }

    pub fn css(&self) -> String {
        format!(
            r#"
            window.pixel-root, .pixel-root {{
              background-color: {bg};
              color: {fg};
            }}
            headerbar, .pixel-menubar {{
              background-color: {dark};
              color: {fg};
            }}
            .pixel-panel {{
              background-color: {darker};
              color: {fg};
            }}
            .pixel-toolbar, .pixel-status {{
              background-color: {dark};
              color: {muted_fg};
            }}
            button.pixel-tool {{
              background: transparent;
              color: {fg};
              border-radius: 8px;
              margin: 4px;
              padding: 10px 4px;
            }}
            button.pixel-tool:checked {{
              background-color: {accent};
              color: {darker};
            }}
            row.pixel-layer:selected {{
              background-color: {selection};
            }}
            paned.pixel-split > separator {{
              background-color: {muted};
              min-width: 8px;
            }}
            "#,
            bg = self.background,
            fg = self.foreground,
            dark = self.dark_background,
            darker = self.darker_background,
            muted_fg = self.dark_foreground,
            accent = self.accent,
            selection = self.selection,
            muted = self.muted,
        )
    }

    fn fallback() -> Self {
        let background = "#1e1e1e".to_string();
        let accent = "#3584e4".to_string();
        Self {
            darker_background: "#121212".into(),
            dark_background: "#242424".into(),
            lighter_background: "#2c2c2c".into(),
            foreground: "#ffffff".into(),
            dark_foreground: "#9a9a9a".into(),
            selection: "#303030".into(),
            muted: "#3a3a3a".into(),
            accent_rgb: parse_hex(&accent).unwrap(),
            background_rgb: parse_hex(&background).unwrap(),
            background,
            accent,
        }
    }
}

pub fn colors_path() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let path = home.join(".local/state/omarchy/current/theme/colors.toml");
    path.is_file().then_some(path)
}

fn parse_hex(color: &str) -> Option<(f32, f32, f32)> {
    let hex = color.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    Some((
        ((value >> 16) & 255) as f32 / 255.0,
        ((value >> 8) & 255) as f32 / 255.0,
        (value & 255) as f32 / 255.0,
    ))
}

#[derive(Deserialize)]
struct Raw {
    accent: Option<String>,
    background: Option<String>,
    darker_background: Option<String>,
    dark_background: Option<String>,
    lighter_background: Option<String>,
    foreground: Option<String>,
    dark_foreground: Option<String>,
    selection: Option<String>,
    muted: Option<String>,
}
