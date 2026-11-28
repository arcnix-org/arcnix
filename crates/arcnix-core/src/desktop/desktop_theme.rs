//! Desktop theme presets and appearance overrides.
use std::collections::BTreeMap;
use std::fmt;

use anyhow::{Result, ensure};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

use crate::Theme as Mode;
use crate::desktop_customization::{Wallpaper, validate_image_path};

/// Built-in WebP wallpapers served by the desktop under each theme asset name.
#[cfg(feature = "bundled-wallpapers")]
pub const BUNDLED_WALLPAPERS: &[(&str, &[u8])] = &[
    (
        "prism-light",
        include_bytes!("../../assets/wallpapers/prism-light.webp"),
    ),
    (
        "prism-dark",
        include_bytes!("../../assets/wallpapers/prism-dark.webp"),
    ),
    (
        "valley-light",
        include_bytes!("../../assets/wallpapers/valley-light.webp"),
    ),
    (
        "valley-dark",
        include_bytes!("../../assets/wallpapers/valley-dark.webp"),
    ),
    (
        "tide-light",
        include_bytes!("../../assets/wallpapers/tide-light.webp"),
    ),
    (
        "tide-dark",
        include_bytes!("../../assets/wallpapers/tide-dark.webp"),
    ),
    (
        "clay-light",
        include_bytes!("../../assets/wallpapers/clay-light.webp"),
    ),
    (
        "clay-dark",
        include_bytes!("../../assets/wallpapers/clay-dark.webp"),
    ),
    (
        "meadow-light",
        include_bytes!("../../assets/wallpapers/meadow-light.webp"),
    ),
    (
        "meadow-dark",
        include_bytes!("../../assets/wallpapers/meadow-dark.webp"),
    ),
    (
        "fjord",
        include_bytes!("../../assets/wallpapers/fjord.webp"),
    ),
    (
        "fjord-daylight",
        include_bytes!("../../assets/wallpapers/fjord-daylight.webp"),
    ),
    (
        "aurora-twilight",
        include_bytes!("../../assets/wallpapers/aurora-twilight.webp"),
    ),
    (
        "aurora-night",
        include_bytes!("../../assets/wallpapers/aurora-night.webp"),
    ),
    (
        "seaside-sunrise",
        include_bytes!("../../assets/wallpapers/seaside-sunrise.webp"),
    ),
    (
        "seaside-moonrise",
        include_bytes!("../../assets/wallpapers/seaside-moonrise.webp"),
    ),
];

pub(crate) fn appearance_value<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(
    deserializer: D,
) -> Result<T, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

pub(crate) fn wallpaper_value<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Wallpaper>, D::Error> {
    let wallpaper: Option<Wallpaper> = appearance_value(deserializer)?;
    Ok(wallpaper.filter(|wallpaper| match wallpaper {
        Wallpaper::Remote(path) => validate_image_path(path).is_ok(),
        _ => true,
    }))
}

fn appearance_colors<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<ColorRole, Rgb>, D::Error> {
    let entries: BTreeMap<String, serde_json::Value> = appearance_value(deserializer)?;
    Ok(entries
        .into_iter()
        .filter_map(|(role, color)| {
            let role = serde_json::from_value(serde_json::Value::String(role)).ok()?;
            let color = serde_json::from_value(color).ok()?;
            Some((role, color))
        })
        .collect())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IconFamily {
    #[default]
    Line,
    #[serde(alias = "solid")]
    Color,
}

impl IconFamily {
    pub const ALL: [Self; 2] = [Self::Line, Self::Color];

    pub fn label(self) -> &'static str {
        match self {
            Self::Line => "Line icons",
            Self::Color => "Color icons",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::Line => "line",
            Self::Color => "color",
        }
    }

    pub fn supports_tile_background(self) -> bool {
        self == Self::Line
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IconStyle {
    #[default]
    Outline,
    Filled,
}

impl IconStyle {
    pub const ALL: [Self; 2] = [Self::Outline, Self::Filled];

    pub fn label(self) -> &'static str {
        match self {
            Self::Outline => "Gradient",
            Self::Filled => "Solid color",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::Outline => "outline",
            Self::Filled => "filled",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    #[default]
    #[serde(alias = "mist", alias = "silk")]
    Prism,
    Valley,
    Tide,
    Clay,
    Meadow,
    Fjord,
    Aurora,
    #[serde(alias = "terracotta", alias = "afterglow")]
    Seaside,
}

impl Preset {
    pub const ALL: [Self; 8] = [
        Self::Prism,
        Self::Valley,
        Self::Tide,
        Self::Clay,
        Self::Meadow,
        Self::Fjord,
        Self::Aurora,
        Self::Seaside,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Prism => "Prism",
            Self::Valley => "Valley",
            Self::Tide => "Tide",
            Self::Clay => "Clay",
            Self::Meadow => "Meadow",
            Self::Fjord => "Fjord",
            Self::Aurora => "Aurora",
            Self::Seaside => "Seaside",
        }
    }

    pub fn theme(self) -> Theme {
        let (wallpaper, light_accent, dark_accent, icons) = match self {
            Self::Prism => (
                Wallpaper::Prism,
                [79, 108, 132],
                [119, 167, 222],
                IconStyle::Outline,
            ),
            Self::Valley => (
                Wallpaper::Valley,
                [137, 99, 201],
                [190, 145, 238],
                IconStyle::Outline,
            ),
            Self::Tide => (
                Wallpaper::Tide,
                [173, 116, 46],
                [226, 161, 81],
                IconStyle::Outline,
            ),
            Self::Clay => (
                Wallpaper::Clay,
                [160, 95, 99],
                [198, 143, 155],
                IconStyle::Outline,
            ),
            Self::Meadow => (
                Wallpaper::Meadow,
                [100, 118, 70],
                [143, 168, 122],
                IconStyle::Outline,
            ),
            Self::Fjord => (
                Wallpaper::Fjord,
                [84, 140, 163],
                [131, 175, 197],
                IconStyle::Outline,
            ),
            Self::Aurora => (
                Wallpaper::Aurora,
                [134, 127, 174],
                [136, 195, 166],
                IconStyle::Outline,
            ),
            Self::Seaside => (
                Wallpaper::Seaside,
                [180, 121, 104],
                [207, 155, 132],
                IconStyle::Filled,
            ),
        };
        let style = |accent| StyleOverrides {
            accent: Some(Rgb(accent)),
            ..StyleOverrides::default()
        };
        Theme {
            preset: self,
            wallpaper,
            mode: Mode::System,
            accent: None,
            icons,
            icon_family: IconFamily::default(),
            colors: BTreeMap::new(),
            light: style(light_accent),
            dark: style(dark_accent),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorRole {
    Background,
    Surface,
    Input,
    Border,
    Text,
    MutedText,
    Accent,
    Error,
}

impl ColorRole {
    pub const ALL: [Self; 8] = [
        Self::Background,
        Self::Surface,
        Self::Input,
        Self::Border,
        Self::Text,
        Self::MutedText,
        Self::Accent,
        Self::Error,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Background => "Background color",
            Self::Surface => "Panel color",
            Self::Input => "Input color",
            Self::Border => "Border color",
            Self::Text => "Text color",
            Self::MutedText => "Secondary text color",
            Self::Accent => "Accent color",
            Self::Error => "Error color",
        }
    }

    pub fn variable(self) -> &'static str {
        match self {
            Self::Background => "--bg",
            Self::Surface => "--surface",
            Self::Input => "--input",
            Self::Border => "--line",
            Self::Text => "--text",
            Self::MutedText => "--muted",
            Self::Accent => "--accent",
            Self::Error => "--danger",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Rgb(pub [u8; 3]);

impl Rgb {
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim().strip_prefix('#')?;
        if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |start| u8::from_str_radix(&value[start..start + 2], 16).ok();
        Some(Self([byte(0)?, byte(2)?, byte(4)?]))
    }

    /// Mix another color into this one by an integer percentage.
    pub fn mix(self, other: Self, percent: u16) -> Self {
        let percent = percent.min(100);
        Self(std::array::from_fn(|i| {
            let left = u16::from(self.0[i]);
            let right = u16::from(other.0[i]);
            ((left * (100 - percent) + right * percent + 50) / 100) as u8
        }))
    }

    pub fn contrasting_label(self, surface: Self, text: Self) -> Self {
        let background = self.luminance();
        for label in [surface, text] {
            if contrast(label.luminance(), background) >= 4.5 {
                return label;
            }
        }
        if contrast(0.0, background) >= contrast(1.0, background) {
            Self([0; 3])
        } else {
            Self([255; 3])
        }
    }

    fn luminance(self) -> f64 {
        let [red, green, blue] = self.0.map(|channel| {
            let value = f64::from(channel) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        });
        0.2126 * red + 0.7152 * green + 0.0722 * blue
    }
}

fn contrast(left: f64, right: f64) -> f64 {
    (left.max(right) + 0.05) / (left.min(right) + 0.05)
}

impl fmt::Display for Rgb {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [r, g, b] = self.0;
        write!(formatter, "#{r:02x}{g:02x}{b:02x}")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    preset: Preset,
    pub wallpaper: Wallpaper,
    pub mode: Mode,
    pub icons: IconStyle,
    pub icon_family: IconFamily,
    pub accent: Option<Rgb>,
    pub colors: BTreeMap<ColorRole, Rgb>,
    pub light: StyleOverrides,
    pub dark: StyleOverrides,
}

impl Theme {
    fn style(&self, dark: bool) -> &StyleOverrides {
        if dark { &self.dark } else { &self.light }
    }

    pub fn background(&self, dark: bool) -> &Wallpaper {
        self.style(dark)
            .wallpaper
            .as_ref()
            .unwrap_or(&self.wallpaper)
    }

    pub fn main_color(&self, dark: bool) -> Option<Rgb> {
        self.style(dark).accent.or(self.accent)
    }

    /// The selected color for swatches and solid fills. Accent text uses the readable palette
    /// shade.
    pub fn accent_fill(&self, dark: bool) -> Rgb {
        self.style(dark)
            .colors
            .get(&ColorRole::Accent)
            .or_else(|| self.colors.get(&ColorRole::Accent))
            .copied()
            .or_else(|| self.main_color(dark))
            .unwrap_or_else(|| Self::default_accent(dark))
    }

    pub fn has_palette(&self, dark: bool) -> bool {
        self.main_color(dark).is_some()
            || !self.colors.is_empty()
            || !self.style(dark).colors.is_empty()
    }

    pub fn default_accent(dark: bool) -> Rgb {
        if dark {
            Rgb([120, 173, 244])
        } else {
            Rgb([61, 115, 186])
        }
    }

    pub fn palette(&self, dark: bool) -> BTreeMap<ColorRole, Rgb> {
        use ColorRole::*;
        let (base, surface, input, border, text, muted, error) = if dark {
            (
                [17, 25, 35],
                [23, 33, 46],
                [17, 27, 40],
                [41, 54, 71],
                [226, 234, 244],
                [146, 163, 185],
                [240, 148, 148],
            )
        } else if self.preset == Preset::Prism {
            (
                [222, 225, 227],
                [234, 236, 237],
                [240, 242, 243],
                [191, 198, 203],
                [45, 55, 63],
                [83, 97, 108],
                [163, 62, 70],
            )
        } else {
            (
                [245, 247, 251],
                [255, 255, 255],
                [255, 255, 255],
                [223, 229, 239],
                [35, 50, 72],
                [104, 122, 146],
                [178, 66, 75],
            )
        };
        let main = self.main_color(dark);
        // Build tinted neutrals from the base color's luma so a warm accent
        // gives warm grays even when the application's base palette is blue.
        let tint = |color: [u8; 3], percent| {
            main.map_or(Rgb(color), |accent| {
                let [r, g, b] = color.map(u32::from);
                let gray = ((21 * r + 72 * g + 7 * b + 50) / 100) as u8;
                Rgb([gray; 3]).mix(accent, percent)
            })
        };
        let background = tint(base, 7);
        let surface = tint(surface, 3);
        let input = tint(input, 2);
        let text = tint(text, 6);
        let raised = surface.mix(text, 7);
        let selection_luminance = background.mix(self.accent_fill(dark), 20).luminance();
        let luminances = [background, surface, input, raised].map(Rgb::luminance);
        let ink = if dark { Rgb([255; 3]) } else { Rgb([0; 3]) };
        let readable = |color: Rgb, accent: bool| {
            (0..=100)
                .map(|percent| color.mix(ink, percent))
                .find(|candidate| {
                    let luminance = candidate.luminance();
                    luminances
                        .iter()
                        .all(|&base| contrast(base, luminance) >= 4.5)
                        && (!accent || contrast(selection_luminance, luminance) >= 4.5)
                })
                .unwrap_or(ink)
        };
        let mut colors = BTreeMap::from([
            (Background, background),
            (Surface, surface),
            (Input, input),
            (Border, tint(border, 12)),
            (Text, text),
            (MutedText, readable(tint(muted, 10), false)),
            (
                Accent,
                readable(main.unwrap_or_else(|| Self::default_accent(dark)), true),
            ),
            (Error, readable(Rgb(error), false)),
        ]);
        colors.extend(&self.colors);
        colors.extend(&self.style(dark).colors);
        colors
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StyleOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "wallpaper_value")]
    pub wallpaper: Option<Wallpaper>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "appearance_value")]
    pub accent: Option<Rgb>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    #[serde(deserialize_with = "appearance_colors")]
    pub colors: BTreeMap<ColorRole, Rgb>,
}

impl StyleOverrides {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Overrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "wallpaper_value")]
    pub wallpaper: Option<Wallpaper>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "appearance_value")]
    pub mode: Option<Mode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "appearance_value")]
    pub icons: Option<IconStyle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "appearance_value")]
    pub icon_family: Option<IconFamily>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "appearance_value")]
    pub accent: Option<Rgb>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    #[serde(deserialize_with = "appearance_colors")]
    pub colors: BTreeMap<ColorRole, Rgb>,
    #[serde(skip_serializing_if = "StyleOverrides::is_empty")]
    pub light: StyleOverrides,
    #[serde(skip_serializing_if = "StyleOverrides::is_empty")]
    pub dark: StyleOverrides,
}

impl Overrides {
    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub(crate) fn validate(&self) -> Result<()> {
        for wallpaper in [&self.wallpaper, &self.light.wallpaper, &self.dark.wallpaper] {
            if let Some(Wallpaper::Remote(path)) = wallpaper {
                validate_image_path(path)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomTheme {
    #[serde(deserialize_with = "appearance_value")]
    pub base: Preset,
    pub overrides: Overrides,
}

impl CustomTheme {
    pub fn resolve(&self) -> Theme {
        let mut theme = self.base.theme();
        let overrides = &self.overrides;
        theme.mode = overrides.mode.unwrap_or(theme.mode);
        theme.icons = overrides.icons.unwrap_or(theme.icons);
        theme.icon_family = overrides.icon_family.unwrap_or(theme.icon_family);
        if let Some(wallpaper) = &overrides.wallpaper {
            theme.wallpaper = wallpaper.clone();
            theme.light.wallpaper = None;
            theme.dark.wallpaper = None;
        }
        if let Some(accent) = overrides.accent {
            theme.accent = Some(accent);
            theme.light.accent = None;
            theme.dark.accent = None;
        }
        theme.colors.extend(&overrides.colors);
        for (style, values) in [
            (&mut theme.light, &overrides.light),
            (&mut theme.dark, &overrides.dark),
        ] {
            for role in overrides.colors.keys() {
                style.colors.remove(role);
            }
            if let Some(wallpaper) = &values.wallpaper {
                style.wallpaper = Some(wallpaper.clone());
            }
            style.accent = values.accent.or(style.accent);
            style.colors.extend(&values.colors);
        }
        theme
    }

    fn validate(&self) -> Result<()> {
        self.overrides.validate()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Selection {
    #[default]
    Legacy,
    Preset(Preset),
    Custom(Box<CustomTheme>),
    Saved(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Themes {
    #[serde(deserialize_with = "appearance_value")]
    pub selection: Selection,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub saved: BTreeMap<String, CustomTheme>,
}

impl Themes {
    pub const LIMIT: usize = 24;

    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub(crate) fn editable(&self, wallpaper: Option<&Wallpaper>, mode: Mode) -> CustomTheme {
        let configured = match &self.selection {
            Selection::Preset(base) => Some(CustomTheme {
                base: *base,
                ..Default::default()
            }),
            Selection::Custom(theme) => Some(theme.as_ref().clone()),
            Selection::Saved(name) => self.saved.get(name).cloned(),
            Selection::Legacy => None,
        };
        configured.unwrap_or_else(|| CustomTheme {
            base: Preset::default(),
            overrides: Overrides {
                wallpaper: wallpaper
                    .filter(|&wallpaper| *wallpaper != Wallpaper::default())
                    .cloned(),
                mode: (mode != Mode::System).then_some(mode),
                ..Default::default()
            },
        })
    }

    pub fn valid_name(name: &str) -> bool {
        name == name.trim()
            && !name.is_empty()
            && name.len() <= 96
            && !name.chars().any(char::is_control)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.saved.len() <= Self::LIMIT,
            "Too many saved desktop themes"
        );
        for (name, theme) in &self.saved {
            ensure!(Self::valid_name(name), "Invalid desktop theme name");
            theme.validate()?;
        }
        match &self.selection {
            Selection::Custom(theme) => theme.validate()?,
            Selection::Saved(name) => ensure!(Self::valid_name(name), "Invalid desktop theme name"),
            _ => {}
        }
        Ok(())
    }
}

/// Gallery-only 320 by 180 previews; desktop backgrounds retain their full-size assets.
#[cfg(feature = "bundled-wallpapers")]
pub const BUNDLED_WALLPAPER_THUMBNAILS: &[(&str, &[u8])] = &[
    (
        "aurora-night",
        include_bytes!("../../assets/wallpapers/thumbnails/aurora-night.webp"),
    ),
    (
        "aurora-twilight",
        include_bytes!("../../assets/wallpapers/thumbnails/aurora-twilight.webp"),
    ),
    (
        "clay-dark",
        include_bytes!("../../assets/wallpapers/thumbnails/clay-dark.webp"),
    ),
    (
        "clay-light",
        include_bytes!("../../assets/wallpapers/thumbnails/clay-light.webp"),
    ),
    (
        "fjord-daylight",
        include_bytes!("../../assets/wallpapers/thumbnails/fjord-daylight.webp"),
    ),
    (
        "fjord",
        include_bytes!("../../assets/wallpapers/thumbnails/fjord.webp"),
    ),
    (
        "meadow-dark",
        include_bytes!("../../assets/wallpapers/thumbnails/meadow-dark.webp"),
    ),
    (
        "meadow-light",
        include_bytes!("../../assets/wallpapers/thumbnails/meadow-light.webp"),
    ),
    (
        "prism-dark",
        include_bytes!("../../assets/wallpapers/thumbnails/prism-dark.webp"),
    ),
    (
        "prism-light",
        include_bytes!("../../assets/wallpapers/thumbnails/prism-light.webp"),
    ),
    (
        "seaside-moonrise",
        include_bytes!("../../assets/wallpapers/thumbnails/seaside-moonrise.webp"),
    ),
    (
        "seaside-sunrise",
        include_bytes!("../../assets/wallpapers/thumbnails/seaside-sunrise.webp"),
    ),
    (
        "tide-dark",
        include_bytes!("../../assets/wallpapers/thumbnails/tide-dark.webp"),
    ),
    (
        "tide-light",
        include_bytes!("../../assets/wallpapers/thumbnails/tide-light.webp"),
    ),
    (
        "valley-dark",
        include_bytes!("../../assets/wallpapers/thumbnails/valley-dark.webp"),
    ),
    (
        "valley-light",
        include_bytes!("../../assets/wallpapers/thumbnails/valley-light.webp"),
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server_apps::{DesktopPreferences, parse_preferences};

    #[test]
    fn preset_accents_are_readable_in_both_schemes() {
        use ColorRole::*;
        for preset in Preset::ALL {
            let theme = preset.theme();
            for dark in [false, true] {
                let palette = theme.palette(dark);
                let fill = theme.accent_fill(dark);
                let surface = palette[&Surface];
                let label = fill.contrasting_label(surface, palette[&Text]);
                let selected = palette[&Background].mix(fill, 20);
                let readable = palette[&Accent];
                assert!(
                    contrast(surface.luminance(), fill.luminance()) >= 3.0,
                    "{preset:?}, dark={dark}"
                );
                assert!(contrast(fill.luminance(), label.luminance()) >= 4.5);
                assert!(contrast(selected.luminance(), readable.luminance()) >= 4.5);
            }
        }
    }

    #[test]
    fn generated_colors_keep_text_and_accent_legible_in_both_schemes() {
        use ColorRole::*;
        for picked in [
            [0; 3],
            [255; 3],
            [255, 255, 0],
            [15, 29, 60],
            [36, 150, 136],
            [120, 120, 120],
            [170, 163, 239],
        ] {
            let theme = CustomTheme {
                overrides: Overrides {
                    accent: Some(Rgb(picked)),
                    ..Default::default()
                },
                ..Default::default()
            }
            .resolve();
            for dark in [false, true] {
                let palette = theme.palette(dark);
                let background = palette[&Background];
                let accent = palette[&Accent];
                let fill = theme.accent_fill(dark);
                assert_eq!(fill, Rgb(picked));
                let label = fill.contrasting_label(palette[&Surface], palette[&Text]);
                assert!(contrast(fill.luminance(), label.luminance()) >= 4.5);
                let raised = palette[&Surface].mix(palette[&Text], 7);
                for surface in [background, palette[&Surface], palette[&Input], raised] {
                    for role in [Text, MutedText, Accent, Error] {
                        assert!(
                            contrast(surface.luminance(), palette[&role].luminance()) >= 4.5,
                            "{picked:?}, dark={dark}, {role:?} on {surface}"
                        );
                    }
                }
                assert!(contrast(background.mix(fill, 20).luminance(), accent.luminance()) >= 4.5);
            }
        }
    }

    #[test]
    fn sparse_legacy_preferences_keep_supplied_values_and_use_current_defaults() -> Result<()> {
        let prefs = parse_preferences(r#"{"appearance":{"show_server":false}}"#)?;
        let theme = prefs
            .appearance
            .themes
            .editable(prefs.appearance.wallpaper.as_ref(), prefs.theme);
        assert_eq!(theme.resolve(), Preset::Prism.theme());
        assert!(!prefs.appearance.show_server);
        assert!(prefs.appearance.show_apps);
        assert!(parse_preferences(r#"{"version":2}"#).is_err());
        Ok(())
    }

    #[test]
    fn explicit_legacy_fjord_wallpaper_survives_the_new_default() -> Result<()> {
        for name in ["fjord", "dunes"] {
            let prefs =
                parse_preferences(&format!(r#"{{"appearance":{{"wallpaper":"{name}"}}}}"#))?;
            let theme = prefs.appearance.theme_settings(prefs.theme).resolve();
            assert_eq!(theme.background(false), &Wallpaper::Fjord);
            assert_eq!(theme.background(true), &Wallpaper::Fjord);
        }
        Ok(())
    }

    #[test]
    fn retired_theme_settings_resolve_and_serialize_as_prism() -> Result<()> {
        for name in ["mist", "silk"] {
            let config = format!(r#"{{"appearance":{{"theme":"{name}","wallpaper":"{name}"}}}}"#);
            let prefs = parse_preferences(&config)?;
            assert_eq!(prefs.appearance.theme, Some(Preset::Prism));
            assert_eq!(prefs.appearance.wallpaper, Some(Wallpaper::Prism));
            let appearance = serde_json::to_value(&prefs.appearance)?;
            assert_eq!(appearance["theme"], "prism");
            assert_eq!(appearance["wallpaper"], "prism");
        }
        Ok(())
    }

    #[test]
    fn missing_saved_theme_uses_legacy_fields_without_removing_reference() -> Result<()> {
        let prefs = parse_preferences(
            r#"{"theme":"Dark","appearance":{"wallpaper":"arcnix","themes":{"selection":{"kind":"saved","value":"Missing"}}}}"#,
        )?;
        let themes = &prefs.appearance.themes;
        let theme = themes
            .editable(prefs.appearance.wallpaper.as_ref(), prefs.theme)
            .resolve();
        assert_eq!(theme.background(true), &Wallpaper::Seaside);
        assert_eq!(theme.mode, Mode::Dark);
        assert_eq!(theme.icons, IconStyle::default());
        assert_eq!(themes.selection, Selection::Saved("Missing".into()));
        Ok(())
    }

    #[test]
    fn previous_theme_names_resolve_to_seaside() -> Result<()> {
        for name in ["terracotta", "afterglow", "seaside"] {
            let prefs = parse_preferences(&format!(
                r#"{{"appearance":{{"theme":"{name}","wallpaper":"{name}"}}}}"#
            ))?;
            assert_eq!(prefs.appearance.theme, Some(Preset::Seaside));
            assert_eq!(prefs.appearance.wallpaper, Some(Wallpaper::Seaside));
        }
        assert_eq!(serde_json::to_value(Preset::Seaside)?, "seaside");
        assert_eq!(serde_json::to_value(Wallpaper::Seaside)?, "seaside");
        Ok(())
    }

    #[test]
    fn appearance_wallpaper_and_style_override_the_selected_preset() -> Result<()> {
        let prefs = parse_preferences(
            r#"{"appearance":{"theme":"aurora","wallpaper":"fjord","overrides":{"mode":"Light","icons":"filled"}}}"#,
        )?;
        let packet = crate::desktop_storage::encode(&prefs)?;
        let prefs = crate::desktop_storage::decode(&packet)?;
        let effective = prefs.appearance.theme_settings(prefs.theme).resolve();
        assert_eq!(effective.mode, Mode::Light);
        assert_eq!(effective.icons, IconStyle::Filled);
        assert_eq!(effective.background(false), &Wallpaper::Fjord);
        assert_eq!(effective.background(true), &Wallpaper::Fjord);

        Ok(())
    }

    #[test]
    fn applying_the_same_or_a_different_preset_restores_all_theme_defaults() -> Result<()> {
        let customized = parse_preferences(
            r#"{
            "theme": "Dark",
            "appearance": {
                "theme": "aurora", "wallpaper": "afterglow", "show_server": false,
                "overrides": {
                    "mode": "Light", "accent": [1,2,3], "colors": {"text": [4,5,6]},
                    "icons": "filled", "icon_family": "color",
                    "light": {"wallpaper": "afterglow", "accent": [7,8,9]},
                    "dark": {"wallpaper": "fjord", "colors": {"border": [10,11,12]}}
                }
            }
        }"#,
        )?;
        for preset in Preset::ALL {
            let mut prefs = customized.clone();
            prefs.appearance.apply_theme_preset(preset);
            let saved = crate::desktop_storage::decode(&crate::desktop_storage::encode(&prefs)?)?;
            assert_eq!(
                saved.appearance.theme_settings(saved.theme).resolve(),
                preset.theme()
            );
            assert!(saved.appearance.wallpaper.is_none());
            assert!(saved.appearance.overrides.is_default());
            assert!(!saved.appearance.show_server);
        }
        Ok(())
    }

    #[test]
    fn retired_appearance_values_fall_back_without_losing_valid_settings() -> Result<()> {
        let prefs = parse_preferences(
            r#"{
            "resource_interval_seconds": 5,
            "appearance": {
                "show_server": false,
                "theme": "retired_theme",
                "wallpaper": {"remote": "/home/demo/removed.png"},
                "wallpaper_pictures": {"/home/demo/removed.png": "Old picture"},
                "layout": {"flow":"rows", "size":"obsolete", "icon_size":100},
                "dock": {"position":"rotated", "auto_hide":true},
                "overrides": {
                    "icon_family": "unavailable",
                    "mode": "Dark",
                    "icons": "unknown_fill",
                    "accent": [50, 20, 280],
                    "colors": {"future_role": [0, 0, 0]}
                }
            }
        }"#,
        )?;
        assert_eq!(prefs.resource_interval_seconds, Some(5));
        assert!(!prefs.appearance.show_server);
        assert!(prefs.appearance.dock.auto_hide);
        assert_eq!(prefs.appearance.dock.position, Default::default());
        assert_eq!(
            prefs.appearance.layout.flow,
            crate::desktop_customization::IconFlow::Rows
        );
        assert_eq!(
            prefs.appearance.layout.icon_pixels(),
            crate::desktop_customization::Layout::default().icon_pixels()
        );
        let theme = prefs.appearance.theme_settings(prefs.theme).resolve();
        assert_eq!(
            theme.background(true),
            &Wallpaper::Remote("/home/demo/removed.png".into())
        );
        assert_eq!(theme.icon_family, IconFamily::Line);
        assert_eq!(theme.icons, IconStyle::Outline);
        assert_eq!(theme.mode, Mode::Dark);
        assert_eq!(theme.accent, None);
        assert_eq!(
            crate::desktop_storage::decode(&crate::desktop_storage::encode(&prefs)?)?,
            prefs
        );
        Ok(())
    }

    #[test]
    fn partial_custom_theme_uses_current_preset_for_each_omitted_value() -> Result<()> {
        let custom: CustomTheme = serde_json::from_str(r#"{"base":"aurora"}"#)?;
        assert_eq!(custom.resolve(), Preset::Aurora.theme());
        let custom: CustomTheme = serde_json::from_str(
            r#"{"base":"aurora","overrides":{"dark":{"colors":{"text":[240,251,255]}}}}"#,
        )?;
        let theme = custom.resolve();
        let preset = Preset::Aurora.theme();
        assert_eq!(theme.palette(false), preset.palette(false));
        assert_eq!(theme.background(false), preset.background(false));
        assert_eq!(theme.background(true), preset.background(true));
        assert_eq!(theme.main_color(true), preset.main_color(true));
        assert_eq!(theme.palette(true)[&ColorRole::Text], Rgb([240, 251, 255]));
        Ok(())
    }

    #[test]
    fn style_overrides_round_trip_with_their_named_theme() -> Result<()> {
        let mut prefs = DesktopPreferences::default();
        let custom = CustomTheme {
            base: Preset::Fjord,
            overrides: Overrides {
                accent: Rgb::parse("#508ea7"),
                colors: BTreeMap::from([(ColorRole::Text, Rgb([35, 45, 53]))]),
                dark: StyleOverrides {
                    wallpaper: Some(Wallpaper::Aurora),
                    colors: BTreeMap::from([(ColorRole::Text, Rgb([240, 251, 255]))]),
                    ..Default::default()
                },
                ..Default::default()
            },
        };
        prefs
            .appearance
            .themes
            .saved
            .insert("Night watch".into(), custom);
        prefs.appearance.themes.selection = Selection::Saved("Night watch".into());
        prefs.validate()?;
        let packet = crate::desktop_storage::encode(&prefs)?;
        let reread = crate::desktop_storage::decode(&packet)?;
        assert_eq!(reread, prefs);
        let original = prefs.appearance.theme_settings(prefs.theme).resolve();
        prefs.appearance.edit_theme_settings(prefs.theme);
        assert_eq!(prefs.appearance.theme, Some(Preset::Fjord));
        let theme = prefs.appearance.theme_settings(prefs.theme).resolve();
        assert_eq!(theme, original);
        assert_eq!(theme.background(false), &Wallpaper::Fjord);
        assert_eq!(theme.background(true), &Wallpaper::Aurora);
        assert_eq!(theme.palette(false)[&ColorRole::Text], Rgb([35, 45, 53]));
        assert_eq!(theme.palette(true)[&ColorRole::Text], Rgb([240, 251, 255]));
        assert_ne!(
            theme.palette(true)[&ColorRole::Background],
            theme.palette(false)[&ColorRole::Background]
        );
        Ok(())
    }
}
