//! Font registration (docs/UI_DESIGN.md §7.2). All fonts are embedded.

use egui::{FontData, FontDefinitions, FontFamily, FontTweak};
use std::sync::Arc;

pub fn install(ctx: &egui::Context) {
    let mut f = FontDefinitions::empty();
    let mut add = |name: &str, bytes: &'static [u8], tweak: Option<FontTweak>| {
        let mut d = FontData::from_static(bytes);
        if let Some(t) = tweak {
            d = d.tweak(t);
        }
        f.font_data.insert(name.to_owned(), Arc::new(d));
    };
    add("inter-400", include_bytes!("../assets/fonts/Inter-Regular.ttf"), None);
    add("inter-500", include_bytes!("../assets/fonts/Inter-Medium.ttf"), None);
    add("inter-600", include_bytes!("../assets/fonts/Inter-SemiBold.ttf"), None);
    add("tnum-400", include_bytes!("../assets/fonts/InterTnum-Regular.ttf"), None);
    add("tnum-500", include_bytes!("../assets/fonts/InterTnum-Medium.ttf"), None);
    add("mono-400", include_bytes!("../assets/fonts/GeistMono-Regular.ttf"), None);
    add("mono-500", include_bytes!("../assets/fonts/GeistMono-Medium.ttf"), None);
    // Phosphor: slightly larger and nudged down so 16 px icons sit centred on 13 px text.
    let icon_tweak = FontTweak {
        scale: 1.15,
        y_offset_factor: 0.10,
        ..Default::default()
    };
    add("ph", include_bytes!("../assets/fonts/Phosphor.ttf"), Some(icon_tweak.clone()));
    add("ph-fill", include_bytes!("../assets/fonts/Phosphor-Fill.ttf"), Some(icon_tweak));

    let mut fam = |family: FontFamily, names: &[&str]| {
        f.families.insert(family, names.iter().map(|s| s.to_string()).collect());
    };
    // The icon font goes FIRST (Inter has PUA glyphs that collide with Phosphor's).
    // Safe only because assets/fonts/Phosphor.ttf is subset to its PUA codepoints.
    fam(FontFamily::Proportional, &["ph", "inter-400"]);
    fam(FontFamily::Name("medium".into()), &["ph", "inter-500"]);
    fam(FontFamily::Name("semibold".into()), &["ph", "inter-600"]);
    fam(FontFamily::Name("tnum".into()), &["ph", "tnum-400", "inter-400"]);
    fam(FontFamily::Name("tnum-medium".into()), &["ph", "tnum-500", "inter-500"]);
    fam(FontFamily::Monospace, &["ph", "mono-400", "inter-400"]);
    fam(FontFamily::Name("mono-medium".into()), &["ph", "mono-500", "inter-500"]);
    fam(FontFamily::Name("icons".into()), &["ph", "inter-400"]);
    fam(FontFamily::Name("icons-fill".into()), &["ph-fill", "inter-400"]);
    ctx.set_fonts(f);
}
