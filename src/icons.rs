//! Phosphor icon font, vendored so it works with our egui version (the
//! published `egui-phosphor` crate targets an older egui). The font is added as
//! a fallback so normal text still uses the default font and only these private
//! codepoints resolve to icons.

use std::sync::Arc;

use eframe::egui;

const FONT: &[u8] = include_bytes!("../assets/Phosphor.ttf");

// Phosphor "regular" codepoints (see https://phosphoricons.com).
pub const PLUS: &str = "\u{E3D4}";
pub const ARROWS_CLOCKWISE: &str = "\u{E094}";
pub const MAGNIFYING_GLASS: &str = "\u{E30C}";
pub const SKIP_BACK: &str = "\u{E5A4}";
pub const SKIP_FORWARD: &str = "\u{E5A6}";
pub const ARROW_ARC_LEFT: &str = "\u{E014}";
pub const ARROW_ARC_RIGHT: &str = "\u{E016}";
pub const PLAY: &str = "\u{E3D0}";
pub const PAUSE: &str = "\u{E39E}";
pub const MOON: &str = "\u{E330}";
pub const BOOKMARK: &str = "\u{E0EA}";
pub const ARROW_U_DOWN_LEFT: &str = "\u{E07E}";
pub const TRASH: &str = "\u{E4A6}";
pub const ARROW_LEFT: &str = "\u{E058}";
pub const CLOCK: &str = "\u{E19A}";
pub const FILE_AUDIO: &str = "\u{EA20}";
// Library view modes (Dolphin-style Icons / Compact / Details).
pub const SQUARES_FOUR: &str = "\u{E464}";
pub const ROWS: &str = "\u{E5A2}";
pub const LIST_BULLETS: &str = "\u{E2F2}";
// Browsing categories.
pub const BOOKS: &str = "\u{E758}";
pub const USERS: &str = "\u{E4D6}";
pub const MICROPHONE: &str = "\u{E75C}"; // microphone-stage (reader/narrator)
pub const CLOCK_COUNTER_CLOCKWISE: &str = "\u{E1A0}";
// My History + misc.
pub const STAR: &str = "\u{E46A}";
pub const X: &str = "\u{E4F6}";
pub const PLUS_CIRCLE: &str = "\u{E3D6}";

/// Register the Phosphor font as a fallback on both font families.
pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("phosphor".to_owned(), Arc::new(egui::FontData::from_static(FONT)));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("phosphor".to_owned());
    }
    ctx.set_fonts(fonts);
}
