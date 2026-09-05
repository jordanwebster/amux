//! Golden captures: what the app draws, compared with what it drew last time.
//!
//! A baseline under `ios/Goldens` is a regression baseline — the app's own
//! output, locked. The design's preserved captures are a separate report:
//! they are what the app is trying to look like, and a difference from one of
//! them is a conversation, not a failure.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::door;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Light,
    Dark,
}

impl Appearance {
    pub fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

impl fmt::Display for Appearance {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(self.name())
    }
}

/// Where a screen in the manifest came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum GoldenOrigin {
    /// One of the design's own screens. `capture` names its preserved capture
    /// in the intake bundle, without the appearance or the extension.
    Reference { capture: String },
    /// A state the design does not have a capture for, added by this work.
    /// The reason is why it is owed at all.
    AddedState { reason: String },
}

/// One row of the manifest: a golden this flight owes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenScreen {
    pub id: String,
    /// The milestone that builds it.
    pub stage: u8,
    /// The app screen it draws, which is the id itself for a reference screen
    /// and an existing screen for an added state.
    pub screen: String,
    /// The named state the screen is filled from.
    pub fixture: String,
    #[serde(flatten)]
    pub origin: GoldenOrigin,
    pub simulator: String,
    pub appearances: Vec<Appearance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenManifest {
    pub screens: Vec<GoldenScreen>,
}

impl GoldenManifest {
    pub fn read(path: &Path) -> Result<Self, GoldenError> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| GoldenError::Io(format!("{}: {error}", path.display())))?;
        serde_json::from_str(&text)
            .map_err(|error| GoldenError::Io(format!("{}: {error}", path.display())))
    }

    pub fn screen(&self, id: &str) -> Option<&GoldenScreen> {
        self.screens.iter().find(|screen| screen.id == id)
    }
}

/// What a comparison found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoldenVerdict {
    Same,
    Different {
        pixels: u64,
        first: (u32, u32),
    },
    SizeMismatch {
        expected: (u32, u32),
        actual: (u32, u32),
    },
    /// Nothing to compare against: the screen has never been locked.
    MissingBaseline,
    /// The app could not show it. An unimplemented screen answers here rather
    /// than producing a placeholder image nobody would notice.
    CaptureFailed(String),
}

impl GoldenVerdict {
    pub fn passed(&self) -> bool {
        matches!(self, Self::Same)
    }
}

impl fmt::Display for GoldenVerdict {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Same => write!(out, "same"),
            Self::Different { pixels, first } => write!(
                out,
                "{pixels} pixels differ, first at {},{}",
                first.0, first.1
            ),
            Self::SizeMismatch { expected, actual } => write!(
                out,
                "the capture is {}x{} and the baseline is {}x{}",
                actual.0, actual.1, expected.0, expected.1
            ),
            Self::MissingBaseline => write!(out, "no baseline"),
            Self::CaptureFailed(why) => write!(out, "{why}"),
        }
    }
}

#[derive(Debug)]
pub enum GoldenError {
    Io(String),
    Png(String),
    Door(door::DoorError),
    /// The manifest names a screen nobody asked about, or the run was asked
    /// about a screen the manifest does not have.
    NoSuchScreen(String),
}

impl fmt::Display for GoldenError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) | Self::Png(message) => out.write_str(message),
            Self::Door(error) => write!(out, "{error}"),
            Self::NoSuchScreen(id) => write!(out, "the manifest has no screen named {id}"),
        }
    }
}

impl std::error::Error for GoldenError {}

impl From<std::io::Error> for GoldenError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl From<door::DoorError> for GoldenError {
    fn from(error: door::DoorError) -> Self {
        Self::Door(error)
    }
}

struct Image {
    width: u32,
    height: u32,
    /// RGBA, row-major.
    pixels: Vec<u8>,
}

fn read_png(path: &Path) -> Result<Image, GoldenError> {
    let file = std::fs::File::open(path)
        .map_err(|error| GoldenError::Io(format!("{}: {error}", path.display())))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    // The simulator writes wide-gamut captures at sixteen bits a channel, and
    // a baseline written by an earlier run may be eight. Both are read as
    // eight so a comparison is always like for like; nothing a golden is
    // about lives in the low byte.
    decoder.set_transformations(png::Transformations::STRIP_16 | png::Transformations::EXPAND);
    let mut reading = decoder
        .read_info()
        .map_err(|error| GoldenError::Png(format!("{}: {error}", path.display())))?;
    let mut buffer = vec![0; reading.output_buffer_size()];
    let info = reading
        .next_frame(&mut buffer)
        .map_err(|error| GoldenError::Png(format!("{}: {error}", path.display())))?;
    buffer.truncate(info.buffer_size());
    let channels = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        other => {
            return Err(GoldenError::Png(format!(
                "{}: captures are RGB or RGBA, not {other:?}",
                path.display()
            )));
        }
    };
    let mut pixels = Vec::with_capacity((info.width * info.height * 4) as usize);
    for chunk in buffer.chunks_exact(channels) {
        pixels.extend_from_slice(&chunk[..3]);
        pixels.push(if channels == 4 { chunk[3] } else { 255 });
    }
    Ok(Image {
        width: info.width,
        height: info.height,
        pixels,
    })
}

fn write_png(path: &Path, image: &Image) -> Result<(), GoldenError> {
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| GoldenError::Png(error.to_string()))?;
    writer
        .write_image_data(&image.pixels)
        .map_err(|error| GoldenError::Png(error.to_string()))
}

/// Compares two captures and writes the pair and their difference.
///
/// The tolerance is per channel, because a capture of the same screen on the
/// same simulator can differ by a value or two where a gradient is dithered,
/// and failing on that would train everybody to update baselines without
/// looking. Anything a person could see differs by far more.
pub fn diff(
    expected: &Path,
    actual: &Path,
    out: &Path,
    tolerance: u8,
    max_differing_pixels: u64,
) -> Result<GoldenVerdict, GoldenError> {
    if !actual.is_file() {
        return Ok(GoldenVerdict::CaptureFailed(format!(
            "{} was never written",
            actual.display()
        )));
    }
    std::fs::create_dir_all(out)?;
    let taken = read_png(actual)?;
    std::fs::copy(actual, out.join("actual.png"))?;
    if !expected.is_file() {
        return Ok(GoldenVerdict::MissingBaseline);
    }
    let baseline = read_png(expected)?;
    std::fs::copy(expected, out.join("expected.png"))?;
    if baseline.width != taken.width || baseline.height != taken.height {
        return Ok(GoldenVerdict::SizeMismatch {
            expected: (baseline.width, baseline.height),
            actual: (taken.width, taken.height),
        });
    }

    // The difference image marks every pixel that differs at all, in red, over
    // a dimmed copy of the capture, so what changed is legible at a glance.
    let mut marked = Image {
        width: taken.width,
        height: taken.height,
        pixels: taken.pixels.clone(),
    };
    let mut differing = 0u64;
    let mut first = None;
    for index in 0..(taken.width * taken.height) as usize {
        let at = index * 4;
        let apart = (0..4)
            .map(|channel| baseline.pixels[at + channel].abs_diff(taken.pixels[at + channel]))
            .max()
            .unwrap_or(0);
        if apart > tolerance {
            differing += 1;
            if first.is_none() {
                first = Some((index as u32 % taken.width, index as u32 / taken.width));
            }
            marked.pixels[at] = 255;
            marked.pixels[at + 1] = 32;
            marked.pixels[at + 2] = 32;
            marked.pixels[at + 3] = 255;
        } else {
            for channel in 0..3 {
                marked.pixels[at + channel] = marked.pixels[at + channel] / 3 + 40;
            }
        }
    }
    write_png(&out.join("diff.png"), &marked)?;
    if differing > max_differing_pixels {
        return Ok(GoldenVerdict::Different {
            pixels: differing,
            first: first.expect("a differing pixel has a position"),
        });
    }
    Ok(GoldenVerdict::Same)
}

/// Every reference screen paired with the design's preserved capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceReport {
    pub pairs: Vec<(String, Appearance, PathBuf, PathBuf)>,
    pub missing_reference: Vec<String>,
}

/// Pairs what the app draws with what the design drew.
///
/// Only screens that have been built are paired: an unbuilt screen has no
/// baseline to pair, and a pair with an empty half would look like a failure
/// rather than like work not started. This is a report and never a gate.
pub fn reference_report(
    manifest: &GoldenManifest,
    captures: &Path,
    baselines: &Path,
    out: &Path,
) -> Result<ReferenceReport, GoldenError> {
    std::fs::create_dir_all(out)?;
    let mut pairs = Vec::new();
    let mut missing_reference = Vec::new();
    for screen in &manifest.screens {
        let GoldenOrigin::Reference { capture } = &screen.origin else {
            continue;
        };
        for appearance in &screen.appearances {
            let baseline = baselines.join(format!("{}.{appearance}.png", screen.id));
            let reference = captures.join(format!("{capture}.only.{appearance}.png"));
            if !baseline.is_file() {
                continue;
            }
            if !reference.is_file() {
                missing_reference.push(format!("{}.{appearance}", screen.id));
                continue;
            }
            let mine = out.join(format!("{}.{appearance}.app.png", screen.id));
            let theirs = out.join(format!("{}.{appearance}.design.png", screen.id));
            std::fs::copy(&baseline, &mine)?;
            std::fs::copy(&reference, &theirs)?;
            pairs.push((screen.id.clone(), *appearance, mine, theirs));
        }
    }
    Ok(ReferenceReport {
        pairs,
        missing_reference,
    })
}

/// One screen in one appearance, captured and judged.
pub struct GoldenOutcome {
    pub id: String,
    pub appearance: Appearance,
    pub verdict: GoldenVerdict,
}

/// Captures the named screens through the driving door and compares each one
/// with its baseline.
///
/// Everything is captured in one conversation with one launch: a launch per
/// screen would triple the run and prove nothing extra.
#[allow(clippy::too_many_arguments)]
pub fn run(
    manifest: &GoldenManifest,
    ids: &[String],
    simulator: &str,
    bundle_id: &str,
    baselines: &Path,
    out: &Path,
    update: bool,
    tolerance: u8,
    max_differing_pixels: u64,
    // A colour token to move before anything is drawn. Only the perturbation
    // check passes one, and it requires every comparison to fail.
    perturb: Option<&str>,
) -> Result<Vec<GoldenOutcome>, GoldenError> {
    let wanted: Vec<&GoldenScreen> = if ids.is_empty() {
        manifest.screens.iter().collect()
    } else {
        ids.iter()
            .map(|id| {
                manifest
                    .screen(id)
                    .ok_or_else(|| GoldenError::NoSuchScreen(id.clone()))
            })
            .collect::<Result<_, _>>()?
    };

    // One conversation per simulator: the manifest may name more than one, and
    // a capture on the wrong device would be the wrong width.
    let mut by_simulator: BTreeMap<&str, Vec<&GoldenScreen>> = BTreeMap::new();
    for screen in &wanted {
        by_simulator
            .entry(screen.simulator.as_str())
            .or_default()
            .push(screen);
    }

    let mut outcomes = Vec::new();
    for (device, screens) in by_simulator {
        let mut requests: Vec<Value> = Vec::new();
        let mut planned = Vec::new();
        // Which screen each request belongs to, so a refusal is attributed to
        // the screen it was about rather than to whatever came next.
        let mut about: Vec<String> = Vec::new();
        if let Some(token) = perturb {
            requests.push(json!({"kind": "perturb", "token": token}));
            about.push(String::new());
        }
        for screen in &screens {
            requests.push(json!({
                "kind": "open", "screen": screen.screen, "fixture": screen.fixture
            }));
            about.push(screen.id.clone());
            for appearance in &screen.appearances {
                let taken = out
                    .join("actual")
                    .join(format!("{}.{appearance}.png", screen.id));
                requests.push(json!({"kind": "appearance", "appearance": appearance.name()}));
                requests.push(json!({"kind": "settle"}));
                requests.push(json!({"kind": "capture", "path": taken.to_string_lossy()}));
                about.extend([screen.id.clone(), screen.id.clone(), screen.id.clone()]);
                planned.push((screen.id.clone(), *appearance, taken));
            }
        }
        requests.push(json!({"kind": "shutdown"}));
        about.push(String::new());

        // The manifest names the device each screen belongs on; the one this
        // run was pointed at is only the default.
        let _ = simulator;
        let replies = door::door(device, bundle_id, requests, Duration::from_secs(300))?;

        // Which screens the door refused, and why. A refusal belongs to the
        // screen it was about, so an unimplemented screen is named rather than
        // reported as a missing file.
        let mut refusal: BTreeMap<String, String> = BTreeMap::new();
        for (index, reply) in replies.iter().enumerate() {
            if reply.get("kind").and_then(Value::as_str) != Some("error") {
                continue;
            }
            let message = reply
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the door refused")
                .to_string();
            if let Some(id) = about.get(index) {
                refusal.entry(id.clone()).or_insert(message);
            }
        }

        for (id, appearance, taken) in planned {
            if let Some(message) = refusal.get(&id) {
                outcomes.push(GoldenOutcome {
                    id,
                    appearance,
                    verdict: GoldenVerdict::CaptureFailed(message.clone()),
                });
                continue;
            }
            let baseline = baselines.join(format!("{id}.{appearance}.png"));
            if update {
                if let Some(directory) = baseline.parent() {
                    std::fs::create_dir_all(directory)?;
                }
                std::fs::copy(&taken, &baseline)?;
            }
            let verdict = diff(
                &baseline,
                &taken,
                &out.join(format!("{id}.{appearance}")),
                tolerance,
                max_differing_pixels,
            )?;
            outcomes.push(GoldenOutcome {
                id,
                appearance,
                verdict,
            });
        }
    }
    Ok(outcomes)
}

// MARK: - The command

const MANIFEST: &str = "ios/Goldens/manifest.json";
const BASELINES: &str = "ios/Goldens";
const OUT: &str = "target/ios/goldens";
const PERTURBED_OUT: &str = "target/ios/goldens/perturbed";
/// The screen the perturbation check is run on, and the token it moves. The
/// probe draws every colour token the design has, so any of them would show.
const PERTURBED_SCREEN: &str = "probe";
const PERTURBED_TOKEN: &str = "accent";
/// A capture of the same screen on the same simulator can differ by a value or
/// two where a gradient is dithered; anything a person could see differs by
/// much more than this, in far more than a handful of pixels.
const TOLERANCE: u8 = 2;
const MAX_DIFFERING_PIXELS: u64 = 64;

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(2).collect();
    match arguments.first().map(String::as_str) {
        Some("run") => run_command(&arguments[1..]),
        Some("perturb") => perturb_command(&arguments[1..]),
        Some("diff") => diff_command(&arguments[1..]),
        Some("reference") => reference_command(&arguments[1..]),
        _ => {
            eprintln!(
                "usage: xtask golden <run [--simulator NAME] [--bundle-id ID] [--install APP] \
                 [--update] [IDS...]|perturb [--simulator NAME] [--bundle-id ID] \
                 [--token NAME] [IDS...]|diff --expected PNG --actual PNG --out DIR|\
                 reference --captures DIR [--out DIR]>"
            );
            std::process::exit(2);
        }
    }
}

fn value(arguments: &[String], name: &str) -> Option<String> {
    arguments
        .iter()
        .position(|argument| argument == name)
        .and_then(|at| arguments.get(at + 1))
        .cloned()
}

fn run_command(arguments: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let simulator = value(arguments, "--simulator").unwrap_or_else(|| "amux-golden".into());
    let bundle_id = value(arguments, "--bundle-id").unwrap_or_else(|| "sh.amux.Amux".into());
    let update = arguments.iter().any(|argument| argument == "--update");
    if let Some(application) = value(arguments, "--install") {
        let udid = door::simulator_udid(&simulator)?;
        door::install(&udid, Path::new(&application))?;
    }
    // Everything that is not a flag or a flag's value names a screen.
    let mut ids: Vec<String> = Vec::new();
    let mut skip_next = false;
    for argument in arguments {
        if skip_next {
            skip_next = false;
            continue;
        }
        if argument.starts_with("--") {
            skip_next = ["--simulator", "--bundle-id", "--install"].contains(&argument.as_str());
            continue;
        }
        ids.push(argument.clone());
    }

    let manifest = GoldenManifest::read(Path::new(MANIFEST))?;
    let out = Path::new(OUT);
    let outcomes = run(
        &manifest,
        &ids,
        &simulator,
        &bundle_id,
        Path::new(BASELINES),
        out,
        update,
        TOLERANCE,
        MAX_DIFFERING_PIXELS,
        None,
    )?;

    let mut failed = Vec::new();
    for outcome in &outcomes {
        let mark = if outcome.verdict.passed() {
            "ok"
        } else {
            "FAILED"
        };
        println!(
            "{mark} {}.{}: {}",
            outcome.id, outcome.appearance, outcome.verdict
        );
        if !outcome.verdict.passed() {
            failed.push(format!("{}.{}", outcome.id, outcome.appearance));
        }
    }
    println!(
        "{} captures, {} failed; triplets under {}",
        outcomes.len(),
        failed.len(),
        out.display()
    );
    if !failed.is_empty() {
        return Err(format!("goldens failed: {}", failed.join(", ")).into());
    }
    Ok(())
}

/// Moves one design token and requires every comparison to notice.
///
/// A golden suite that has never been seen to fail proves nothing: the
/// captures could be of the wrong window, the comparison could be reading the
/// baseline twice, the tolerance could be swallowing everything. So one
/// colour token is replaced with a magenta the design never uses, the same
/// screens are captured the same way, and this command fails unless every one
/// of them came back different with a difference image beside it.
fn perturb_command(arguments: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let simulator = value(arguments, "--simulator").unwrap_or_else(|| "amux-golden".into());
    let bundle_id = value(arguments, "--bundle-id").unwrap_or_else(|| "sh.amux.Amux".into());
    let token = value(arguments, "--token").unwrap_or_else(|| PERTURBED_TOKEN.into());
    let mut ids: Vec<String> = Vec::new();
    let mut skip_next = false;
    for argument in arguments {
        if skip_next {
            skip_next = false;
            continue;
        }
        if argument.starts_with("--") {
            skip_next = ["--simulator", "--bundle-id", "--token"].contains(&argument.as_str());
            continue;
        }
        ids.push(argument.clone());
    }
    if ids.is_empty() {
        ids.push(PERTURBED_SCREEN.into());
    }

    let manifest = GoldenManifest::read(Path::new(MANIFEST))?;
    // Its own directory: this run's captures are wrong on purpose, and
    // leaving them where an ordinary run writes its triplets would put a
    // magenta screen in front of whoever looks at the last real failure.
    let out = Path::new(PERTURBED_OUT);
    let outcomes = run(
        &manifest,
        &ids,
        &simulator,
        &bundle_id,
        Path::new(BASELINES),
        out,
        false,
        TOLERANCE,
        MAX_DIFFERING_PIXELS,
        Some(&token),
    )?;

    let mut unnoticed = Vec::new();
    for outcome in &outcomes {
        let image = out
            .join(format!("{}.{}", outcome.id, outcome.appearance))
            .join("diff.png");
        let noticed = matches!(outcome.verdict, GoldenVerdict::Different { .. }) && image.is_file();
        println!(
            "{} {}.{}: {}",
            if noticed { "caught" } else { "MISSED" },
            outcome.id,
            outcome.appearance,
            outcome.verdict
        );
        if noticed {
            println!("  {}", image.display());
        } else {
            unnoticed.push(format!("{}.{}", outcome.id, outcome.appearance));
        }
    }
    if outcomes.is_empty() {
        return Err("the perturbation check captured nothing".into());
    }
    if !unnoticed.is_empty() {
        return Err(format!(
            "the `{token}` token was moved and the golden run did not fail on {}",
            unnoticed.join(", ")
        )
        .into());
    }
    println!(
        "{} captures, all different with the `{token}` token moved; \
         difference images under {}",
        outcomes.len(),
        out.display()
    );
    Ok(())
}

fn diff_command(arguments: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let expected = value(arguments, "--expected").ok_or("--expected names the baseline PNG")?;
    let actual = value(arguments, "--actual").ok_or("--actual names the capture")?;
    let out = value(arguments, "--out").unwrap_or_else(|| format!("{OUT}/diff"));
    let tolerance = value(arguments, "--tolerance")
        .map(|text| text.parse())
        .transpose()?
        .unwrap_or(TOLERANCE);
    let allowed = value(arguments, "--max-differing")
        .map(|text| text.parse())
        .transpose()?
        .unwrap_or(MAX_DIFFERING_PIXELS);
    let verdict = diff(
        Path::new(&expected),
        Path::new(&actual),
        Path::new(&out),
        tolerance,
        allowed,
    )?;
    println!("{verdict}");
    if verdict.passed() {
        Ok(())
    } else {
        Err(format!("{expected} and {actual}: {verdict}").into())
    }
}

fn reference_command(arguments: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let captures = value(arguments, "--captures")
        .unwrap_or_else(|| "notes/ios-intake/design-reference/design/captures".into());
    let out = value(arguments, "--out").unwrap_or_else(|| format!("{OUT}/reference"));
    let manifest = GoldenManifest::read(Path::new(MANIFEST))?;
    if !Path::new(&captures).is_dir() {
        println!(
            "{captures} is not here, so there is nothing to pair with; \
             the intake bundle is working material, not a build input"
        );
        return Ok(());
    }
    let report = reference_report(
        &manifest,
        Path::new(&captures),
        Path::new(BASELINES),
        Path::new(&out),
    )?;
    for (id, appearance, mine, theirs) in &report.pairs {
        println!(
            "{id}.{appearance}: {} beside {}",
            mine.display(),
            theirs.display()
        );
    }
    if !report.missing_reference.is_empty() {
        println!(
            "no preserved capture for: {}",
            report.missing_reference.join(", ")
        );
    }
    println!("{} pairs under {out}", report.pairs.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, width: u32, height: u32, colour: [u8; 4]) {
        let image = Image {
            width,
            height,
            pixels: colour
                .iter()
                .cycle()
                .take((width * height * 4) as usize)
                .copied()
                .collect(),
        };
        write_png(path, &image).expect("the test image is written");
    }

    #[test]
    fn the_same_capture_twice_is_the_same() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        let actual = room.path().join("actual.png");
        write(&expected, 4, 4, [10, 20, 30, 255]);
        write(&actual, 4, 4, [10, 20, 30, 255]);
        let out = room.path().join("out");
        assert_eq!(
            diff(&expected, &actual, &out, 2, 0).expect("a verdict"),
            GoldenVerdict::Same
        );
        assert!(out.join("expected.png").is_file());
        assert!(out.join("actual.png").is_file());
        assert!(out.join("diff.png").is_file());
    }

    #[test]
    fn a_change_too_small_to_see_is_inside_the_tolerance() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        let actual = room.path().join("actual.png");
        write(&expected, 4, 4, [10, 20, 30, 255]);
        write(&actual, 4, 4, [11, 21, 31, 255]);
        assert_eq!(
            diff(&expected, &actual, &room.path().join("out"), 2, 0).expect("a verdict"),
            GoldenVerdict::Same
        );
    }

    #[test]
    fn a_visible_change_is_reported_with_where_it_starts() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        let actual = room.path().join("actual.png");
        write(&expected, 4, 4, [10, 20, 30, 255]);
        write(&actual, 4, 4, [200, 20, 30, 255]);
        match diff(&expected, &actual, &room.path().join("out"), 2, 0).expect("a verdict") {
            GoldenVerdict::Different { pixels, first } => {
                assert_eq!(pixels, 16);
                assert_eq!(first, (0, 0));
            }
            other => panic!("expected a difference, got {other}"),
        }
    }

    #[test]
    fn a_capture_of_another_size_is_not_compared_pixel_by_pixel() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        let actual = room.path().join("actual.png");
        write(&expected, 4, 4, [10, 20, 30, 255]);
        write(&actual, 8, 4, [10, 20, 30, 255]);
        assert_eq!(
            diff(&expected, &actual, &room.path().join("out"), 2, 0).expect("a verdict"),
            GoldenVerdict::SizeMismatch {
                expected: (4, 4),
                actual: (8, 4)
            }
        );
    }

    #[test]
    fn a_screen_that_has_never_been_locked_says_so() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let actual = room.path().join("actual.png");
        write(&actual, 4, 4, [10, 20, 30, 255]);
        assert_eq!(
            diff(
                &room.path().join("nothing.png"),
                &actual,
                &room.path().join("out"),
                2,
                0
            )
            .expect("a verdict"),
            GoldenVerdict::MissingBaseline
        );
    }

    #[test]
    fn a_capture_that_never_happened_says_so() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        write(&expected, 4, 4, [10, 20, 30, 255]);
        let verdict = diff(
            &expected,
            &room.path().join("nothing.png"),
            &room.path().join("out"),
            2,
            0,
        )
        .expect("a verdict");
        assert!(
            matches!(verdict, GoldenVerdict::CaptureFailed(_)),
            "expected a failed capture, got {verdict}"
        );
    }

    #[test]
    fn the_committed_manifest_owes_the_catalogue_and_the_added_states() {
        let manifest =
            GoldenManifest::read(Path::new("../../ios/Goldens/manifest.json")).expect("a manifest");
        let references: Vec<&GoldenScreen> = manifest
            .screens
            .iter()
            .filter(|screen| matches!(screen.origin, GoldenOrigin::Reference { .. }))
            .collect();
        assert_eq!(
            references.len(),
            33,
            "the design's catalogue has 33 in-scope screens"
        );
        assert!(
            manifest.screen("notification").is_none(),
            "notifications are out of scope"
        );
        assert!(manifest.screen("probe").is_some(), "the probe is owed");
        for screen in &manifest.screens {
            assert!(
                !screen.appearances.is_empty(),
                "{} owes at least one appearance",
                screen.id
            );
            assert!(
                screen.simulator == "amux-golden" || screen.simulator == "amux-small",
                "{} names an unpinned simulator",
                screen.id
            );
            if let GoldenOrigin::AddedState { reason } = &screen.origin {
                assert!(!reason.is_empty(), "{} says why it is owed", screen.id);
            }
        }
    }
}
