//! Semantic colour tokens shared by every TUI surface.

use std::collections::{BTreeMap, BTreeSet};
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
/// base16 has no diff backgrounds, so both tints start from `base01` and are
/// then tinted with the scheme's own success and error hues.
///
/// Direct `tokens:` overrides are applied after this mapping and are taken
/// literally; everything else is then put through [`make_readable`].
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

    let mut authored = BTreeSet::new();
    for (name, value) in &file.tokens {
        let token = parse_token(name, value)?;
        set_token(&mut tokens, name, token)?;
        authored.insert(name.clone());
    }

    make_readable(&mut tokens, &authored);

    Ok(Theme {
        tokens,
        mode,
        name: ThemeName::Imported,
    })
}

/// The contrast an imported palette has to reach before amux paints words
/// with it. Body copy is held above the WCAG AAA threshold so an imported
/// scheme still reads like the shipped ones; labels, accents and status
/// colours are held to AA; gutters, rules and hunk metadata are decoration
/// and only have to be visible.
const READABLE_BODY: f64 = 7.0;
const READABLE_LABEL: f64 = 4.5;
const READABLE_TRIM: f64 = 3.0;

/// How far apart two foregrounds have to be before a reader can see that
/// one of them is emphasised.
const SEPARATION: f64 = 1.25;

/// Repair an imported palette so every foreground is legible on the surfaces
/// it can land on.
///
/// A base16 scheme is authored for an editor, where the only thing painted on
/// `base00` is source code that the scheme's author looked at. amux paints
/// labels, a composer and a status line as well, and a scheme whose `base05`
/// sits a few steps above `base00` leaves all of that unreadable — worse once
/// sixteen-colour degradation rounds both of them to the same terminal face.
/// Rather than reject such a scheme, lift each foreground away from its
/// surfaces until it clears the floor above, moving only HSL lightness so the
/// scheme's own hues and saturation survive exactly. A scheme that is already
/// readable — which is nearly all of them — comes through untouched.
///
/// Tokens named directly under `tokens:` are the user's own word and are
/// taken literally; only the mechanical base mapping is repaired.
fn make_readable(tokens: &mut Tokens, authored: &BTreeSet<String>) {
    // base16 has no diff colours, so the mapping falls back to `base01` for
    // both tints and a hunk reads as one undifferentiated slab. Tint that
    // surface with the scheme's own success and error hues instead, and give
    // each tint the saturated terminal face its hue rounds to so a diff still
    // reads as a diff in sixteen colours.
    if !authored.contains("diff_added_bg") {
        tokens.diff_added_bg = tinted(tokens.diff_added_bg, tokens.ok);
    }
    if !authored.contains("diff_removed_bg") {
        tokens.diff_removed_bg = tinted(tokens.diff_removed_bg, tokens.error);
    }

    // A scheme's block surfaces are shades of its background, and rounding
    // each one to its own terminal face turns a barely-there shade into a
    // loud grey band and then constrains every foreground that lands on it.
    // Anything this close to the background is not meant to read as a colour,
    // so let it share the background's face and disappear the way it does in
    // truecolor.
    if !authored.contains("user_surface") && shade_of(tokens.user_surface, tokens.background) {
        tokens.user_surface.ansi = tokens.background.ansi;
    }
    if !authored.contains("panel") && shade_of(tokens.panel, tokens.background) {
        tokens.panel.ansi = tokens.background.ansi;
    }

    let surfaces = [tokens.background, tokens.user_surface, tokens.panel];
    let added = [tokens.diff_added_bg];
    let removed = [tokens.diff_removed_bg];
    let hunk = [
        tokens.panel,
        tokens.background,
        tokens.diff_added_bg,
        tokens.diff_removed_bg,
    ];

    let lift = |token: &mut Token, name: &str, on: &[Token], floor: f64| {
        if authored.contains(name) {
            return;
        }
        let against: Vec<(Token, f64)> = on.iter().map(|surface| (*surface, floor)).collect();
        *token = readable_token(*token, &against);
    };

    lift(&mut tokens.text, "text", &surfaces, READABLE_BODY);
    lift(
        &mut tokens.diff_context,
        "diff_context",
        &hunk,
        READABLE_BODY,
    );
    lift(&mut tokens.muted, "muted", &surfaces, READABLE_LABEL);
    lift(&mut tokens.accent, "accent", &surfaces, READABLE_LABEL);
    lift(&mut tokens.focus, "focus", &surfaces, READABLE_LABEL);
    lift(&mut tokens.code, "code", &surfaces, READABLE_LABEL);
    lift(&mut tokens.ok, "ok", &surfaces, READABLE_LABEL);
    lift(&mut tokens.warn, "warn", &surfaces, READABLE_LABEL);
    lift(&mut tokens.error, "error", &surfaces, READABLE_LABEL);
    lift(
        &mut tokens.diff_added_fg,
        "diff_added_fg",
        &added,
        READABLE_LABEL,
    );
    lift(
        &mut tokens.diff_removed_fg,
        "diff_removed_fg",
        &removed,
        READABLE_LABEL,
    );
    lift(&mut tokens.gutter, "gutter", &surfaces, READABLE_TRIM);
    lift(&mut tokens.diff_meta, "diff_meta", &hunk, READABLE_TRIM);

    // Emphasis is only emphasis if it out-reads the body it interrupts, and
    // lifting two flat greys to the same floor would land them on the same
    // colour, so hold it away from the repaired text as well.
    if !authored.contains("emphasis") {
        let against = [
            (tokens.background, READABLE_BODY),
            (tokens.user_surface, READABLE_BODY),
            (tokens.panel, READABLE_BODY),
            (tokens.text, SEPARATION),
        ];
        tokens.emphasis = readable_token(tokens.emphasis, &against);
    }
}

/// Mix a hue into a surface far enough to be seen without turning the
/// surface into a foreground. Unlike the block surfaces above, a tint is
/// meant to be seen, so it keeps a face of its own even when the mix is
/// pale: the nearest face by identity, which is the tint's hue at whichever
/// end of the ramp the mix landed on.
fn tinted(surface: Token, hue: Token) -> Token {
    const MIX: f64 = 0.22;
    let blend = |surface: u8, hue: u8| {
        (f64::from(surface) * (1.0 - MIX) + f64::from(hue) * MIX).round() as u8
    };
    let rgb = (
        blend(surface.rgb.0, hue.rgb.0),
        blend(surface.rgb.1, hue.rgb.1),
        blend(surface.rgb.2, hue.rgb.2),
    );
    let ansi = ANSI_FACES
        .into_iter()
        .min_by(|left, right| {
            face_identity(rgb, left.1)
                .partial_cmp(&face_identity(rgb, right.1))
                .expect("face cost is finite")
        })
        .map(|(face, _)| face)
        .expect("ANSI palette is non-empty");
    Token { rgb, ansi }
}

/// Whether one surface is close enough to the background to read as the same
/// surface rather than as a block of its own.
fn shade_of(surface: Token, background: Token) -> bool {
    const SHADE: f64 = 1.5;
    contrast(surface.rgb, background.rgb) < SHADE
}

/// Lift one token until it clears every floor it was given, in both colour
/// modes.
fn readable_token(token: Token, against: &[(Token, f64)]) -> Token {
    let rgb = lift_lightness(token.rgb, against);
    Token {
        rgb,
        ansi: readable_face(rgb, against),
    }
}

/// The smallest ratio of achieved contrast to demanded contrast across every
/// surface: at or above 1.0 the colour is readable everywhere it can land.
fn margin(
    candidate: (u8, u8, u8),
    against: &[(Token, f64)],
    surface: fn(Token) -> (u8, u8, u8),
) -> f64 {
    against
        .iter()
        .map(|(token, floor)| contrast(candidate, surface(*token)) / floor)
        .fold(f64::INFINITY, f64::min)
}

/// Move a colour's HSL lightness — and nothing else — until it is readable.
fn lift_lightness(rgb: (u8, u8, u8), against: &[(Token, f64)]) -> (u8, u8, u8) {
    let reach = |candidate| margin(candidate, against, |token| token.rgb);
    if reach(rgb) >= 1.0 {
        return rgb;
    }
    let (hue, saturation, lightness) = to_hsl(rgb);
    let toward = if reach(from_hsl(hue, saturation, 1.0)) >= reach(from_hsl(hue, saturation, 0.0)) {
        1.0
    } else {
        0.0
    };
    // Binary search the least movement that reads, keeping `far` as the end
    // known to be no worse than the extreme this scheme can reach.
    let (mut near, mut far) = (lightness, toward);
    for _ in 0..24 {
        let middle = (near + far) / 2.0;
        if reach(from_hsl(hue, saturation, middle)) >= 1.0 {
            far = middle;
        } else {
            near = middle;
        }
    }
    from_hsl(hue, saturation, far)
}

/// Choose the sixteen-colour face that keeps the most of a repaired colour.
///
/// Rounding each colour to its nearest RGB neighbour on its own is what made
/// imported schemes vanish on a monochrome terminal — a whole dark ramp
/// collapses onto black — and nearest-RGB also throws hues away, because a
/// half-saturated lavender is closer to grey than to magenta. Score every face
/// on what it preserves instead: a saturated colour has to stay a coloured
/// face and a neutral one a grey face, then the nearer hue wins, then the
/// nearer lightness. Falling short of the contrast floor is a penalty rather
/// than a veto, since in this mode the terminal owns the colours it actually
/// paints and the floor is measured against conventional values; a face may
/// win by a small shortfall if it keeps the hue, never by a large one.
fn readable_face(rgb: (u8, u8, u8), against: &[(Token, f64)]) -> Color {
    const SHORTFALL: f64 = 4.0;
    let reach = |candidate: (u8, u8, u8)| margin(candidate, against, |token| ansi_rgb(token.ansi));

    // Against a coloured surface — a diff tint — the surface already says what
    // the row is, and a second hue laid over it only clashes. Take the face
    // that reads best there and let it be neutral, which is what the shipped
    // palettes do with their own tints.
    if against
        .iter()
        .any(|(surface, _)| chromatic(ansi_rgb(surface.ansi)))
    {
        return ANSI_FACES
            .into_iter()
            .max_by(|left, right| {
                reach(left.1)
                    .partial_cmp(&reach(right.1))
                    .expect("contrast is finite")
                    .then_with(|| {
                        face_identity(rgb, right.1)
                            .partial_cmp(&face_identity(rgb, left.1))
                            .expect("face cost is finite")
                    })
            })
            .map(|(face, _)| face)
            .expect("ANSI palette is non-empty");
    }

    let cost = |candidate: (u8, u8, u8)| {
        face_identity(rgb, candidate) + (1.0 - reach(candidate)).max(0.0) * SHORTFALL
    };
    ANSI_FACES
        .into_iter()
        .min_by(|left, right| {
            cost(left.1)
                .partial_cmp(&cost(right.1))
                .expect("face cost is finite")
        })
        .map(|(face, _)| face)
        .expect("ANSI palette is non-empty")
}

/// Whether a colour reads as a hue rather than as a grey.
fn chromatic(rgb: (u8, u8, u8)) -> bool {
    /// Below this, a colour reads as a grey rather than as a hue.
    const CHROMATIC: f64 = 0.2;
    to_hsl(rgb).1 >= CHROMATIC
}

/// How much of a colour's identity a terminal face gives up, in the same
/// units as the contrast shortfall above.
fn face_identity(rgb: (u8, u8, u8), face: (u8, u8, u8)) -> f64 {
    let (hue, _, lightness) = to_hsl(rgb);
    let (face_hue, _, face_lightness) = to_hsl(face);
    let coloured = chromatic(rgb);
    let face_coloured = chromatic(face);
    let class = f64::from(u8::from(coloured != face_coloured));
    let gap = if coloured && face_coloured {
        let gap = (hue - face_hue).abs().rem_euclid(360.0);
        gap.min(360.0 - gap) / 180.0
    } else {
        0.0
    };
    class + gap * 1.5 + (lightness - face_lightness).abs() * 0.5
}

/// Relative luminance, per WCAG 2.
pub(crate) fn luminance(rgb: (u8, u8, u8)) -> f64 {
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

/// The WCAG 2 contrast ratio between two colours, from 1.0 to 21.0.
pub(crate) fn contrast(left: (u8, u8, u8), right: (u8, u8, u8)) -> f64 {
    let (left, right) = (luminance(left), luminance(right));
    let (lighter, darker) = if left > right {
        (left, right)
    } else {
        (right, left)
    };
    (lighter + 0.05) / (darker + 0.05)
}

fn to_hsl(rgb: (u8, u8, u8)) -> (f64, f64, f64) {
    let (red, green, blue) = (
        f64::from(rgb.0) / 255.0,
        f64::from(rgb.1) / 255.0,
        f64::from(rgb.2) / 255.0,
    );
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let lightness = (max + min) / 2.0;
    let range = max - min;
    if range <= f64::EPSILON {
        return (0.0, 0.0, lightness);
    }
    let saturation = range / (1.0 - (2.0 * lightness - 1.0).abs());
    let hue = if max == red {
        ((green - blue) / range).rem_euclid(6.0)
    } else if max == green {
        (blue - red) / range + 2.0
    } else {
        (red - green) / range + 4.0
    };
    (hue * 60.0, saturation, lightness)
}

fn from_hsl(hue: f64, saturation: f64, lightness: f64) -> (u8, u8, u8) {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue.rem_euclid(360.0) / 60.0;
    let middle = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let (red, green, blue) = match sector as u32 {
        0 => (chroma, middle, 0.0),
        1 => (middle, chroma, 0.0),
        2 => (0.0, chroma, middle),
        3 => (0.0, middle, chroma),
        4 => (middle, 0.0, chroma),
        _ => (chroma, 0.0, middle),
    };
    let base = lightness - chroma / 2.0;
    let quantize = |value: f64| ((value + base) * 255.0).round().clamp(0.0, 255.0) as u8;
    (quantize(red), quantize(green), quantize(blue))
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

/// The sixteen terminal faces at their conventional RGB values, which is the
/// only ground amux has for reasoning about contrast in a mode where the
/// terminal, not amux, owns the actual colours.
const ANSI_FACES: [(Color, (u8, u8, u8)); 16] = [
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

/// The conventional RGB value of one terminal face.
fn ansi_rgb(face: Color) -> (u8, u8, u8) {
    ANSI_FACES
        .into_iter()
        .find(|(candidate, _)| *candidate == face)
        .map(|(_, rgb)| rgb)
        .unwrap_or((0, 0, 0))
}

/// Choose the nearest named 16-colour terminal face by squared RGB distance.
pub fn nearest_ansi(rgb: (u8, u8, u8)) -> Color {
    ANSI_FACES
        .into_iter()
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
    use crate::fixtures::{NamedState, fixture};
    use crate::{FrameContext, render};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    use super::*;

    const BASE16_SAMPLE: &str = include_str!("../tests/themes/base16-sample.yaml");
    const BASE24_SAMPLE: &str = include_str!("../tests/themes/base24-sample.yaml");

    fn render_claude_working(theme: Theme) -> Buffer {
        let fixture = fixture(NamedState::ClaudeWorking);
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("theme test terminal");
        let context = FrameContext {
            viewport: (120, 40),
            theme,
            now: fixture.now,
        };
        terminal
            .draw(|frame| render(&fixture.model, &fixture.view, &context, frame))
            .expect("render ClaudeWorking theme proof");
        terminal.backend().buffer().clone()
    }

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

    fn hue_and_saturation(rgb: (u8, u8, u8)) -> (f64, f64) {
        let (hue, saturation, _) = to_hsl(rgb);
        (hue, saturation)
    }

    /// Every semantic family comes from the base16 slot the mapping table
    /// promises. The readability pass may move a colour's lightness, so each
    /// family is identified by the hue and saturation it kept, which is the
    /// part of an imported scheme a user would recognise.
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
        for (token, base, family) in [
            (theme.tokens.muted, (0x40, 0x40, 0x40), "muted"),
            (theme.tokens.diff_meta, (0x50, 0x50, 0x50), "diff_meta"),
            (theme.tokens.text, (0x60, 0x60, 0x60), "text"),
            (theme.tokens.emphasis, (0x70, 0x70, 0x70), "emphasis"),
            (theme.tokens.error, (0x90, 0x00, 0x00), "error"),
            (theme.tokens.warn, (0xa0, 0x60, 0x00), "warn"),
            (theme.tokens.ok, (0x00, 0x90, 0x00), "ok"),
            (theme.tokens.code, (0x00, 0x80, 0x90), "code"),
            (theme.tokens.accent, (0x00, 0x60, 0xa0), "accent"),
            (theme.tokens.focus, (0x70, 0x40, 0xa0), "focus"),
        ] {
            let (hue, saturation) = hue_and_saturation(token.rgb);
            let (base_hue, base_saturation) = hue_and_saturation(base);
            assert!(
                (hue - base_hue).abs() < 1.0 && (saturation - base_saturation).abs() < 0.02,
                "{family} lost its imported colour: {:?} is not {base:?}",
                token.rgb
            );
        }
    }

    /// A base24 scheme's extended accents win over the base16 ones it also
    /// carries, and a direct override wins over both. The two fixtures give
    /// each family the same hue in both ranges, so the extended slot is
    /// identified by the saturation only it has.
    #[test]
    fn base24_sample_uses_bright_accents_and_applies_overrides_last() {
        let file = parse_theme_file(BASE24_SAMPLE).expect("parse base24 fixture");
        assert_eq!(file.variant, Some(Variant::Light));
        assert_eq!(file.base.len(), 24);

        let theme = theme_from_file(&file, ColorMode::Ansi).expect("resolve base24 fixture");
        assert_eq!(theme.mode, ColorMode::Ansi);
        for (token, extended, base16, family) in [
            (
                theme.tokens.error,
                (0xff, 0x40, 0x40),
                (0xb0, 0x30, 0x30),
                "error",
            ),
            (
                theme.tokens.ok,
                (0x20, 0xc0, 0x20),
                (0x30, 0x80, 0x30),
                "ok",
            ),
            (
                theme.tokens.warn,
                (0xd0, 0x90, 0x20),
                (0xb0, 0x60, 0x20),
                "warn",
            ),
            (
                theme.tokens.code,
                (0x30, 0xa0, 0xa0),
                (0x20, 0x80, 0x80),
                "code",
            ),
            (
                theme.tokens.focus,
                (0xa0, 0x60, 0xc0),
                (0x70, 0x40, 0x90),
                "focus",
            ),
        ] {
            let (hue, saturation) = hue_and_saturation(token.rgb);
            let (extended_hue, extended_saturation) = hue_and_saturation(extended);
            assert!(
                (hue - extended_hue).abs() < 1.0 && (saturation - extended_saturation).abs() < 0.02,
                "{family} did not come from the extended range: {:?}",
                token.rgb
            );
            let (_, base16_saturation) = hue_and_saturation(base16);
            assert!(
                (saturation - base16_saturation).abs() > 0.02,
                "{family} is indistinguishable from its base16 slot"
            );
        }

        // A token named directly is the user's own word, so it is taken
        // literally in both faces rather than repaired.
        assert_eq!(theme.tokens.accent.rgb, (0xab, 0xcd, 0xef));
        assert_eq!(theme.tokens.accent.ansi, nearest_ansi((0xab, 0xcd, 0xef)));
    }

    #[test]
    fn base24_direct_override_reaches_the_rendered_frame() {
        let file = parse_theme_file(BASE24_SAMPLE).expect("parse base24 fixture");
        let theme = theme_from_file(&file, ColorMode::TrueColor).expect("resolve base24 fixture");
        let buffer = render_claude_working(theme);

        assert!(
            buffer
                .content()
                .iter()
                .any(|cell| cell.fg == Color::Rgb(0xab, 0xcd, 0xef)),
            "ClaudeWorking should paint its accent bars from the direct accent override"
        );
    }

    #[test]
    fn imported_ansi_theme_puts_no_rgb_colours_in_the_rendered_buffer() {
        let file = parse_theme_file(BASE16_SAMPLE).expect("parse base16 fixture");
        let theme = theme_from_file(&file, ColorMode::Ansi).expect("resolve ANSI base16 fixture");
        let buffer = render_claude_working(theme);

        for (index, cell) in buffer.content().iter().enumerate() {
            assert!(
                !matches!(cell.fg, Color::Rgb(..)),
                "cell {index} has RGB foreground {:?}",
                cell.fg
            );
            assert!(
                !matches!(cell.bg, Color::Rgb(..)),
                "cell {index} has RGB background {:?}",
                cell.bg
            );
        }
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

    // --- imported palettes ---------------------------------------------

    fn imported(yaml: &str, mode: ColorMode) -> Theme {
        let file = parse_theme_file(yaml).expect("parse imported fixture");
        theme_from_file(&file, mode).expect("resolve imported fixture")
    }

    /// The same scheme with its direct overrides dropped. A token named under
    /// `tokens:` is the user's own word and is taken literally, so only the
    /// base mapping carries a readability guarantee.
    fn imported_mapping(yaml: &str, mode: ColorMode) -> Theme {
        let mut file = parse_theme_file(yaml).expect("parse imported fixture");
        file.tokens.clear();
        theme_from_file(&file, mode).expect("resolve imported fixture")
    }

    /// Every foreground an imported scheme supplies has to read on every
    /// surface it can land on, whatever the scheme's author thought was
    /// enough contrast for an editor.
    #[test]
    fn imported_palettes_are_lifted_until_they_read() {
        for yaml in [BASE16_SAMPLE, BASE24_SAMPLE] {
            let tokens = imported_mapping(yaml, ColorMode::TrueColor).tokens;
            let surfaces = [
                (tokens.background.rgb, "background"),
                (tokens.user_surface.rgb, "user surface"),
                (tokens.panel.rgb, "panel"),
            ];
            for (fg, floor, family) in [
                (tokens.text, READABLE_BODY, "text"),
                (tokens.emphasis, READABLE_BODY, "emphasis"),
                (tokens.muted, READABLE_LABEL, "muted"),
                (tokens.accent, READABLE_LABEL, "accent"),
                (tokens.focus, READABLE_LABEL, "focus"),
                (tokens.code, READABLE_LABEL, "code"),
                (tokens.ok, READABLE_LABEL, "ok"),
                (tokens.warn, READABLE_LABEL, "warn"),
                (tokens.error, READABLE_LABEL, "error"),
                (tokens.gutter, READABLE_TRIM, "gutter"),
            ] {
                for (surface, name) in surfaces {
                    let ratio = contrast(fg.rgb, surface);
                    assert!(
                        ratio >= floor,
                        "imported {family} on {name} is only {ratio:.1}:1"
                    );
                }
            }
            for (fg, bg, name) in [
                (tokens.diff_added_fg, tokens.diff_added_bg, "added"),
                (tokens.diff_removed_fg, tokens.diff_removed_bg, "removed"),
            ] {
                let ratio = contrast(fg.rgb, bg.rgb);
                assert!(
                    ratio >= READABLE_LABEL,
                    "imported diff {name} is only {ratio:.1}:1"
                );
            }
            assert!(
                contrast(tokens.emphasis.rgb, tokens.text.rgb) >= SEPARATION,
                "imported emphasis is the same colour as its body text"
            );
            assert_ne!(
                tokens.diff_added_bg.rgb, tokens.diff_removed_bg.rgb,
                "imported diff tints are the same colour"
            );
        }
    }

    /// The same has to hold once the palette is rounded to sixteen faces,
    /// which is where an imported dark scheme used to disappear onto black.
    #[test]
    fn imported_ansi_faces_read_against_the_surfaces_they_land_on() {
        for yaml in [BASE16_SAMPLE, BASE24_SAMPLE] {
            let tokens = imported_mapping(yaml, ColorMode::Ansi).tokens;
            let surfaces = [tokens.background, tokens.user_surface, tokens.panel];
            for (fg, family, on) in [
                (tokens.text, "text", &surfaces[..]),
                (tokens.emphasis, "emphasis", &surfaces[..]),
                (tokens.muted, "muted", &surfaces[..]),
                (tokens.focus, "focus", &surfaces[..]),
                (tokens.code, "code", &surfaces[..]),
                (tokens.ok, "ok", &surfaces[..]),
                (tokens.warn, "warn", &surfaces[..]),
                (tokens.error, "error", &surfaces[..]),
                (tokens.gutter, "gutter", &surfaces[..]),
                (
                    tokens.diff_added_fg,
                    "diff added",
                    &[tokens.diff_added_bg][..],
                ),
                (
                    tokens.diff_removed_fg,
                    "diff removed",
                    &[tokens.diff_removed_bg][..],
                ),
            ] {
                for surface in on {
                    let ratio = contrast(ansi_rgb(fg.ansi), ansi_rgb(surface.ansi));
                    assert!(
                        ratio >= READABLE_TRIM,
                        "imported {family} reads {ratio:.1}:1 as {:?} on {:?}",
                        fg.ansi,
                        surface.ansi
                    );
                }
            }
        }
    }

    /// The palettes are only a claim until something paints with them: walk
    /// the cells of a real frame and hold every painted glyph to the floor
    /// below. The shipped palettes are held here in truecolor only — in
    /// sixteen colours their block surfaces round onto the background face
    /// and muted text inside a block goes invisible, which the style-map
    /// classifier depends on and so has to be repaired on its own.
    #[test]
    fn every_theme_paints_a_readable_frame() {
        for theme in [
            Theme::dark(ColorMode::TrueColor),
            Theme::light(ColorMode::TrueColor),
            imported_mapping(BASE16_SAMPLE, ColorMode::TrueColor),
            imported_mapping(BASE16_SAMPLE, ColorMode::Ansi),
            imported_mapping(BASE24_SAMPLE, ColorMode::TrueColor),
            imported_mapping(BASE24_SAMPLE, ColorMode::Ansi),
        ] {
            let surface = match theme.mode {
                ColorMode::TrueColor => theme.tokens.background.rgb,
                ColorMode::Ansi => ansi_rgb(theme.tokens.background.ansi),
            };
            let painted = |color: Color| match color {
                Color::Rgb(red, green, blue) => (red, green, blue),
                Color::Reset => surface,
                face => ansi_rgb(face),
            };
            let buffer = render_claude_working(theme);
            for cell in buffer.content() {
                if cell.symbol().trim().is_empty() {
                    continue;
                }
                let ratio = contrast(painted(cell.fg), painted(cell.bg));
                assert!(
                    ratio >= READABLE_TRIM,
                    "{:?} {:?} paints {:?} at {ratio:.1}:1, {:?} on {:?}",
                    theme.name,
                    theme.mode,
                    cell.symbol(),
                    cell.fg,
                    cell.bg
                );
            }
        }
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
