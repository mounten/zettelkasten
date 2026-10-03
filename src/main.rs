#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod assets;
mod model;
mod profile;
mod store;

use gpui_kit::component::highlighter::LanguageRegistry;
use gpui_kit::component::{Theme, ThemeMode, TitleBar};
use gpui_kit::*;

/// tree-sitter-cpp's highlight query only covers C++ additions and is meant
/// to be layered on top of the C query, so combine them for C++.
fn register_cpp_highlights() {
    let registry = LanguageRegistry::singleton();
    if let (Some(c), Some(mut cpp)) = (registry.language("c"), registry.language("cpp")) {
        cpp.highlights = format!("{}\n{}", cpp.highlights, c.highlights).into();
        registry.register("cpp", &cpp);
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(assets::AppAssets)
        .run(|cx| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            register_cpp_highlights();
            app::init(cx);

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1320.), px(860.)),
                    cx,
                ))),
                window_min_size: Some(size(px(720.), px(480.))),
                ..TitleBar::window_options()
            };

            gpui_kit::open_window(options, cx, |window, cx| {
                window.set_window_title("Zettelkasten");
                cx.new(|cx| app::ZettelApp::new(window, cx))
            })
            .expect("failed to open window");
            cx.activate(true);
        });
}
