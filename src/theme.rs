use clap::ValueEnum;

#[derive(Clone, Copy, Debug, PartialEq, Default, ValueEnum)]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

pub struct ThemeColors {
    pub bg: &'static str,
    pub node_default_fill: &'static str,
    pub node_default_border: &'static str,
    pub node_default_font: &'static str,
    pub edge_color: &'static str,
    pub plain_fill: &'static str,
    pub plain_border: &'static str,
    pub plain_font: &'static str,
    pub plain_current_fill: &'static str,
    pub plain_current_border: &'static str,
    pub plain_current_font: &'static str,
    pub tag_fill: &'static str,
    pub tag_border: &'static str,
    pub tag_font: &'static str,
    pub tag_penwidth: f32,
    pub other_fill: &'static str,
    pub other_border: &'static str,
    pub other_font: &'static str,
    pub cell_border_color: &'static str,
    pub cell_bgcolor: &'static str,
    pub cell_font_color: &'static str,
}

impl Theme {
    pub fn colors(self) -> ThemeColors {
        match self {
            Theme::Dark => ThemeColors {
                bg: "#0F172A",
                node_default_fill: "#1E293B",
                node_default_border: "#475569",
                node_default_font: "#E2E8F0",
                edge_color: "#475569",
                plain_fill: "#1E293B",
                plain_border: "#475569",
                plain_font: "#94A3B8",
                plain_current_fill: "#334155",
                plain_current_border: "#F8FAFC",
                plain_current_font: "#F8FAFC",
                tag_fill: "#0F172A",
                tag_border: "#94A3B8",
                tag_font: "#CBD5E1",
                tag_penwidth: 1.0,
                other_fill: "#334155",
                other_border: "#64748B",
                other_font: "#CBD5E1",
                cell_border_color: "#334155",
                cell_bgcolor: "#1E293B",
                cell_font_color: "#94A3B8",
            },
            Theme::Light => ThemeColors {
                bg: "#F8FAFC",
                node_default_fill: "#FFFFFF",
                node_default_border: "#E2E8F0",
                node_default_font: "#334155",
                edge_color: "#CBD5E1",
                plain_fill: "#FFFFFF",
                plain_border: "#E2E8F0",
                plain_font: "#64748B",
                plain_current_fill: "#F1F5F9",
                plain_current_border: "#0F172A",
                plain_current_font: "#0F172A",
                tag_fill: "transparent",
                tag_border: "#94A3B8",
                tag_font: "#475569",
                tag_penwidth: 1.5,
                other_fill: "#F8FAFC",
                other_border: "#64748B",
                other_font: "#334155",
                cell_border_color: "#E2E8F0",
                cell_bgcolor: "#F8FAFC",
                cell_font_color: "#64748B",
            },
        }
    }

    /// Returns (fill, border, font) for a given branch name.
    pub fn branch_colors(self, branch_name: &str) -> (&'static str, &'static str, &'static str) {
        let is_main = branch_name == "main" || branch_name == "master";
        let is_develop = branch_name == "develop";
        let is_feature = branch_name.starts_with("feature/");
        let is_release = branch_name.starts_with("release/");
        let is_hotfix = branch_name.starts_with("hotfix/");

        match self {
            Theme::Dark => {
                if is_main {
                    ("#059669", "#34D399", "#F0FDF4")
                } else if is_develop {
                    ("#7C3AED", "#A78BFA", "#F5F3FF")
                } else if is_feature {
                    ("#2563EB", "#60A5FA", "#EFF6FF")
                } else if is_release {
                    ("#D97706", "#FBBF24", "#FFFBEB")
                } else if is_hotfix {
                    ("#DC2626", "#F87171", "#FEF2F2")
                } else {
                    ("#334155", "#60A5FA", "#E2E8F0")
                }
            }
            Theme::Light => {
                if is_main {
                    ("#ECFDF5", "#10B981", "#065F46")
                } else if is_develop {
                    ("#F3E8FF", "#8B5CF6", "#5B21B6")
                } else if is_feature {
                    ("#EFF6FF", "#3B82F6", "#1E40AF")
                } else if is_release {
                    ("#FFF7ED", "#F59E0B", "#92400E")
                } else if is_hotfix {
                    ("#FEF2F2", "#EF4444", "#7F1D1D")
                } else {
                    ("#F8FAFC", "#64748B", "#334155")
                }
            }
        }
    }
}
