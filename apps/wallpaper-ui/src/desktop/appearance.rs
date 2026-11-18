use super::theme::{color_to_hex, ThemePreset};
use super::*;

impl DesktopApp {
    pub(super) fn render_appearance(&mut self, ui: &mut egui::Ui) {
        ui.heading("Appearance");
        ui.small("Saved locally in the UI process. Changing the theme never restarts playback.");
        ui.add_space(6.0);

        let theme_before = self.theme.clone();
        let (accent, background, dark) = self.theme.colors();

        card_frame(&self.theme).show(ui, |ui| {
            ui.strong("Theme preset");
            ui.add_space(4.0);
            egui::ComboBox::from_id_salt("ui_theme_preset")
                .selected_text(self.theme.preset.label())
                .show_ui(ui, |ui| {
                    for preset in ThemePreset::ALL {
                        if ui
                            .selectable_label(self.theme.preset == preset, preset.label())
                            .clicked()
                        {
                            self.theme.preset = preset;
                        }
                    }
                });
            if ui.button("Reset to default").clicked() {
                self.theme = UiTheme::default();
            }
        });

        card_frame(&self.theme).show(ui, |ui| {
            ui.strong("Colors");
            ui.add_space(4.0);
            let mut accent_edit = accent;
            let mut background_edit = background;
            ui.horizontal(|ui| {
                ui.label("Accent");
                if ui.color_edit_button_srgba(&mut accent_edit).changed() {
                    self.theme.preset = ThemePreset::Custom;
                    self.theme.accent = color_to_hex(accent_edit);
                }
                ui.label("Background");
                if ui.color_edit_button_srgba(&mut background_edit).changed() {
                    self.theme.preset = ThemePreset::Custom;
                    self.theme.background = color_to_hex(background_edit);
                }
            });
            let mut dark_edit = dark;
            if ui.checkbox(&mut dark_edit, "Dark theme").changed() {
                self.theme.preset = ThemePreset::Custom;
                self.theme.dark_mode = dark_edit;
            }
        });

        card_frame(&self.theme).show(ui, |ui| {
            ui.strong("Layout");
            ui.add_space(4.0);
            let mut scale = self.theme.scale;
            if ui
                .add(egui::Slider::new(&mut scale, 0.85..=1.25).text("UI scale"))
                .changed()
            {
                self.theme.scale = scale;
            }
            ui.small("Compact for small laptops, larger for high-DPI displays.");
        });

        card_frame(&self.theme).show(ui, |ui| {
            ui.strong("Preview");
            ui.add_space(6.0);
            let mut preview_volume = 65_u32;
            ui.horizontal(|ui| {
                let _ = ui.button("Primary button");
                let _ = ui.button("Secondary");
                ui.add(egui::Slider::new(&mut preview_volume, 0..=100).text("Volume"));
            });
            ui.label("Sample text uses the active theme colors.");
        });

        if self.theme != theme_before {
            self.theme.apply(ui.ctx());
            self.theme.save();
        }
    }
}
