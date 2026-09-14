use eframe::egui;

#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    pub background: egui::Color32,
    pub surface: egui::Color32,
    pub surface_light: egui::Color32,
    pub border: egui::Color32,
    pub primary: egui::Color32,
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
    // Chat chrome (borders, code blocks, badges, separators)
    pub bubble_border: egui::Color32,
    pub code_bg: egui::Color32,
    pub code_border: egui::Color32,
    pub code_text: egui::Color32,
    pub divider: egui::Color32,
    pub hover_bg: egui::Color32,
    pub badge_bg: egui::Color32,
    pub badge_text: egui::Color32,
}

impl Theme {
    pub fn from_name(name: &str) -> Self {
        match name {
            "light" => Self::light(),
            _ => Self::dark(),
        }
    }

    /// Dark theme: subtle, layered greys with a single blue accent.
    pub fn dark() -> Self {
        Theme {
            name: "dark".to_string(),
            background: egui::Color32::from_rgb(15, 16, 18),
            surface: egui::Color32::from_rgb(22, 23, 26),
            surface_light: egui::Color32::from_rgb(32, 33, 37),
            border: egui::Color32::from_rgb(48, 50, 56),
            // User avatar + selected items
            primary: egui::Color32::from_rgb(59, 130, 246),
            // High-contrast text for readability on dark backgrounds
            text_primary: egui::Color32::from_rgb(235, 238, 242),
            text_secondary: egui::Color32::from_rgb(150, 158, 170),
            text_dim: egui::Color32::from_rgb(108, 116, 128),
            success: egui::Color32::from_rgb(52, 199, 123),
            warning: egui::Color32::from_rgb(251, 191, 36),
            error: egui::Color32::from_rgb(239, 91, 91),
            accent: egui::Color32::from_rgb(96, 165, 250),
            selected_bg: egui::Color32::from_rgb(59, 130, 246),
            panel_bg: egui::Color32::from_rgb(22, 23, 26),
            // Chat bubble colors — user bubble keeps a saturated blue (white
            // text on it in both themes); AI/tool bubbles are quiet surfaces.
            user_bg: egui::Color32::from_rgb(37, 99, 235),
            ai_bg: egui::Color32::from_rgb(28, 29, 33),
            tool_bg: egui::Color32::from_rgb(26, 32, 28),
            // Chat chrome
            bubble_border: egui::Color32::from_rgb(52, 54, 60),
            code_bg: egui::Color32::from_rgb(12, 13, 15),
            code_border: egui::Color32::from_rgb(38, 40, 45),
            code_text: egui::Color32::from_rgb(206, 210, 218),
            divider: egui::Color32::from_rgb(40, 42, 47),
            hover_bg: egui::Color32::from_rgba_unmultiplied(255, 255, 255, 16),
            badge_bg: egui::Color32::from_rgb(35, 45, 66),
            badge_text: egui::Color32::from_rgb(147, 197, 253),
        }
    }

    /// Light theme: white cards on a soft grey background.
    pub fn light() -> Self {
        Theme {
            name: "light".to_string(),
            background: egui::Color32::from_rgb(243, 244, 246),
            surface: egui::Color32::from_rgb(255, 255, 255),
            surface_light: egui::Color32::from_rgb(236, 238, 240),
            border: egui::Color32::from_rgb(214, 217, 221),
            primary: egui::Color32::from_rgb(37, 99, 235),
            // High-contrast text for readability on light backgrounds
            text_primary: egui::Color32::from_rgb(24, 26, 30),
            text_secondary: egui::Color32::from_rgb(82, 90, 100),
            text_dim: egui::Color32::from_rgb(130, 138, 148),
            success: egui::Color32::from_rgb(21, 150, 76),
            warning: egui::Color32::from_rgb(202, 138, 4),
            error: egui::Color32::from_rgb(200, 45, 45),
            accent: egui::Color32::from_rgb(37, 99, 235),
            selected_bg: egui::Color32::from_rgb(37, 99, 235),
            panel_bg: egui::Color32::from_rgb(255, 255, 255),
            // Chat bubble colors
            user_bg: egui::Color32::from_rgb(37, 99, 235),
            ai_bg: egui::Color32::from_rgb(255, 255, 255),
            tool_bg: egui::Color32::from_rgb(238, 246, 235),
            // Chat chrome
            bubble_border: egui::Color32::from_rgb(218, 220, 224),
            code_bg: egui::Color32::from_rgb(244, 245, 247),
            code_border: egui::Color32::from_rgb(220, 222, 226),
            code_text: egui::Color32::from_rgb(40, 44, 52),
            divider: egui::Color32::from_rgb(214, 217, 221),
            hover_bg: egui::Color32::from_rgba_unmultiplied(0, 0, 0, 16),
            badge_bg: egui::Color32::from_rgb(222, 232, 250),
            badge_text: egui::Color32::from_rgb(30, 80, 210),
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
