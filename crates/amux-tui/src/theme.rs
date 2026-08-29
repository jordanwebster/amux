//! Semantic colour tokens shared by every TUI surface.

use std::collections::BTreeMap;
use std::io;

use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;
use thiserror::Error;

/// The colour capability selected once at the terminal boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColorMode {
    TrueColor,
    Ansi,
}

/// The user's colour-capability preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorPreference {
    #[default]
    Auto,
    TrueColor,
    Ansi,
}

/// One semantic colour with truecolor and 16-colour terminal faces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub rgb: (u8, u8, u8),
    pub ansi: Color,
}

impl Token {
    const fn new(rgb: (u8, u8, u8), ansi: Color) -> Self {
        Self { rgb, ansi }
    }

    /// Resolve this token without making painters branch on terminal support.
    pub fn resolve(self, mode: ColorMode) -> Color {
        match mode {
            ColorMode::TrueColor => Color::Rgb(self.rgb.0, self.rgb.1, self.rgb.2),
            ColorMode::Ansi => self.ansi,
        }
    }
}

/// The complete semantic vocabulary available to TUI painters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tokens {
    pub background: Token,
    pub text: Token,
    pub muted: Token,
    pub emphasis: Token,
    pub accent: Token,
    pub user_surface: Token,
    pub panel: Token,
    pub focus: Token,
    pub code: Token,
    pub ok: Token,
    pub warn: Token,
    pub error: Token,
    pub diff_added_fg: Token,
    pub diff_added_bg: Token,
    pub diff_removed_fg: Token,
    pub diff_removed_bg: Token,
    pub diff_context: Token,
    pub diff_meta: Token,
    pub gutter: Token,
}

/// The provenance of a resolved palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemeName {
    Dark,
    Light,
    Imported,
}

/// Whether the imported scheme describes a dark or light palette.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Variant {
    Dark,
    Light,
}

/// A parsed base16/base24 scheme plus direct semantic overrides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeFile {
    pub scheme: Option<String>,
    pub base: BTreeMap<String, String>,
    pub tokens: BTreeMap<String, String>,
    pub variant: Option<Variant>,
}

/// Failures produced while parsing or resolving an imported theme.
#[derive(Debug, Error)]
pub enum ThemeError {
    #[error("invalid theme YAML: {0}")]
    Yaml(String),
    #[error("missing base colour `{0}`")]
    MissingBase(String),
    #[error("invalid colour for `{key}`: `{value}`")]
    BadColor { key: String, value: String },
    #[error("unknown theme token `{0}`")]
    UnknownToken(String),
    #[error("failed to read theme: {0}")]
    Io(#[from] io::Error),
}

/// A complete palette resolved in one terminal colour mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub tokens: Tokens,
    pub mode: ColorMode,
    pub name: ThemeName,
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark(ColorMode::TrueColor)
    }
}

impl Theme {
    /// amux's own dark palette: a cool near-black that is not quite
    /// black, one clearly lighter surface for the person's own words, and
    /// accents pulled toward teal and moss so nothing in the feed shouts.
    /// Tuned by eye at 120x40 against the working Claude and Codex
    /// screens; every hex is amux's, borrowed from no published scheme.
    pub const fn dark(mode: ColorMode) -> Self {
        Self {
            tokens: Tokens {
                background: Token::new((15, 18, 22), Color::Black),
                text: Token::new((223, 227, 232), Color::White),
                muted: Token::new((138, 144, 160), Color::DarkGray),
                emphasis: Token::new((242, 244, 247), Color::White),
                accent: Token::new((95, 179, 198), Color::Cyan),
                user_surface: Token::new((24, 32, 40), Color::DarkGray),
                panel: Token::new((23, 27, 34), Color::Blue),
                focus: Token::new((156, 140, 214), Color::Magenta),
                code: Token::new((127, 182, 217), Color::Cyan),
                ok: Token::new((134, 184, 122), Color::Green),
                warn: Token::new((210, 162, 76), Color::Yellow),
                error: Token::new((222, 123, 132), Color::Red),
                diff_added_fg: Token::new((143, 203, 138), Color::Black),
                diff_added_bg: Token::new((22, 38, 27), Color::Green),
                diff_removed_fg: Token::new((224, 141, 149), Color::White),
                diff_removed_bg: Token::new((44, 26, 30), Color::Red),
                diff_context: Token::new((195, 201, 212), Color::White),
                diff_meta: Token::new((110, 118, 134), Color::DarkGray),
                gutter: Token::new((89, 96, 111), Color::Gray),
            },
            mode,
            name: ThemeName::Dark,
        }
    }

    /// amux's own light palette: a warm off-white that is easier to sit
    /// in front of than pure white, the same teal accent darkened until
    /// it holds its own on paper, and diff tints kept pale enough that a
    /// hunk still reads as text.
    pub const fn light(mode: ColorMode) -> Self {
        Self {
            tokens: Tokens {
                background: Token::new((250, 250, 248), Color::White),
                text: Token::new((42, 46, 56), Color::Black),
                muted: Token::new((106, 112, 128), Color::DarkGray),
                emphasis: Token::new((21, 24, 31), Color::Black),
                accent: Token::new((31, 111, 130), Color::Blue),
                user_surface: Token::new((236, 241, 243), Color::Cyan),
                panel: Token::new((240, 240, 238), Color::Gray),
                focus: Token::new((109, 78, 156), Color::Magenta),
                code: Token::new((26, 95, 135), Color::Blue),
                ok: Token::new((47, 122, 68), Color::Green),
                warn: Token::new((138, 91, 16), Color::Yellow),
                error: Token::new((168, 50, 68), Color::Red),
                diff_added_fg: Token::new((34, 107, 51), Color::Black),
                diff_added_bg: Token::new((227, 242, 229), Color::Green),
                diff_removed_fg: Token::new((158, 47, 63), Color::White),
                diff_removed_bg: Token::new((250, 228, 230), Color::Red),
                diff_context: Token::new((58, 63, 76), Color::Black),
                diff_meta: Token::new((117, 122, 136), Color::DarkGray),
                gutter: Token::new((138, 143, 156), Color::Gray),
            },
            mode,
            name: ThemeName::Light,
        }
    }

    fn color(self, token: Token) -> Color {
        token.resolve(self.mode)
    }

    // These semantic styles are the whole vocabulary the chat and the fleet
    // may paint from: naming a token here rather than a colour literal is
    // what makes a palette swap a data change and a token rename a compile
    // error. Every one of them has a caller.
    /// The terminal background.
    pub(crate) fn background(self) -> Style {
        Style::default().bg(self.color(self.tokens.background))
    }

    /// Body text.
    pub(crate) fn text(self) -> Style {
        Style::default().fg(self.color(self.tokens.text))
    }

    /// De-emphasis: markers, rules, continuations, and hints.
    pub(crate) fn muted(self) -> Style {
        Style::default().fg(self.color(self.tokens.muted))
    }

    /// Markdown emphasis (bold) and headings.
    pub(crate) fn emphasis(self) -> Style {
        Style::default()
            .fg(self.color(self.tokens.emphasis))
            .add_modifier(Modifier::BOLD)
    }

    /// Markdown italic emphasis.
    pub(crate) fn italic(self) -> Style {
        Style::default()
            .fg(self.color(self.tokens.text))
            .add_modifier(Modifier::ITALIC)
    }

    /// Inline code and fenced code blocks.
    pub(crate) fn code(self) -> Style {
        Style::default().fg(self.color(self.tokens.code))
    }

    /// Success accents (`✔`).
    pub(crate) fn ok(self) -> Style {
        Style::default().fg(self.color(self.tokens.ok))
    }

    /// Attention accents (`needs you`, question marks).
    pub(crate) fn warn(self) -> Style {
        Style::default().fg(self.color(self.tokens.warn))
    }

    /// Failure accents (`✗`, API errors, failed sends).
    pub(crate) fn error(self) -> Style {
        Style::default().fg(self.color(self.tokens.error))
    }

    /// The filled user-message and composer surface.
    pub(crate) fn user_surface(self) -> Style {
        self.text().bg(self.color(self.tokens.user_surface))
    }

    /// The filled diff and ask-panel surface.
    pub(crate) fn panel(self) -> Style {
        self.text().bg(self.color(self.tokens.panel))
    }

    /// The bar at the left edge of a user surface.
    pub(crate) fn accent_bar(self) -> Style {
        Style::default()
            .fg(self.color(self.tokens.accent))
            .bg(self.color(self.tokens.user_surface))
    }

    /// The bar marking the focused feed block.
    pub(crate) fn focus_bar(self) -> Style {
        Style::default().fg(self.color(self.tokens.focus))
    }

    /// Added diff content and its tint.
    pub(crate) fn diff_added(self) -> Style {
        Style::default()
            .fg(self.color(self.tokens.diff_added_fg))
            .bg(self.color(self.tokens.diff_added_bg))
    }

    /// Removed diff content and its tint.
    pub(crate) fn diff_removed(self) -> Style {
        Style::default()
            .fg(self.color(self.tokens.diff_removed_fg))
            .bg(self.color(self.tokens.diff_removed_bg))
    }

    /// Unchanged diff content on the panel surface.
    pub(crate) fn diff_context(self) -> Style {
        Style::default()
            .fg(self.color(self.tokens.diff_context))
            .bg(self.color(self.tokens.panel))
    }

    /// Diff metadata on the panel surface.
    pub(crate) fn diff_meta(self) -> Style {
        Style::default()
            .fg(self.color(self.tokens.diff_meta))
            .bg(self.color(self.tokens.panel))
    }

    /// The numbered diff gutter on the panel surface.
    pub(crate) fn gutter(self) -> Style {
        Style::default()
            .fg(self.color(self.tokens.gutter))
            .bg(self.color(self.tokens.panel))
    }

    /// Convert a rendered cell style into its semantic style-map class.
    pub fn classify(self, style: Style) -> char {
        let fg = style.fg;
        let bg = style.bg;

        if fg == Some(self.color(self.tokens.accent))
            && bg == Some(self.color(self.tokens.user_surface))
        {
            return 'A';
        }
        if fg == Some(self.color(self.tokens.focus)) {
            return 'F';
        }
        if bg == Some(self.color(self.tokens.diff_added_bg)) {
            return '+';
        }
        if bg == Some(self.color(self.tokens.diff_removed_bg)) {
            return '-';
        }
        if bg == Some(self.color(self.tokens.user_surface)) {
            return 'U';
        }
        if bg == Some(self.color(self.tokens.panel)) {
            return 'P';
        }

        // A row that fills to the width sets the background token
        // explicitly, so a foreground token over it is still a plain row
        // wearing one colour — not a surface.
        let plain_bg = bg.is_none() || bg == Some(self.color(self.tokens.background));

        if style.add_modifier.contains(Modifier::BOLD)
            && fg == Some(self.color(self.tokens.emphasis))
        {
            return 'e';
        }
        if style.add_modifier.contains(Modifier::ITALIC) && fg == Some(self.color(self.tokens.text))
        {
            return 'i';
        }

        for (token, class) in [
            (self.tokens.muted, 'm'),
            (self.tokens.code, 'c'),
            (self.tokens.ok, 'o'),
            (self.tokens.warn, 'w'),
            (self.tokens.error, 'x'),
            (self.tokens.diff_meta, 'M'),
            (self.tokens.gutter, 'G'),
        ] {
            if fg == Some(self.color(token)) && plain_bg && style.add_modifier.is_empty() {
                return class;
            }
        }

        let plain_fg = fg.is_none()
            || fg == Some(self.color(self.tokens.text))
            || fg == Some(self.color(self.tokens.diff_context));
        if plain_fg && plain_bg && style.add_modifier.is_empty() && style.sub_modifier.is_empty() {
            '.'
        } else {
            '?'
        }
    }
}

#[derive(Deserialize)]
struct RawThemeFile {
    scheme: Option<String>,
    variant: Option<Variant>,
    #[serde(default)]
    tokens: BTreeMap<String, String>,
    #[serde(flatten)]
    remaining: BTreeMap<String, String>,
}

/// Parse the conventional top-level base16/base24 YAML shape. Metadata fields
/// other than `scheme` and `variant` are ignored; `base00` through `base17`
/// are retained for validation and resolution.
pub fn parse_theme_file(yaml: &str) -> Result<ThemeFile, ThemeError> {
    let raw: RawThemeFile =
        serde_yaml::from_str(yaml).map_err(|error| ThemeError::Yaml(error.to_string()))?;
    let base = raw
        .remaining
        .into_iter()
        .filter_map(|(key, value)| normalize_base_key(&key).map(|key| (key, value)))
        .collect();
    Ok(ThemeFile {
        scheme: raw.scheme,
        base,
        tokens: raw.tokens,
        variant: raw.variant,
    })
}

fn normalize_base_key(key: &str) -> Option<String> {
    let suffix = key.strip_prefix("base")?;
    let index = (suffix.len() == 2)
        .then(|| u8::from_str_radix(suffix, 16).ok())
        .flatten()
        .filter(|index| *index <= 0x17)?;
    Some(format!("base{index:02X}"))
}

/// Resolve a parsed scheme into every semantic token.
///
/// | semantic token | base16 | base24 |
/// | --- | --- | --- |
/// | background | 00 | 00 |
/// | user surface, diff tint fallbacks | 01 | 01 |
/// | panel | 02 | 02 |
/// | muted, gutter | 03 | 03 |
/// | diff metadata | 04 | 04 |
/// | text, diff context | 05 | 05 |
/// | emphasis | 06 | 06 |
/// | error, removed foreground | 08 | 12 |
/// | warning | 09 | 14 |
/// | success, added foreground | 0B | 13 |
/// | code | 0C | 17 |
/// | accent | 0D | 15 |
/// | focus | 0E | 16 |
///
/// Direct `tokens:` overrides are applied after this mapping.
pub fn theme_from_file(file: &ThemeFile, mode: ColorMode) -> Result<Theme, ThemeError> {
    for index in 0..=0x0F {
        mapped_token(file, &format!("base{index:02X}"))?;
    }

    let base24 = (0x10..=0x17).any(|index| file.base.contains_key(&format!("base{index:02X}")));
    if base24 {
        for index in 0x10..=0x17 {
            mapped_token(file, &format!("base{index:02X}"))?;
        }
    }

    let accent_base = |base16, base24_key| if base24 { base24_key } else { base16 };
    let mut tokens = Tokens {
        background: mapped_token(file, "base00")?,
        user_surface: mapped_token(file, "base01")?,
        panel: mapped_token(file, "base02")?,
        muted: mapped_token(file, "base03")?,
        gutter: mapped_token(file, "base03")?,
        diff_meta: mapped_token(file, "base04")?,
        text: mapped_token(file, "base05")?,
        diff_context: mapped_token(file, "base05")?,
        emphasis: mapped_token(file, "base06")?,
        error: mapped_token(file, accent_base("base08", "base12"))?,
        diff_removed_fg: mapped_token(file, accent_base("base08", "base12"))?,
        warn: mapped_token(file, accent_base("base09", "base14"))?,
        ok: mapped_token(file, accent_base("base0B", "base13"))?,
        diff_added_fg: mapped_token(file, accent_base("base0B", "base13"))?,
        code: mapped_token(file, accent_base("base0C", "base17"))?,
        accent: mapped_token(file, accent_base("base0D", "base15"))?,
        focus: mapped_token(file, accent_base("base0E", "base16"))?,
        diff_added_bg: mapped_token(file, "base01")?,
        diff_removed_bg: mapped_token(file, "base01")?,
    };

    for (name, value) in &file.tokens {
        let token = parse_token(name, value)?;
        set_token(&mut tokens, name, token)?;
    }

    Ok(Theme {
        tokens,
        mode,
        name: ThemeName::Imported,
    })
}

fn require_base<'a>(file: &'a ThemeFile, key: &str) -> Result<&'a str, ThemeError> {
    file.base
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| ThemeError::MissingBase(key.to_string()))
}

fn mapped_token(file: &ThemeFile, key: &str) -> Result<Token, ThemeError> {
    let value = require_base(file, key)?;
    parse_token(key, value)
}

fn parse_token(key: &str, value: &str) -> Result<Token, ThemeError> {
    let hex = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ThemeError::BadColor {
            key: key.to_string(),
            value: value.to_string(),
        });
    }
    let rgb = (
        u8::from_str_radix(&hex[0..2], 16).expect("validated hex"),
        u8::from_str_radix(&hex[2..4], 16).expect("validated hex"),
        u8::from_str_radix(&hex[4..6], 16).expect("validated hex"),
    );
    Ok(Token::new(rgb, nearest_ansi(rgb)))
}

fn set_token(tokens: &mut Tokens, name: &str, token: Token) -> Result<(), ThemeError> {
    let destination = match name {
        "background" => &mut tokens.background,
        "text" => &mut tokens.text,
        "muted" => &mut tokens.muted,
        "emphasis" => &mut tokens.emphasis,
        "accent" => &mut tokens.accent,
        "user_surface" => &mut tokens.user_surface,
        "panel" => &mut tokens.panel,
        "focus" => &mut tokens.focus,
        "code" => &mut tokens.code,
        "ok" => &mut tokens.ok,
        "warn" => &mut tokens.warn,
        "error" => &mut tokens.error,
        "diff_added_fg" => &mut tokens.diff_added_fg,
        "diff_added_bg" => &mut tokens.diff_added_bg,
        "diff_removed_fg" => &mut tokens.diff_removed_fg,
        "diff_removed_bg" => &mut tokens.diff_removed_bg,
        "diff_context" => &mut tokens.diff_context,
        "diff_meta" => &mut tokens.diff_meta,
        "gutter" => &mut tokens.gutter,
        _ => return Err(ThemeError::UnknownToken(name.to_string())),
    };
    *destination = token;
    Ok(())
}

/// Choose the nearest named 16-colour terminal face by squared RGB distance.
pub fn nearest_ansi(rgb: (u8, u8, u8)) -> Color {
    const ANSI: [(Color, (u8, u8, u8)); 16] = [
        (Color::Black, (0, 0, 0)),
        (Color::Red, (128, 0, 0)),
        (Color::Green, (0, 128, 0)),
        (Color::Yellow, (128, 128, 0)),
        (Color::Blue, (0, 0, 128)),
        (Color::Magenta, (128, 0, 128)),
        (Color::Cyan, (0, 128, 128)),
        (Color::Gray, (192, 192, 192)),
        (Color::DarkGray, (128, 128, 128)),
        (Color::LightRed, (255, 0, 0)),
        (Color::LightGreen, (0, 255, 0)),
        (Color::LightYellow, (255, 255, 0)),
        (Color::LightBlue, (0, 0, 255)),
        (Color::LightMagenta, (255, 0, 255)),
        (Color::LightCyan, (0, 255, 255)),
        (Color::White, (255, 255, 255)),
    ];

    ANSI.into_iter()
        .min_by_key(|(_, candidate)| rgb_distance(rgb, *candidate))
        .map(|(color, _)| color)
        .expect("ANSI palette is non-empty")
}

fn rgb_distance(left: (u8, u8, u8), right: (u8, u8, u8)) -> u32 {
    let red = i32::from(left.0) - i32::from(right.0);
    let green = i32::from(left.1) - i32::from(right.1);
    let blue = i32::from(left.2) - i32::from(right.2);
    (red * red + green * green + blue * blue) as u32
}

/// Resolve the terminal colour mode from an explicit preference and captured
/// environment strings. `TERM` is accepted at this boundary for diagnostics,
/// but auto mode deliberately trusts only the standard `COLORTERM` signal.
pub fn detect_color_mode(
    preference: ColorPreference,
    colorterm: Option<&str>,
    _term: Option<&str>,
    no_color: bool,
) -> ColorMode {
    match preference {
        ColorPreference::TrueColor => ColorMode::TrueColor,
        ColorPreference::Ansi => ColorMode::Ansi,
        ColorPreference::Auto => {
            let advertised = colorterm.is_some_and(|value| {
                value.eq_ignore_ascii_case("truecolor") || value.eq_ignore_ascii_case("24bit")
            });
            if advertised && !no_color {
                ColorMode::TrueColor
            } else {
                ColorMode::Ansi
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE16_SAMPLE: &str = include_str!("../tests/themes/base16-sample.yaml");
    const BASE24_SAMPLE: &str = include_str!("../tests/themes/base24-sample.yaml");

    #[test]
    fn default_is_dark_truecolor() {
        let theme = Theme::default();
        assert_eq!(theme.name, ThemeName::Dark);
        assert_eq!(theme.mode, ColorMode::TrueColor);
        assert!(matches!(theme.text().fg, Some(Color::Rgb(..))));
    }

    #[test]
    fn token_resolves_the_selected_face() {
        let token = Token::new((1, 2, 3), Color::Cyan);
        assert_eq!(token.resolve(ColorMode::TrueColor), Color::Rgb(1, 2, 3));
        assert_eq!(token.resolve(ColorMode::Ansi), Color::Cyan);
    }

    #[test]
    fn classify_recognizes_semantic_styles() {
        let theme = Theme::default();
        assert_eq!(theme.classify(Style::default()), '.');
        assert_eq!(theme.classify(theme.text()), '.');
        assert_eq!(theme.classify(theme.background()), '.');
        assert_eq!(theme.classify(theme.muted()), 'm');
        assert_eq!(theme.classify(theme.emphasis()), 'e');
        assert_eq!(theme.classify(theme.italic()), 'i');
        assert_eq!(theme.classify(theme.code()), 'c');
        assert_eq!(theme.classify(theme.ok()), 'o');
        assert_eq!(theme.classify(theme.warn()), 'w');
        assert_eq!(theme.classify(theme.error()), 'x');
        assert_eq!(theme.classify(theme.user_surface()), 'U');
        assert_eq!(theme.classify(theme.panel()), 'P');
        assert_eq!(theme.classify(theme.accent_bar()), 'A');
        assert_eq!(theme.classify(theme.focus_bar()), 'F');
        assert_eq!(theme.classify(theme.diff_added()), '+');
        assert_eq!(theme.classify(theme.diff_removed()), '-');
        assert_eq!(theme.classify(theme.diff_context()), 'P');
        assert_eq!(theme.classify(theme.diff_meta()), 'P');
        assert_eq!(theme.classify(theme.gutter()), 'P');
    }

    #[test]
    fn classify_rejects_stray_literal_styles() {
        let theme = Theme::default();
        let stray = Style::default().fg(Color::Rgb(1, 2, 3));
        assert_eq!(theme.classify(stray), '?');
    }

    #[test]
    fn classify_resolves_ansi_faces_without_confusing_the_accent_and_code() {
        for theme in [Theme::dark(ColorMode::Ansi), Theme::light(ColorMode::Ansi)] {
            assert_eq!(theme.classify(theme.accent_bar()), 'A');
            assert_eq!(theme.classify(theme.code()), 'c');
            assert_eq!(theme.classify(theme.diff_added()), '+');
            assert_eq!(theme.classify(theme.diff_removed()), '-');
            assert_eq!(theme.classify(theme.user_surface()), 'U');
            assert_eq!(theme.classify(theme.panel()), 'P');
        }
    }

    #[test]
    fn auto_detection_requires_a_truecolor_advertisement() {
        assert_eq!(
            detect_color_mode(
                ColorPreference::Auto,
                Some("truecolor"),
                Some("xterm"),
                false
            ),
            ColorMode::TrueColor
        );
        assert_eq!(
            detect_color_mode(ColorPreference::Auto, Some("24BIT"), None, false),
            ColorMode::TrueColor
        );
        assert_eq!(
            detect_color_mode(ColorPreference::Auto, None, Some("xterm-truecolor"), false),
            ColorMode::Ansi
        );
        assert_eq!(
            detect_color_mode(ColorPreference::Auto, Some("yes"), Some("xterm"), false),
            ColorMode::Ansi
        );
    }

    #[test]
    fn forced_detection_ignores_environment_capability() {
        assert_eq!(
            detect_color_mode(ColorPreference::TrueColor, None, Some("dumb"), true),
            ColorMode::TrueColor
        );
        assert_eq!(
            detect_color_mode(
                ColorPreference::Ansi,
                Some("truecolor"),
                Some("xterm"),
                false
            ),
            ColorMode::Ansi
        );
    }

    #[test]
    fn no_color_disables_auto_truecolor() {
        assert_eq!(
            detect_color_mode(
                ColorPreference::Auto,
                Some("truecolor"),
                Some("xterm"),
                true
            ),
            ColorMode::Ansi
        );
    }

    #[test]
    fn base16_sample_maps_every_semantic_family() {
        let file = parse_theme_file(BASE16_SAMPLE).expect("parse base16 fixture");
        assert_eq!(file.scheme.as_deref(), Some("amux base16 sample"));
        assert_eq!(file.variant, Some(Variant::Dark));
        assert_eq!(file.base.len(), 16);

        let theme = theme_from_file(&file, ColorMode::TrueColor).expect("resolve base16 fixture");
        assert_eq!(theme.name, ThemeName::Imported);
        assert_eq!(theme.tokens.background.rgb, (0x10, 0x10, 0x10));
        assert_eq!(theme.tokens.user_surface.rgb, (0x20, 0x20, 0x20));
        assert_eq!(theme.tokens.panel.rgb, (0x30, 0x30, 0x30));
        assert_eq!(theme.tokens.muted.rgb, (0x40, 0x40, 0x40));
        assert_eq!(theme.tokens.diff_meta.rgb, (0x50, 0x50, 0x50));
        assert_eq!(theme.tokens.text.rgb, (0x60, 0x60, 0x60));
        assert_eq!(theme.tokens.emphasis.rgb, (0x70, 0x70, 0x70));
        assert_eq!(theme.tokens.error.rgb, (0x90, 0x00, 0x00));
        assert_eq!(theme.tokens.warn.rgb, (0xa0, 0x60, 0x00));
        assert_eq!(theme.tokens.ok.rgb, (0x00, 0x90, 0x00));
        assert_eq!(theme.tokens.code.rgb, (0x00, 0x80, 0x90));
        assert_eq!(theme.tokens.accent.rgb, (0x00, 0x60, 0xa0));
        assert_eq!(theme.tokens.focus.rgb, (0x70, 0x40, 0xa0));
        assert_eq!(theme.tokens.diff_added_bg.rgb, (0x20, 0x20, 0x20));
        assert_eq!(theme.tokens.diff_removed_bg.rgb, (0x20, 0x20, 0x20));
    }

    #[test]
    fn base24_sample_uses_bright_accents_and_applies_overrides_last() {
        let file = parse_theme_file(BASE24_SAMPLE).expect("parse base24 fixture");
        assert_eq!(file.variant, Some(Variant::Light));
        assert_eq!(file.base.len(), 24);

        let theme = theme_from_file(&file, ColorMode::Ansi).expect("resolve base24 fixture");
        assert_eq!(theme.mode, ColorMode::Ansi);
        assert_eq!(theme.tokens.error.rgb, (0xff, 0x40, 0x40));
        assert_eq!(theme.tokens.ok.rgb, (0x40, 0xb0, 0x40));
        assert_eq!(theme.tokens.warn.rgb, (0xd0, 0x90, 0x20));
        assert_eq!(theme.tokens.code.rgb, (0x30, 0xa0, 0xa0));
        assert_eq!(theme.tokens.focus.rgb, (0xa0, 0x60, 0xc0));
        assert_eq!(theme.tokens.accent.rgb, (0xab, 0xcd, 0xef));
        assert_eq!(theme.tokens.accent.ansi, nearest_ansi((0xab, 0xcd, 0xef)));
    }

    #[test]
    fn direct_token_override_replaces_the_base_mapping() {
        let mut file = parse_theme_file(BASE16_SAMPLE).expect("parse fixture");
        file.tokens.insert("diff_added_bg".into(), "#123456".into());
        let theme = theme_from_file(&file, ColorMode::TrueColor).expect("resolve fixture");
        assert_eq!(theme.tokens.diff_added_bg.rgb, (0x12, 0x34, 0x56));
    }

    #[test]
    fn missing_base_names_the_key() {
        let mut file = parse_theme_file(BASE16_SAMPLE).expect("parse fixture");
        file.base.remove("base0F");
        assert!(matches!(
            theme_from_file(&file, ColorMode::TrueColor),
            Err(ThemeError::MissingBase(key)) if key == "base0F"
        ));
    }

    #[test]
    fn partial_base24_extension_names_the_first_missing_key() {
        let mut file = parse_theme_file(BASE16_SAMPLE).expect("parse fixture");
        file.base.insert("base10".into(), "#101010".into());
        assert!(matches!(
            theme_from_file(&file, ColorMode::TrueColor),
            Err(ThemeError::MissingBase(key)) if key == "base11"
        ));
    }

    #[test]
    fn bad_base_colour_names_the_key_and_value() {
        let mut file = parse_theme_file(BASE16_SAMPLE).expect("parse fixture");
        file.base.insert("base0F".into(), "ultraviolet".into());
        assert!(matches!(
            theme_from_file(&file, ColorMode::TrueColor),
            Err(ThemeError::BadColor { key, value })
                if key == "base0F" && value == "ultraviolet"
        ));
    }

    #[test]
    fn bad_override_colour_names_the_token_and_value() {
        let mut file = parse_theme_file(BASE16_SAMPLE).expect("parse fixture");
        file.tokens.insert("accent".into(), "#xyzxyz".into());
        assert!(matches!(
            theme_from_file(&file, ColorMode::TrueColor),
            Err(ThemeError::BadColor { key, value })
                if key == "accent" && value == "#xyzxyz"
        ));
    }

    #[test]
    fn unknown_override_names_the_token() {
        let mut file = parse_theme_file(BASE16_SAMPLE).expect("parse fixture");
        file.tokens.insert("sparkle".into(), "#123456".into());
        assert!(matches!(
            theme_from_file(&file, ColorMode::TrueColor),
            Err(ThemeError::UnknownToken(key)) if key == "sparkle"
        ));
    }

    #[test]
    fn malformed_yaml_is_a_yaml_error() {
        assert!(matches!(
            parse_theme_file("base00: [unterminated"),
            Err(ThemeError::Yaml(_))
        ));
    }

    #[test]
    fn lowercase_base_keys_are_normalized() {
        let yaml = BASE16_SAMPLE.replace("base0A", "base0a");
        let file = parse_theme_file(&yaml).expect("parse lowercase base key");
        assert!(file.base.contains_key("base0A"));
        theme_from_file(&file, ColorMode::TrueColor).expect("resolve normalized fixture");
    }

    #[test]
    fn nearest_ansi_uses_named_terminal_colours() {
        assert_eq!(nearest_ansi((1, 2, 3)), Color::Black);
        assert_eq!(nearest_ansi((250, 10, 10)), Color::LightRed);
        assert_eq!(nearest_ansi((10, 240, 245)), Color::LightCyan);
        assert_eq!(nearest_ansi((248, 248, 248)), Color::White);
    }

    // --- the two shipped palettes -------------------------------------

    /// Relative luminance, per WCAG 2.
    fn luminance(rgb: (u8, u8, u8)) -> f64 {
        fn channel(value: u8) -> f64 {
            let value = f64::from(value) / 255.0;
            if value <= 0.040_45 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(rgb.0) + 0.7152 * channel(rgb.1) + 0.0722 * channel(rgb.2)
    }

    fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        let (lighter, darker) = if a > b { (a, b) } else { (b, a) };
        (lighter + 0.05) / (darker + 0.05)
    }

    /// Body text has to be readable on every surface it can land on, in
    /// both palettes — 4.5:1 is the WCAG AA threshold for ordinary text.
    #[test]
    fn palettes_keep_text_readable_on_every_surface() {
        for theme in [Theme::dark(ColorMode::TrueColor), Theme::light(ColorMode::TrueColor)] {
            let text = theme.tokens.text.rgb;
            for (surface, name) in [
                (theme.tokens.background.rgb, "background"),
                (theme.tokens.user_surface.rgb, "user_surface"),
                (theme.tokens.panel.rgb, "panel"),
            ] {
                let ratio = contrast(text, surface);
                assert!(
                    ratio >= 4.5,
                    "{:?} text on {name} is only {ratio:.1}:1",
                    theme.name
                );
            }
            for (fg, bg, name) in [
                (
                    theme.tokens.diff_added_fg.rgb,
                    theme.tokens.diff_added_bg.rgb,
                    "diff added",
                ),
                (
                    theme.tokens.diff_removed_fg.rgb,
                    theme.tokens.diff_removed_bg.rgb,
                    "diff removed",
                ),
            ] {
                let ratio = contrast(fg, bg);
                assert!(
                    ratio >= 4.5,
                    "{:?} {name} is only {ratio:.1}:1",
                    theme.name
                );
            }
        }
    }

    /// A tinted diff row has to be visibly a tint and not just the panel
    /// it sits on, and the two tints have to be told apart from each
    /// other at a glance.
    #[test]
    fn palettes_separate_the_diff_tints_from_the_panel() {
        for theme in [Theme::dark(ColorMode::TrueColor), Theme::light(ColorMode::TrueColor)] {
            let panel = theme.tokens.panel.rgb;
            let added = theme.tokens.diff_added_bg.rgb;
            let removed = theme.tokens.diff_removed_bg.rgb;
            assert_ne!(added, panel, "{:?} added tint is the panel", theme.name);
            assert_ne!(removed, panel, "{:?} removed tint is the panel", theme.name);
            assert_ne!(added, removed, "{:?} tints are the same", theme.name);
            assert_ne!(
                theme.tokens.user_surface.rgb, panel,
                "{:?} user surface is the panel",
                theme.name
            );
        }
    }

    /// The shipped palettes are amux's own. This locks the deliberate
    /// move off the borrowed hexes the provisional palette used, so a
    /// later tweak cannot quietly reintroduce a published scheme amux
    /// only supports as an imported theme file.
    #[test]
    fn palettes_borrow_no_published_scheme() {
        const BORROWED: [(u8, u8, u8); 6] = [
            (122, 162, 247),
            (125, 207, 255),
            (158, 206, 106),
            (224, 175, 104),
            (247, 118, 142),
            (187, 154, 247),
        ];
        for theme in [Theme::dark(ColorMode::TrueColor), Theme::light(ColorMode::TrueColor)] {
            for token in [
                theme.tokens.background,
                theme.tokens.text,
                theme.tokens.muted,
                theme.tokens.emphasis,
                theme.tokens.accent,
                theme.tokens.user_surface,
                theme.tokens.panel,
                theme.tokens.focus,
                theme.tokens.code,
                theme.tokens.ok,
                theme.tokens.warn,
                theme.tokens.error,
                theme.tokens.diff_added_fg,
                theme.tokens.diff_removed_fg,
                theme.tokens.diff_context,
                theme.tokens.diff_meta,
                theme.tokens.gutter,
            ] {
                assert!(
                    !BORROWED.contains(&token.rgb),
                    "{:?} still carries a borrowed colour {:?}",
                    theme.name,
                    token.rgb
                );
            }
        }
    }
}
