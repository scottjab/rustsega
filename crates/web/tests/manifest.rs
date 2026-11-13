//! The installable-PWA assets (M7-18, #151).
//!
//! Native tests, no browser: they read the files in `crates/web/assets` that
//! `trunk` copies into `dist` and check the two things Chromium reads before it
//! offers "Install app" - a web app manifest with a name, a start URL, a display
//! mode and icons that are really there, and an icon file that is an SVG.
//!
//! There is deliberately no service worker test: offline support is out of
//! scope for this app, and modern Chromium does not need one to install.

use std::path::{Path, PathBuf};

/// The directory the icons and the manifest live in, next to `index.html`.
fn assets_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
}

/// The manifest as JSON. Parsing is the first thing the test does, so a broken
/// manifest fails here with the parser's message rather than later with
/// "field missing".
fn manifest() -> serde_json::Value {
    let path = assets_dir().join("manifest.webmanifest");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()))
}

/// The dark `--bg` token, read out of `style.css` rather than repeated here.
///
/// `theme_color` is that colour: the manifest's colour has to match the colour
/// the page paints with or the installed app gets a mismatched frame. The first
/// `--bg:` in the file is the one in the default `:root` block, before the
/// light-theme overrides, which is the one `color-scheme: dark` selects.
fn css_bg() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("style.css");
    let css = std::fs::read_to_string(&path).expect("style.css is readable");
    let line = css
        .lines()
        .find(|line| line.trim_start().starts_with("--bg:"))
        .expect("style.css declares --bg");
    let value = line
        .split(':')
        .nth(1)
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_string())
        .expect("--bg is a colour");
    assert!(
        value.starts_with('#'),
        "--bg should be a hex colour, found {value:?}"
    );
    value
}

#[test]
fn manifest_declares_the_install_fields() {
    let manifest = manifest();
    let bg = css_bg();

    // The name the OS shows. `app_title` is the one spelling of it in the app.
    assert_eq!(manifest["name"].as_str(), Some(web::app_title()));
    assert_eq!(manifest["short_name"].as_str(), Some(web::app_title()));

    // Install needs a start URL and a display mode that is not `browser`.
    assert_eq!(manifest["start_url"].as_str(), Some("/"));
    assert_eq!(manifest["display"].as_str(), Some("standalone"));

    // Same colour as the page background.
    assert_eq!(manifest["theme_color"].as_str(), Some(bg.as_str()));
    assert_eq!(manifest["background_color"].as_str(), Some(bg.as_str()));

    // Every icon the manifest lists has to exist next to it: a manifest that
    // names a missing icon is not installable, and the path is the only thing
    // that ties the two together.
    let icons = manifest["icons"].as_array().expect("manifest lists icons");
    assert!(!icons.is_empty(), "manifest must list at least one icon");
    for icon in icons {
        let src = icon["src"].as_str().expect("icon has a src");
        let file = assets_dir().join(src.trim_start_matches('/'));
        assert!(
            file.is_file(),
            "manifest icon {src} is missing: {}",
            file.display()
        );
        assert!(
            icon["sizes"].as_str().is_some() && icon["type"].as_str().is_some(),
            "icon {src} needs sizes and type"
        );
    }
}

#[test]
fn icon_svg_is_a_viewbox_svg() {
    let path = assets_dir().join("icon.svg");
    let svg = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()));
    let trimmed = svg.trim_start();
    assert!(
        trimmed.starts_with("<svg"),
        "icon.svg must start with the <svg> element, found {:?}",
        trimmed.chars().take(40).collect::<String>()
    );
    assert!(
        trimmed.contains("viewBox"),
        "icon.svg must declare a viewBox so it scales to any icon size"
    );
}
