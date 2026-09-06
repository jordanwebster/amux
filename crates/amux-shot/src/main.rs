use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use amux_shot::{
    ShotError, append_set, record_draft, record_review, record_scroll, render_to_path, verify,
};
use amux_tui::fixtures::{NamedState, all_states};
use amux_tui::{ColorMode, Theme, parse_theme_file, theme_from_file};
use amux_ui::StructuredProtocol;
use clap::{Parser, Subcommand, ValueEnum};

const SET_NAMES: &[&str] = &[
    "chat",
    "agent-specific",
    "gallery",
    "scroll",
    "copy",
    "collapse",
    "attachments",
    "review",
    "themes",
    "fleet",
    "parity",
    "profiles",
    "all",
];

#[derive(Debug, Parser)]
#[command(about = "Render deterministic 120x40 PNGs of named amux TUI states")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List registered fixture states and declared render sets.
    List,
    /// Render one named state to a PNG.
    Render {
        state: String,
        #[arg(long, default_value = "dark")]
        theme: String,
        #[arg(long, value_enum, default_value_t = ColorArg::Truecolor)]
        color: ColorArg,
        #[arg(long)]
        out: PathBuf,
    },
    /// Render every member of a declared set.
    RenderSet {
        set: String,
        #[arg(long, default_value = "target/amux-shot")]
        out: PathBuf,
    },
    /// Record twelve wheel-up and twelve wheel-down events as an animated GIF.
    RecordScroll {
        #[arg(value_enum)]
        agent: AgentArg,
        #[arg(long, default_value = "target/amux-shot")]
        out: PathBuf,
    },
    /// Record the trip through a review as an animated GIF.
    RecordReview {
        #[arg(long, default_value = "target/amux-shot")]
        out: PathBuf,
    },
    /// Record the trip through a draft carrying attachments as an animated GIF.
    RecordDraft {
        #[arg(long, default_value = "target/amux-shot")]
        out: PathBuf,
    },
    /// Verify PNG dimensions, hashes, decoding, and completed sets.
    Verify { dir: PathBuf },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ColorArg {
    Truecolor,
    Ansi,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AgentArg {
    Claude,
    ClaudeSdk,
    Codex,
}

impl From<AgentArg> for StructuredProtocol {
    fn from(value: AgentArg) -> Self {
        match value {
            AgentArg::Claude => Self::Claude,
            AgentArg::ClaudeSdk => Self::ClaudeSdk,
            AgentArg::Codex => Self::Codex,
        }
    }
}

impl From<ColorArg> for ColorMode {
    fn from(value: ColorArg) -> Self {
        match value {
            ColorArg::Truecolor => Self::TrueColor,
            ColorArg::Ansi => Self::Ansi,
        }
    }
}

#[derive(Clone, Copy)]
enum ThemeSpec {
    Dark,
    Light,
    Base16Sample,
}

#[derive(Clone, Copy)]
struct SetMember {
    state: &'static str,
    file: &'static str,
    theme: ThemeSpec,
    color: ColorMode,
}

const CHAT: &[SetMember] = &[
    member("claude-idle", "claude-idle-dark.png", ThemeSpec::Dark),
    member("claude-idle", "claude-idle-light.png", ThemeSpec::Light),
    member("claude-working", "claude-working-dark.png", ThemeSpec::Dark),
    member(
        "claude-working",
        "claude-working-light.png",
        ThemeSpec::Light,
    ),
    member("claude-sdk-idle", "claude-sdk-idle.png", ThemeSpec::Dark),
    member(
        "claude-sdk-idle",
        "claude-sdk-idle-light.png",
        ThemeSpec::Light,
    ),
    member(
        "claude-sdk-streaming",
        "claude-sdk-streaming.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-sdk-streaming",
        "claude-sdk-streaming-light.png",
        ThemeSpec::Light,
    ),
    member("codex-idle", "codex-idle-dark.png", ThemeSpec::Dark),
    member("codex-idle", "codex-idle-light.png", ThemeSpec::Light),
    member("codex-working", "codex-working-dark.png", ThemeSpec::Dark),
    member("codex-working", "codex-working-light.png", ThemeSpec::Light),
];

const AGENT_SPECIFIC: &[SetMember] = &[
    member(
        "claude-permission-ask",
        "claude-permission-ask-dark.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-plan-reader",
        "claude-plan-reader-dark.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-diff-reader",
        "claude-diff-reader-dark.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-sdk-permission-ask",
        "claude-sdk-permission-ask.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-sdk-plan-reader",
        "claude-sdk-plan-reader.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-sdk-question-ask",
        "claude-sdk-question-ask.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-sdk-elicitation",
        "claude-sdk-elicitation.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-sdk-dialog",
        "claude-sdk-dialog.png",
        ThemeSpec::Dark,
    ),
    member("claude-sdk-tasks", "claude-sdk-tasks.png", ThemeSpec::Dark),
    member(
        "claude-sdk-context",
        "claude-sdk-context.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-sdk-context-breakdown",
        "claude-sdk-context-breakdown.png",
        ThemeSpec::Dark,
    ),
    member("codex-approval", "codex-approval-dark.png", ThemeSpec::Dark),
    member(
        "codex-network-policy",
        "codex-network-policy-dark.png",
        ThemeSpec::Dark,
    ),
    member(
        "codex-mcp-startup",
        "codex-mcp-startup-dark.png",
        ThemeSpec::Dark,
    ),
];

const GALLERY: &[SetMember] = &[
    member(
        "component-gallery",
        "component-gallery-dark.png",
        ThemeSpec::Dark,
    ),
    member(
        "component-gallery",
        "component-gallery-light.png",
        ThemeSpec::Light,
    ),
    member(
        "component-gallery-codex",
        "component-gallery-codex-dark.png",
        ThemeSpec::Dark,
    ),
    member(
        "component-gallery-codex",
        "component-gallery-codex-light.png",
        ThemeSpec::Light,
    ),
];

const SCROLL: &[SetMember] = &[
    member(
        "claude-long-feed",
        "claude-following-dark.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-scrolled-back",
        "claude-scrolled-back-dark.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-sdk-long-feed",
        "claude-sdk-following-dark.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-sdk-scrolled-back",
        "claude-sdk-scrolled-back-dark.png",
        ThemeSpec::Dark,
    ),
    member(
        "codex-long-feed",
        "codex-following-dark.png",
        ThemeSpec::Dark,
    ),
    member(
        "codex-scrolled-back",
        "codex-scrolled-back-dark.png",
        ThemeSpec::Dark,
    ),
];

const COPY: &[SetMember] = &[member(
    "help-overlay",
    "help-overlay-dark.png",
    ThemeSpec::Dark,
)];

const COLLAPSE: &[SetMember] = &[
    member(
        "exploration-collapsed",
        "exploration-collapsed-dark.png",
        ThemeSpec::Dark,
    ),
    member(
        "exploration-expanded",
        "exploration-expanded-dark.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-sdk-exploration",
        "claude-sdk-exploration.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-sdk-exploration-expanded",
        "claude-sdk-exploration-expanded.png",
        ThemeSpec::Dark,
    ),
];

/// What a message carries besides its words: the feed's attachment rows
/// and a draft holding two attachments of different kinds at once.
const ATTACHMENTS: &[SetMember] = &[
    member(
        "chat-attachment-blocks",
        "chat-attachment-blocks-dark.png",
        ThemeSpec::Dark,
    ),
    member(
        "chat-attachment-blocks",
        "chat-attachment-blocks-light.png",
        ThemeSpec::Light,
    ),
    member(
        "chat-mixed-draft",
        "chat-mixed-draft-dark.png",
        ThemeSpec::Dark,
    ),
    member(
        "chat-mixed-draft",
        "chat-mixed-draft-light.png",
        ThemeSpec::Light,
    ),
];

/// The review page's named states, dark and light for the two a reader
/// judges the page's look from and dark alone for the rest: the set is
/// about the page's shape, and doubling every state would bury it.
const REVIEW: &[SetMember] = &[
    member("review-open", "review-open-dark.png", ThemeSpec::Dark),
    member("review-open", "review-open-light.png", ThemeSpec::Light),
    member(
        "review-selection",
        "review-selection-dark.png",
        ThemeSpec::Dark,
    ),
    member(
        "review-comment-box",
        "review-comment-box-dark.png",
        ThemeSpec::Dark,
    ),
    member("review-threads", "review-threads-dark.png", ThemeSpec::Dark),
    member(
        "review-threads",
        "review-threads-light.png",
        ThemeSpec::Light,
    ),
    member(
        "review-file-list",
        "review-file-list-dark.png",
        ThemeSpec::Dark,
    ),
    member("review-folded", "review-folded-dark.png", ThemeSpec::Dark),
    member(
        "review-branch-base",
        "review-branch-base-dark.png",
        ThemeSpec::Dark,
    ),
    member(
        "chat-review-token",
        "chat-review-token-dark.png",
        ThemeSpec::Dark,
    ),
];

const THEMES: &[SetMember] = &[
    member("claude-working", "claude-working-dark.png", ThemeSpec::Dark),
    member(
        "claude-working",
        "claude-working-light.png",
        ThemeSpec::Light,
    ),
    member(
        "claude-working",
        "claude-working-imported-base16.png",
        ThemeSpec::Base16Sample,
    ),
    SetMember {
        state: "claude-working",
        file: "claude-working-imported-base16-ansi.png",
        theme: ThemeSpec::Base16Sample,
        color: ColorMode::Ansi,
    },
];

const FLEET: &[SetMember] = &[
    member("fleet", "fleet-dark.png", ThemeSpec::Dark),
    member("fleet-mixed", "fleet-mixed.png", ThemeSpec::Dark),
    member("fleet-mixed", "fleet-mixed-light.png", ThemeSpec::Light),
    member("fleet", "fleet-light.png", ThemeSpec::Light),
    member("fleet-sdk-help", "fleet-sdk-help.png", ThemeSpec::Dark),
    member("a2a-sdk-family", "a2a-sdk-family.png", ThemeSpec::Dark),
    member("claude-idle", "claude-idle-dark.png", ThemeSpec::Dark),
];

/// The same surface across all three chats, one file per chat, named so
/// that a directory listing puts each triple together. This is the set a
/// person walks to judge whether the three read as one design.
const PARITY: &[SetMember] = &[
    member("claude-idle", "idle-claude.png", ThemeSpec::Dark),
    member("claude-sdk-idle", "idle-claude-sdk.png", ThemeSpec::Dark),
    member("codex-idle", "idle-codex.png", ThemeSpec::Dark),
    member("claude-working", "working-claude.png", ThemeSpec::Dark),
    member(
        "claude-sdk-streaming",
        "working-claude-sdk.png",
        ThemeSpec::Dark,
    ),
    member("codex-working", "working-codex.png", ThemeSpec::Dark),
    member(
        "claude-permission-ask",
        "permission-claude.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-sdk-permission-ask",
        "permission-claude-sdk.png",
        ThemeSpec::Dark,
    ),
    member("codex-approval", "permission-codex.png", ThemeSpec::Dark),
    member("claude-plan-reader", "plan-claude.png", ThemeSpec::Dark),
    member(
        "claude-sdk-plan-reader",
        "plan-claude-sdk.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-question-ask",
        "question-claude.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-sdk-question-ask",
        "question-claude-sdk.png",
        ThemeSpec::Dark,
    ),
    // Collapsed exploration is the newest shared shape, so the walk
    // compares both chats closed and both chats open: a fold that reads
    // differently on either side is the kind of drift this set exists
    // to catch. Codex has no counterpart — it never folds a run.
    member(
        "exploration-collapsed",
        "exploration-claude.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-sdk-exploration",
        "exploration-claude-sdk.png",
        ThemeSpec::Dark,
    ),
    member(
        "exploration-expanded",
        "exploration-open-claude.png",
        ThemeSpec::Dark,
    ),
    member(
        "claude-sdk-exploration-expanded",
        "exploration-open-claude-sdk.png",
        ThemeSpec::Dark,
    ),
];

/// The account switcher and the two fleets it moves between. The pair of
/// fleets is the point: the second frame is a different account's device,
/// so it shares no host and no agent with the first.
const PROFILES: &[SetMember] = &[
    member(
        "profile-switcher",
        "profile-switcher-dark.png",
        ThemeSpec::Dark,
    ),
    member("fleet", "fleet-personal-dark.png", ThemeSpec::Dark),
    member("fleet-switched", "fleet-work-dark.png", ThemeSpec::Dark),
];

const fn member(state: &'static str, file: &'static str, theme: ThemeSpec) -> SetMember {
    SetMember {
        state,
        file,
        theme,
        color: ColorMode::TrueColor,
    }
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), ShotError> {
    match cli.command {
        Command::List => list(),
        Command::Render {
            state,
            theme,
            color,
            out,
        } => {
            let state = parse_state(&state)?;
            let (theme, label) = resolve_theme_argument(&theme, color.into())?;
            let entry = render_to_path(state, theme, &label, &out)?;
            println!(
                "rendered {} ({} {}) to {}",
                entry.state,
                entry.theme,
                entry.color,
                out.display()
            );
            Ok(())
        }
        Command::RenderSet { set, out } => render_set(&set, &out),
        Command::RecordScroll { agent, out } => {
            let recording = record_scroll(agent.into(), Theme::dark(ColorMode::TrueColor), &out)?;
            println!(
                "recorded {} frames for {} to {}",
                recording.frames,
                recording.agent,
                out.join(recording.gif).display()
            );
            Ok(())
        }
        Command::RecordReview { out } => {
            let recording = record_review(&out, Theme::dark(ColorMode::TrueColor), "dark")?;
            println!(
                "recorded {} frames of {} to {}",
                recording.frames,
                recording.name,
                out.join(recording.gif).display()
            );
            Ok(())
        }
        Command::RecordDraft { out } => {
            let recording = record_draft(&out, Theme::dark(ColorMode::TrueColor), "dark")?;
            println!(
                "recorded {} frames of {} to {}",
                recording.frames,
                recording.name,
                out.join(recording.gif).display()
            );
            Ok(())
        }
        Command::Verify { dir } => {
            let manifest = verify(&dir)?;
            println!(
                "verified {} PNG entries across {} completed sets below {}",
                manifest.entries.len(),
                manifest.sets.len(),
                dir.display()
            );
            Ok(())
        }
    }
}

fn list() -> Result<(), ShotError> {
    println!("states:");
    for state in all_states() {
        println!("  {}", state.name());
    }
    println!("sets:");
    for set in SET_NAMES {
        println!("  {set}");
    }
    Ok(())
}

fn render_set(name: &str, out: &Path) -> Result<(), ShotError> {
    let members = set_members(name)?;
    let resolved = members
        .iter()
        .map(|member| Ok((*member, parse_state(member.state)?)))
        .collect::<Result<Vec<_>, ShotError>>()?;
    fs::create_dir_all(out)?;
    let mut files = Vec::with_capacity(resolved.len());
    for (member, state) in resolved {
        let (theme, label) = resolve_theme_spec(member.theme, member.color)?;
        let path = out.join(member.file);
        render_to_path(state, theme, &label, &path)?;
        println!("rendered {} to {}", member.state, path.display());
        files.push(member.file.to_string());
    }
    append_set(out, name, files)?;
    Ok(())
}

fn set_members(name: &str) -> Result<Vec<SetMember>, ShotError> {
    let direct = match name {
        "chat" => CHAT,
        "agent-specific" => AGENT_SPECIFIC,
        "gallery" => GALLERY,
        "scroll" => SCROLL,
        "copy" => COPY,
        "collapse" => COLLAPSE,
        "attachments" => ATTACHMENTS,
        "review" => REVIEW,
        "themes" => THEMES,
        "fleet" => FLEET,
        "parity" => PARITY,
        "profiles" => PROFILES,
        "all" => {
            let mut members = Vec::new();
            let mut files = BTreeSet::new();
            for set in [
                CHAT,
                AGENT_SPECIFIC,
                GALLERY,
                SCROLL,
                COPY,
                COLLAPSE,
                ATTACHMENTS,
                REVIEW,
                THEMES,
                FLEET,
                PARITY,
                PROFILES,
            ] {
                for member in set {
                    if files.insert(member.file) {
                        members.push(*member);
                    }
                }
            }
            return Ok(members);
        }
        other => return Err(ShotError::UnknownSet(other.to_string())),
    };
    Ok(direct.to_vec())
}

fn parse_state(name: &str) -> Result<NamedState, ShotError> {
    NamedState::parse(name).ok_or_else(|| ShotError::UnknownState(name.to_string()))
}

fn resolve_theme_argument(value: &str, mode: ColorMode) -> Result<(Theme, String), ShotError> {
    match value {
        "dark" => Ok((Theme::dark(mode), "dark".to_string())),
        "light" => Ok((Theme::light(mode), "light".to_string())),
        path => {
            let path = Path::new(path);
            let yaml = fs::read_to_string(path)?;
            let file = parse_theme_file(&yaml)?;
            let theme = theme_from_file(&file, mode)?;
            let label = file.scheme.unwrap_or_else(|| path.display().to_string());
            Ok((theme, label))
        }
    }
}

fn resolve_theme_spec(spec: ThemeSpec, mode: ColorMode) -> Result<(Theme, String), ShotError> {
    match spec {
        ThemeSpec::Dark => Ok((Theme::dark(mode), "dark".to_string())),
        ThemeSpec::Light => Ok((Theme::light(mode), "light".to_string())),
        ThemeSpec::Base16Sample => {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../amux-tui/tests/themes/base16-sample.yaml");
            let yaml = fs::read_to_string(path)?;
            let file = parse_theme_file(&yaml)?;
            Ok((theme_from_file(&file, mode)?, "imported-base16".to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use amux_shot::Manifest;
    use tempfile::tempdir;

    use super::{SET_NAMES, render_set, set_members};

    #[test]
    fn every_declared_set_has_members() {
        for set in SET_NAMES {
            assert!(!set_members(set).unwrap().is_empty(), "{set}");
        }
    }

    #[test]
    fn the_scroll_set_names_states_that_exist() {
        for member in set_members("scroll").unwrap() {
            super::parse_state(member.state).unwrap();
        }
    }

    /// The switcher set is what the screenshots of account switching come
    /// from, so a renamed fixture must fail here rather than at capture time.
    #[test]
    fn the_switcher_set_names_states_that_exist() {
        for member in set_members("profiles").unwrap() {
            super::parse_state(member.state).unwrap();
        }
    }

    #[test]
    fn the_gallery_collapse_and_parity_sets_name_states_that_exist() {
        for set in ["gallery", "collapse", "parity"] {
            for member in set_members(set).unwrap() {
                super::parse_state(member.state).unwrap_or_else(|error| panic!("{set}: {error:?}"));
            }
        }
    }

    /// A capture record is read by a person hunting for the screenshot
    /// they need, so its `state` and `file` are the fixture registry's own
    /// handles and must name the surface they show — `claude-sdk-idle` is
    /// how somebody finds that PNG. Everything else a manifest carries is
    /// vocabulary this tool chose: a set name, a theme label. None of it
    /// may name how an agent is driven, because those names travel with
    /// the captures into whatever a reviewer reads next.
    #[test]
    fn backend_vocabulary_stays_out_of_screenshot_manifests() {
        const MACHINERY_WORDS: &[&str] = &["sdk", "pty", "driver", "backend", "unsupported"];
        let says_machinery = |text: &str| -> Vec<String> {
            text.split(|character: char| !character.is_ascii_alphanumeric())
                .map(str::to_ascii_lowercase)
                .filter(|word| MACHINERY_WORDS.contains(&word.as_str()))
                .collect()
        };

        for set in SET_NAMES {
            assert!(
                says_machinery(set).is_empty(),
                "the render set {set} is named after how an agent is driven"
            );
            for member in set_members(set).unwrap() {
                let (_, label) = super::resolve_theme_spec(member.theme, member.color).unwrap();
                assert!(
                    says_machinery(&label).is_empty(),
                    "the theme {label} in set {set} is named after how an agent is driven"
                );
                let color = match member.color {
                    amux_tui::ColorMode::TrueColor => "truecolor",
                    amux_tui::ColorMode::Ansi => "ansi",
                };
                assert!(says_machinery(color).is_empty(), "{color}");
            }
        }
    }

    #[test]
    fn theme_set_manifest_records_the_forced_ansi_render() {
        let directory = tempdir().expect("theme set tempdir");
        render_set("themes", directory.path()).expect("render theme set");
        let manifest: Manifest = serde_json::from_slice(
            &fs::read(directory.path().join("manifest.json")).expect("read theme manifest"),
        )
        .expect("parse theme manifest");

        let ansi = manifest
            .entries
            .iter()
            .find(|entry| entry.file == "claude-working-imported-base16-ansi.png")
            .expect("ANSI theme entry");
        assert_eq!(ansi.theme, "imported-base16");
        assert_eq!(ansi.color, "ansi");
        assert_eq!(manifest.sets[0].name, "themes");
    }
}
