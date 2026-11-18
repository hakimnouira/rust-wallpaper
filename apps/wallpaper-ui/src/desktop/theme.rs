//! Lightweight UI theming. Theme state lives in the UI process only: applying a
//! theme is a few egui style assignments, so changing colors never touches the
//! daemon's playback loop or adds per-frame IPC work.

use eframe::egui;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreset {
    Midnight,
    Graphite,
    Aurora,
    Ember,
    Frost,
    Custom,
}

impl ThemePreset {
    pub const ALL: [Self; 6] = [
        Self::Midnight,
        Self::Graphite,
        Self::Aurora,
        Self::Ember,
        Self::Frost,
        Self::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Midnight => "Midnight (blue)",
            Self::Graphite => "Graphite (neutral)",
            Self::Aurora => "Aurora (teal)",
            Self::Ember => "Ember (warm)",
            Self::Frost => "Frost (light)",
            Self::Custom => "Custom",
        }
    }

    fn palette(self) -> Option<(&'static str, &'static str, bool)> {
        match self {
            Self::Midnight => Some(("#5B8CFF", "#121826", true)),
            Self::Graphite => Some(("#B0BEC5", "#1B1D1F", true)),
            Self::Aurora => Some(("#35C6A0", "#0E1C1B", true)),
            Self::Ember => Some(("#FF7A59", "#201614", true)),
            Self::Frost => Some(("#2563EB", "#F5F7FB", false)),
            Self::Custom => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiTheme {
    pub preset: ThemePreset,
    pub accent: String,
    pub background: String,
    pub dark_mode: bool,
    pub scale: f32,
}

impl Default for UiTheme {
    fn default() -> Self {
        Self {
            preset: ThemePreset::Midnight,
            accent: "#5B8CFF".into(),
            background: "#121826".into(),
            dark_mode: true,
            scale: 1.0,
        }
    }
}

impl UiTheme {
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = config_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, bytes);
        }
    }

    /// Resolves preset colors into concrete values. Custom themes use the stored hex.
    pub fn colors(&self) -> (egui::Color32, egui::Color32, bool) {
        if let Some((accent, background, dark)) = self.preset.palette() {
            return (parse_hex(accent), parse_hex(background), dark);
        }
        (
            parse_hex(&self.accent),
            parse_hex(&self.background),
            self.dark_mode,
        )
    }

    /// Applies the theme once. Called on startup and only when the user changes it.
    pub fn apply(&self, ctx: &egui::Context) {
        let (accent, background, dark) = self.colors();
        let mut visuals = if dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };

        let border = shade(background, if dark { 0.16 } else { -0.14 });
        let raised = shade(background, if dark { 0.05 } else { -0.035 });
        let sunken = shade(background, if dark { -0.07 } else { -0.06 });
        let text = if dark {
            egui::Color32::from_gray(232)
        } else {
            egui::Color32::from_gray(32)
        };

        visuals.panel_fill = background;
        visuals.window_fill = background;
        visuals.faint_bg_color = raised;
        visuals.extreme_bg_color = sunken;
        visuals.code_bg_color = sunken;
        visuals.hyperlink_color = accent;
        visuals.selection.bg_fill = shade(accent, if dark { -0.30 } else { 0.55 });
        visuals.selection.stroke.color = accent;

        visuals.widgets.noninteractive.bg_fill = raised;
        visuals.widgets.noninteractive.weak_bg_fill = raised;
        visuals.widgets.noninteractive.bg_stroke.color = border;
        visuals.widgets.noninteractive.fg_stroke.color =
            shade(text, if dark { -0.30 } else { 0.30 });

        visuals.widgets.inactive.weak_bg_fill = raised;
        visuals.widgets.inactive.bg_fill = raised;
        visuals.widgets.inactive.bg_stroke.color = border;
        visuals.widgets.inactive.fg_stroke.color = text;

        visuals.widgets.hovered.weak_bg_fill = shade(accent, if dark { -0.42 } else { 0.68 });
        visuals.widgets.hovered.bg_fill = shade(accent, if dark { -0.38 } else { 0.62 });
        visuals.widgets.hovered.bg_stroke.color = accent;
        visuals.widgets.hovered.fg_stroke.color = text;

        visuals.widgets.active.weak_bg_fill = shade(accent, if dark { -0.26 } else { 0.48 });
        visuals.widgets.active.bg_fill = shade(accent, if dark { -0.22 } else { 0.42 });
        visuals.widgets.active.bg_stroke.color = accent;
        visuals.widgets.active.fg_stroke.color = text;

        visuals.widgets.open = visuals.widgets.hovered;

        ctx.set_visuals(visuals);
        ctx.set_zoom_factor(self.scale.clamp(0.8, 1.3));
    }
}

/// Raised surface used for cards, settings groups and status tiles.
pub fn card_frame(theme: &UiTheme) -> egui::Frame {
    let (_, background, dark) = theme.colors();
    egui::Frame::none()
        .fill(shade(background, if dark { 0.055 } else { -0.02 }))
        .stroke(egui::Stroke::new(
            0.5_f32,
            shade(background, if dark { 0.22 } else { -0.16 }),
        ))
        .rounding(egui::Rounding::same(12.0))
        .inner_margin(egui::Margin::same(14.0))
        .outer_margin(egui::Margin::symmetric(0.0, 6.0))
}

/// Quieter surface for secondary content such as filters or helper text.
pub fn subtle_frame(theme: &UiTheme) -> egui::Frame {
    let (_, background, dark) = theme.colors();
    egui::Frame::none()
        .fill(shade(background, if dark { 0.025 } else { -0.012 }))
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::same(10.0))
        .outer_margin(egui::Margin::symmetric(0.0, 4.0))
}

pub fn accent_color(theme: &UiTheme) -> egui::Color32 {
    theme.colors().0
}

pub fn soft_accent(theme: &UiTheme) -> egui::Color32 {
    let (accent, _, dark) = theme.colors();
    shade(accent, if dark { -0.48 } else { 0.68 })
}

pub fn monitor_fill(theme: &UiTheme, selected: bool, primary: bool) -> egui::Color32 {
    let (accent, background, dark) = theme.colors();
    if selected {
        mix(accent, background, if dark { 0.45 } else { 0.58 })
    } else if primary {
        mix(accent, background, if dark { 0.78 } else { 0.86 })
    } else {
        shade(background, if dark { 0.08 } else { -0.05 })
    }
}

pub fn muted_color(theme: &UiTheme) -> egui::Color32 {
    let (_, _, dark) = theme.colors();
    if dark {
        egui::Color32::from_gray(160)
    } else {
        egui::Color32::from_gray(110)
    }
}

fn config_path() -> Option<std::path::PathBuf> {
    let app_data = std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("APPDATA"))?;
    Some(
        std::path::PathBuf::from(app_data)
            .join("RustWallpaper")
            .join("ui-theme.json"),
    )
}

fn parse_hex(value: &str) -> egui::Color32 {
    let value = value.trim().trim_start_matches('#');
    if value.len() != 6 {
        return egui::Color32::from_gray(40);
    }
    let Ok(r) = u8::from_str_radix(&value[0..2], 16) else {
        return egui::Color32::from_gray(40);
    };
    let Ok(g) = u8::from_str_radix(&value[2..4], 16) else {
        return egui::Color32::from_gray(40);
    };
    let Ok(b) = u8::from_str_radix(&value[4..6], 16) else {
        return egui::Color32::from_gray(40);
    };
    egui::Color32::from_rgb(r, g, b)
}

pub fn color_to_hex(color: egui::Color32) -> String {
    let [r, g, b, _] = color.to_array();
    format!("#{r:02X}{g:02X}{b:02X}")
}

fn mix(from: egui::Color32, to: egui::Color32, amount: f32) -> egui::Color32 {
    let [r1, g1, b1, a1] = from.to_array();
    let [r2, g2, b2, a2] = to.to_array();
    let t = amount.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| -> u8 {
        (a as f32 + (b as f32 - a as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    egui::Color32::from_rgba_premultiplied(
        channel(r1, r2),
        channel(g1, g2),
        channel(b1, b2),
        channel(a1, a2),
    )
}

/// Mixes a color towards white (`amount > 0`) or black (`amount < 0`).
fn shade(color: egui::Color32, amount: f32) -> egui::Color32 {
    let [r, g, b, a] = color.to_array();
    let target = if amount >= 0.0 { 255.0 } else { 0.0 };
    let t = amount.abs().clamp(0.0, 1.0);
    let channel = |value: u8| -> u8 {
        (value as f32 + (target - value as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    egui::Color32::from_rgba_premultiplied(channel(r), channel(g), channel(b), a)
}
