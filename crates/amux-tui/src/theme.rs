//! Semantic colour tokens shared by every TUI surface.

use ratatui::style::{Color, Modifier, Style};

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
    /// The provisional dark palette. Visual tuning happens after the block
    /// vocabulary can be rendered as screenshots.
    pub const fn dark(mode: ColorMode) -> Self {
        Self {
            tokens: Tokens {
                background: Token::new((17, 19, 24), Color::Black),
                text: Token::new((230, 233, 239), Color::White),
                muted: Token::new((127, 132, 156), Color::DarkGray),
                emphasis: Token::new((244, 244, 245), Color::White),
                accent: Token::new((125, 207, 255), Color::Cyan),
                user_surface: Token::new((27, 36, 50), Color::DarkGray),
                panel: Token::new((32, 36, 50), Color::Blue),
                focus: Token::new((187, 154, 247), Color::Magenta),
                code: Token::new((122, 162, 247), Color::Cyan),
                ok: Token::new((158, 206, 106), Color::Green),
                warn: Token::new((224, 175, 104), Color::Yellow),
                error: Token::new((247, 118, 142), Color::Red),
                diff_added_fg: Token::new((158, 206, 106), Color::Black),
                diff_added_bg: Token::new((24, 44, 31), Color::Green),
                diff_removed_fg: Token::new((247, 118, 142), Color::White),
                diff_removed_bg: Token::new((52, 28, 34), Color::Red),
                diff_context: Token::new((192, 202, 224), Color::White),
                diff_meta: Token::new((105, 113, 147), Color::DarkGray),
                gutter: Token::new((86, 95, 137), Color::Gray),
            },
            mode,
            name: ThemeName::Dark,
        }
    }

    /// The provisional light palette. Visual tuning happens after the block
    /// vocabulary can be rendered as screenshots.
    pub const fn light(mode: ColorMode) -> Self {
        Self {
            tokens: Tokens {
                background: Token::new((247, 247, 245), Color::White),
                text: Token::new((36, 40, 59), Color::Black),
                muted: Token::new((107, 112, 137), Color::DarkGray),
                emphasis: Token::new((22, 22, 30), Color::Black),
                accent: Token::new((0, 109, 143), Color::Blue),
                user_surface: Token::new((233, 240, 243), Color::Cyan),
                panel: Token::new((236, 236, 241), Color::Gray),
                focus: Token::new((122, 76, 160), Color::Magenta),
                code: Token::new((0, 95, 135), Color::Blue),
                ok: Token::new((47, 125, 50), Color::Green),
                warn: Token::new((138, 90, 0), Color::Yellow),
                error: Token::new((180, 35, 53), Color::Red),
                diff_added_fg: Token::new((35, 105, 47), Color::Black),
                diff_added_bg: Token::new((224, 242, 226), Color::Green),
                diff_removed_fg: Token::new((168, 35, 52), Color::White),
                diff_removed_bg: Token::new((250, 226, 229), Color::Red),
                diff_context: Token::new((56, 60, 78), Color::Black),
                diff_meta: Token::new((117, 121, 139), Color::DarkGray),
                gutter: Token::new((137, 142, 160), Color::Gray),
            },
            mode,
            name: ThemeName::Light,
        }
    }

    fn color(self, token: Token) -> Color {
        token.resolve(self.mode)
    }

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
            if fg == Some(self.color(token)) && bg.is_none() && style.add_modifier.is_empty() {
                return class;
            }
        }

        let plain_fg = fg.is_none()
            || fg == Some(self.color(self.tokens.text))
            || fg == Some(self.color(self.tokens.diff_context));
        let plain_bg = bg.is_none() || bg == Some(self.color(self.tokens.background));
        if plain_fg && plain_bg && style.add_modifier.is_empty() && style.sub_modifier.is_empty() {
            '.'
        } else {
            '?'
        }
    }
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
}
