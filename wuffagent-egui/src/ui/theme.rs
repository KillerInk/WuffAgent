use eframe::egui;

#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    pub background: egui::Color32,
    pub surface: egui::Color32,
    pub surface_light: egui::Color32,
    pub border: egui::Color32,
    pub primary: egui::Color32,
    pub primary_light: egui::Color32,
    pub text_primary: egui::Color32,
    pub text_secondary: egui::Color32,
    pub text_dim: egui::Color32,
    pub success: egui::Color32,
    pub warning: egui::Color32,
    pub error: egui::Color32,
    pub accent: egui::Color32,
    pub selected_bg: egui::Color32,
    pub panel_bg: egui::Color32,
    // Chat bubble colors
    pub user_bg: egui::Color32,
    pub ai_bg: egui::Color32,
    pub tool_bg: egui::Color32,
}

impl Theme {
    pub fn from_name(name: &str) -> Self {
        match name {
            "light" => Self::light(),
            _ => Self::dark(),
        }
    }

    /// Dark theme with high-contrast, readable colors
    pub fn dark() -> Self {
        Theme {
            name: "dark".to_string(),
            background: egui::Color32::from_rgb(17, 17, 17),
            surface: egui::Color32::from_rgb(22, 22, 22),
            surface_light: egui::Color32::from_rgb(35, 35, 35),
            border: egui::Color32::from_rgb(50, 50, 50),
            // User avatar + selected items
            primary: egui::Color32::from_rgb(59, 130, 246),
            primary_light: egui::Color32::from_rgb(96, 165, 250),
            // High-contrast text for readability on dark backgrounds
            text_primary: egui::Color32::from_rgb(243, 244, 246),
            text_secondary: egui::Color32::from_rgb(156, 163, 175),
            text_dim: egui::Color32::from_rgb(120, 125, 135),
            success: egui::Color32::from_rgb(34, 197, 94),
            warning: egui::Color32::from_rgb(251, 191, 36),
            error: egui::Color32::from_rgb(239, 68, 68),
            accent: egui::Color32::from_rgb(96, 165, 250),
            selected_bg: egui::Color32::from_rgb(59, 130, 246),
            panel_bg: egui::Color32::from_rgb(22, 22, 22),
            // Chat bubble colors
            user_bg: egui::Color32::from_rgb(29, 58, 102),
            ai_bg: egui::Color32::from_rgb(35, 35, 35),
            tool_bg: egui::Color32::from_rgb(20, 40, 20),
        }
    }

    /// Light theme with high-contrast, readable colors
    pub fn light() -> Self {
        Theme {
            name: "light".to_string(),
            background: egui::Color32::from_rgb(240, 240, 240),
            surface: egui::Color32::from_rgb(255, 255, 255),
            surface_light: egui::Color32::from_rgb(235, 235, 235),
            border: egui::Color32::from_rgb(200, 200, 200),
            primary: egui::Color32::from_rgb(37, 99, 235),
            primary_light: egui::Color32::from_rgb(79, 148, 255),
            // High-contrast text for readability on light backgrounds
            text_primary: egui::Color32::from_rgb(17, 17, 17),
            text_secondary: egui::Color32::from_rgb(80, 80, 80),
            text_dim: egui::Color32::from_rgb(115, 115, 115),
            success: egui::Color32::from_rgb(22, 163, 74),
            warning: egui::Color32::from_rgb(234, 179, 8),
            error: egui::Color32::from_rgb(220, 38, 38),
            accent: egui::Color32::from_rgb(79, 148, 255),
            selected_bg: egui::Color32::from_rgb(37, 99, 235),
            panel_bg: egui::Color32::from_rgb(255, 255, 255),
            // Chat bubble colors
            user_bg: egui::Color32::from_rgb(59, 130, 246),
            ai_bg: egui::Color32::from_rgb(240, 240, 240),
            tool_bg: egui::Color32::from_rgb(220, 237, 200),
        }
    }

    /// Apply theme to the egui context using egui's built-in defaults
    pub fn apply(&self, ctx: &egui::Context) {
        let visuals = match self.name.as_str() {
            "light" => egui::Visuals::light(),
            _ => egui::Visuals::dark(),
        };
        ctx.set_visuals(visuals);
    }
}
