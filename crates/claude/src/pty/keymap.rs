//! Versioned data that parameterizes Claude Code PTY input programs.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    pub name: String,
    pub applies_to: VersionReq,
    pub verified: Vec<VerifiedVersion>,
    pub provenance: Provenance,
    pub keys: BTreeMap<KeyName, Vec<u8>>,
    pub delays: BTreeMap<DelayName, u32>,
    pub menus: BTreeMap<MenuName, MenuLayout>,
    pub verified_shapes: BTreeMap<ProgramName, ShapeSet>,
    pub programs: BTreeMap<ProgramName, Program>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedVersion {
    pub version: Version,
    pub run_id: String,
    pub spec: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub recorded_version: Version,
    pub model: String,
    pub dates: Vec<String>,
    pub specs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MenuLayout {
    pub entries: BTreeMap<MenuEntryName, u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShapeSet {
    #[serde(default)]
    pub permission_suggestions: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    pub stability: Stability,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stability {
    Stable,
    Menu,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    Key {
        key: KeyName,
    },
    Paste {
        text: TextSource,
    },
    Type {
        text: TextSource,
    },
    Digit {
        digit: DigitSource,
    },
    Delay {
        delay: DelayName,
    },
    Repeat {
        count: CountSource,
        steps: Vec<Step>,
    },
    ForEach {
        over: Iter,
        steps: Vec<Step>,
    },
    If {
        cond: Cond,
        #[serde(rename = "then")]
        then_steps: Vec<Step>,
        #[serde(default)]
        otherwise: Vec<Step>,
    },
    MoveTo {
        row: RowSource,
    },
    Call {
        program: ProgramName,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum DigitSource {
    MenuEntry {
        menu: MenuName,
        entry: MenuEntryName,
    },
    SelectedOption,
    OtherRow,
    Suggestion,
}

macro_rules! string_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }
    };
}

string_enum!(KeyName {
    Escape,
    Enter,
    Tab,
    Space,
    Down,
    ShiftTab,
    PasteBegin,
    PasteEnd,
});

string_enum!(DelayName {
    AfterPaste,
    AfterToggle,
    AfterMove,
    AfterTab,
    AfterEditorOpen,
    BeforeSubmit,
    AfterReviewOpen,
    AfterDeny,
    AfterMenuTextOpen,
    AfterOtherSave,
});

string_enum!(MenuName { Permission, Plan });

string_enum!(MenuEntryName {
    AllowOnce,
    AllowScoped,
    Deny,
    ApproveAuto,
    ApproveManual,
    RequestChanges,
});

string_enum!(ProgramName {
    Prompt,
    Interrupt,
    ModeCycle,
    PermissionMenu,
    PlanMenu,
    QuestionForm,
});

string_enum!(TextSource {
    PromptText,
    DenyFeedback,
    PlanFeedback,
    OtherText,
});

string_enum!(CountSource {
    Questions,
    SelectedOptions,
});

string_enum!(Iter {
    Questions,
    SelectedOptions,
});

string_enum!(Cond {
    HasFeedback,
    HasOther,
    MultiSelect,
    IsFirst,
    LastQuestionMulti,
    SingleQuestionSingleSelect,
});

string_enum!(RowSource {
    SelectedOption,
    OtherRow,
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeymapSource {
    Baked,
    User(PathBuf),
}

#[derive(Debug, thiserror::Error)]
pub enum KeymapError {
    #[error("could not read keymap '{0}': {1}")]
    Io(PathBuf, #[source] io::Error),
    #[error("could not parse keymap '{origin}': {reason}")]
    Parse { origin: String, reason: String },
    #[error("unknown keymap program '{0}'")]
    UnknownProgram(String),
    #[error("unknown keymap step '{0}'")]
    UnknownStep(String),
    #[error("unknown keymap key '{0}'")]
    UnknownKey(String),
    #[error("invalid keymap version range '{0}'")]
    BadRange(String),
    #[error("user keymap '{origin}' contains hand-authored verified versions")]
    HandVerified { origin: String },
    #[error("program table violates the fixed intent mapping at '{program}'")]
    ProgramTableViolation { program: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKeymap {
    name: String,
    applies_to: String,
    verified: Vec<VerifiedVersion>,
    provenance: Provenance,
    keys: BTreeMap<KeyName, Vec<u8>>,
    delays: BTreeMap<DelayName, u32>,
    menus: BTreeMap<MenuName, MenuLayout>,
    verified_shapes: BTreeMap<ProgramName, ShapeSet>,
    programs: BTreeMap<ProgramName, Program>,
}

pub fn load(path: &Path, source: KeymapSource) -> Result<Keymap, KeymapError> {
    let contents = std::fs::read_to_string(path)
        .map_err(|error| KeymapError::Io(path.to_path_buf(), error))?;
    load_str(&contents, &path.display().to_string(), source)
}

pub fn load_str(source: &str, origin: &str, kind: KeymapSource) -> Result<Keymap, KeymapError> {
    let raw: RawKeymap = toml::from_str(source).map_err(|error| KeymapError::Parse {
        origin: origin.to_owned(),
        reason: error.to_string(),
    })?;

    if matches!(kind, KeymapSource::User(_)) && !raw.verified.is_empty() {
        return Err(KeymapError::HandVerified {
            origin: origin.to_owned(),
        });
    }

    let applies_to = raw
        .applies_to
        .parse::<VersionReq>()
        .map_err(|error| KeymapError::Parse {
            origin: origin.to_owned(),
            reason: format!("field 'applies_to' is invalid: {error}"),
        })?;

    validate_references(&raw, origin)?;

    Ok(Keymap {
        name: raw.name,
        applies_to,
        verified: raw.verified,
        provenance: raw.provenance,
        keys: raw.keys,
        delays: raw.delays,
        menus: raw.menus,
        verified_shapes: raw.verified_shapes,
        programs: raw.programs,
    })
}

fn validate_references(raw: &RawKeymap, origin: &str) -> Result<(), KeymapError> {
    for (program_name, program) in &raw.programs {
        validate_steps(
            &program.steps,
            raw,
            origin,
            &format!("programs.{program_name:?}.steps"),
        )?;
    }
    Ok(())
}

fn validate_steps(
    steps: &[Step],
    raw: &RawKeymap,
    origin: &str,
    field: &str,
) -> Result<(), KeymapError> {
    for (index, step) in steps.iter().enumerate() {
        let field = format!("{field}[{index}]");
        match step {
            Step::Key { key } if !raw.keys.contains_key(key) => {
                return reference_error(origin, &format!("{field}.key"), key);
            }
            Step::Delay { delay } if !raw.delays.contains_key(delay) => {
                return reference_error(origin, &format!("{field}.delay"), delay);
            }
            Step::Digit {
                digit: DigitSource::MenuEntry { menu, entry },
            } => {
                let Some(layout) = raw.menus.get(menu) else {
                    return reference_error(origin, &format!("{field}.digit.menu"), menu);
                };
                if !layout.entries.contains_key(entry) {
                    return reference_error(origin, &format!("{field}.digit.entry"), entry);
                }
            }
            Step::Call { program } if !raw.programs.contains_key(program) => {
                return reference_error(origin, &format!("{field}.program"), program);
            }
            Step::Repeat { steps, .. } | Step::ForEach { steps, .. } => {
                validate_steps(steps, raw, origin, &format!("{field}.steps"))?;
            }
            Step::If {
                then_steps,
                otherwise,
                ..
            } => {
                validate_steps(then_steps, raw, origin, &format!("{field}.then"))?;
                validate_steps(otherwise, raw, origin, &format!("{field}.otherwise"))?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn reference_error<T: std::fmt::Debug>(
    origin: &str,
    field: &str,
    value: &T,
) -> Result<(), KeymapError> {
    Err(KeymapError::Parse {
        origin: origin.to_owned(),
        reason: format!("field '{field}' references missing value '{value:?}'"),
    })
}

#[cfg(test)]
mod format {
    use super::*;

    const BAKED_ORIGIN: &str = "keymaps/claude-2.1.toml";
    const BAKED: &str = include_str!("../../keymaps/claude-2.1.toml");

    fn user_source() -> KeymapSource {
        KeymapSource::User(PathBuf::from("user.toml"))
    }

    fn assert_parse_mentions(source: &str, needle: &str) {
        let error = load_str(source, "broken.toml", KeymapSource::Baked)
            .expect_err("malformed keymap must fail");
        let rendered = error.to_string();
        assert!(rendered.contains("broken.toml"), "{rendered}");
        assert!(rendered.contains(needle), "{rendered}");
    }

    #[test]
    fn baked_keymap_transcribes_the_2_1_tables() {
        let keymap = load_str(BAKED, BAKED_ORIGIN, KeymapSource::Baked).expect("baked keymap");

        assert_eq!(keymap.name, "claude-2.1");
        assert_eq!(keymap.applies_to.to_string(), ">=2.1.228, <2.2.0");
        assert!(keymap.verified.is_empty());
        assert_eq!(keymap.keys[&KeyName::ShiftTab], b"\x1b[Z");
        assert_eq!(keymap.delays[&DelayName::AfterDeny], 1_500);
        assert_eq!(keymap.delays[&DelayName::AfterOtherSave], 600);
        assert_eq!(
            keymap.menus[&MenuName::Permission].entries[&MenuEntryName::Deny],
            3
        );
        assert_eq!(
            keymap.menus[&MenuName::Plan].entries[&MenuEntryName::RequestChanges],
            3
        );
        assert_eq!(
            keymap.verified_shapes[&ProgramName::PermissionMenu].permission_suggestions,
            [1]
        );
        assert!(keymap.programs.is_empty());
    }

    #[test]
    fn load_names_a_missing_file() {
        let path = PathBuf::from("definitely-missing-keymap.toml");
        let error = load(&path, KeymapSource::Baked).expect_err("missing file");
        assert!(error.to_string().contains("definitely-missing-keymap.toml"));
    }

    #[test]
    fn user_files_cannot_claim_verification() {
        let source = BAKED.replacen(
            "verified = []",
            "verified = [{ version = \"2.1.251\", run_id = \"probe-1\", spec = \"prompt\" }]",
            1,
        );
        let error = load_str(&source, "user.toml", user_source()).expect_err("must refuse");
        assert!(matches!(
            error,
            KeymapError::HandVerified { ref origin } if origin == "user.toml"
        ));
    }

    #[test]
    fn unknown_top_level_field_is_rejected() {
        let source = BAKED.replacen("name =", "surprise = true\nname =", 1);
        assert_parse_mentions(&source, "surprise");
    }

    #[test]
    fn unknown_program_is_rejected() {
        let source = BAKED.replace(
            "programs = {}",
            "[programs.launch]\nstability = \"stable\"\nsteps = []",
        );
        assert_parse_mentions(&source, "launch");
    }

    #[test]
    fn unknown_step_is_rejected() {
        let source = BAKED.replace(
            "programs = {}",
            "[programs.prompt]\nstability = \"stable\"\nsteps = [{ step = \"shell\", command = \"stty\" }]",
        );
        assert_parse_mentions(&source, "shell");
    }

    #[test]
    fn unknown_key_is_rejected() {
        let source = BAKED.replace("enter = [13]", "return = [13]");
        assert_parse_mentions(&source, "return");
    }

    #[test]
    fn unknown_condition_is_rejected() {
        let source = BAKED.replace(
            "programs = {}",
            "[programs.prompt]\nstability = \"stable\"\nsteps = [{ step = \"if\", cond = \"pane_visible\", then = [], otherwise = [] }]",
        );
        assert_parse_mentions(&source, "pane_visible");
    }

    #[test]
    fn malformed_toml_names_its_origin_and_field() {
        let source = BAKED.replacen("name = \"claude-2.1\"", "name = 7", 1);
        assert_parse_mentions(&source, "name");
    }

    #[test]
    fn references_must_name_values_present_in_the_same_file() {
        let source = BAKED.replace(
            "programs = {}",
            "[programs.prompt]\nstability = \"stable\"\nsteps = [{ step = \"key\", key = \"enter\" }]",
        );
        let source = source.replace("enter = [13]\n", "");
        assert_parse_mentions(&source, "programs.Prompt.steps[0].key");
    }
}
