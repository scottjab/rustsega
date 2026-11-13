//! The colour theme setting, and the `data-theme` attribute it drives.

use std::str::FromStr;

/// Which colour theme the shell should use.
///
/// `System` means "no opinion": no `data-theme` attribute is written, and
/// `style.css` follows `prefers-color-scheme` instead. That is why
/// [`Theme::as_attr`] returns `None` for it rather than `"system"`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    /// The default: follow `prefers-color-scheme`.
    #[default]
    System,
    Dark,
    Light,
}

impl Theme {
    /// Every theme, in the order the settings select should list them.
    pub const ALL: [Theme; 3] = [Theme::System, Theme::Dark, Theme::Light];

    /// The `data-theme` value this theme writes on `<html>`.
    pub fn as_attr(self) -> Option<&'static str> {
        match self {
            Theme::System => None,
            Theme::Dark => Some("dark"),
            Theme::Light => Some("light"),
        }
    }

    /// The human-readable label for this theme in the settings drawer.
    pub fn label(self) -> &'static str {
        match self {
            Theme::System => "System",
            Theme::Dark => "Dark",
            Theme::Light => "Light",
        }
    }

    /// Writes `data-theme` on `<html>`, or removes it for [`Theme::System`].
    ///
    /// A no-op off the browser: natively the shell is only ever tested as data.
    pub fn apply_to_document(self) {
        #[cfg(target_arch = "wasm32")]
        {
            let Some(root) = web_sys::window()
                .and_then(|window| window.document())
                .and_then(|document| document.document_element())
            else {
                return;
            };
            match self.as_attr() {
                Some(value) => {
                    let _ = root.set_attribute("data-theme", value);
                }
                None => {
                    let _ = root.remove_attribute("data-theme");
                }
            }
        }
    }
}

/// The lower-case token used by [`FromStr`] and `Display`: `system`, `dark`,
/// `light`. M7-10 (#143) stores this string in `localStorage`.
impl core::fmt::Display for Theme {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let text = match self {
            Theme::System => "system",
            Theme::Dark => "dark",
            Theme::Light => "light",
        };
        f.write_str(text)
    }
}

/// Parses the tokens written by [`Display`], ignoring case and surrounding
/// space. Anything else is an error, so a corrupt stored value falls back to
/// [`Theme::default`] instead of guessing.
impl FromStr for Theme {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "system" => Ok(Theme::System),
            "dark" => Ok(Theme::Dark),
            "light" => Ok(Theme::Light),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_has_no_data_theme_attribute() {
        assert_eq!(Theme::System.as_attr(), None);
    }

    #[test]
    fn dark_and_light_name_their_data_theme_attribute() {
        assert_eq!(Theme::Dark.as_attr(), Some("dark"));
        assert_eq!(Theme::Light.as_attr(), Some("light"));
    }

    #[test]
    fn theme_round_trips_through_display_and_from_str() {
        for theme in Theme::ALL {
            let text = theme.to_string();
            let parsed: Theme = text.parse().expect("Display writes a parseable token");
            assert_eq!(parsed, theme);
        }
    }

    #[test]
    fn from_str_accepts_each_token_case_insensitively() {
        assert_eq!("system".parse(), Ok(Theme::System));
        assert_eq!("DARK".parse(), Ok(Theme::Dark));
        assert_eq!(" light ".parse(), Ok(Theme::Light));
    }

    #[test]
    fn from_str_rejects_anything_else() {
        assert!("".parse::<Theme>().is_err());
        assert!("sepia".parse::<Theme>().is_err());
    }

    #[test]
    fn default_theme_follows_the_system() {
        assert_eq!(Theme::default(), Theme::System);
    }
}
