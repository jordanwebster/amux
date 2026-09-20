//! Golden captures: what the app draws, compared with what it drew last time.
//!
//! A baseline under `apps/apple/Goldens` is a regression baseline — the app's own
//! output, locked. The design's preserved captures are a separate report:
//! they are what the app is trying to look like, and a difference from one of
//! them is a conversation, not a failure.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

/// A simulator rendering variation that is still captured, compared and
/// reported, but whose pixel-difference verdict does not gate CI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenFlake {
    pub appearances: Vec<Appearance>,
    pub reason: String,
}

/// A region of a simulator's display that the system draws over every app,
/// which no capture compares.
///
/// A golden is a photograph of the display, so it holds whatever SpringBoard
/// puts over the app as well as the app. The status bar is pinned, but the
/// home indicator cannot be: SpringBoard draws it when an app launches and
/// withdraws it once backboardd's attention timer says nobody is touching the
/// screen, and on a GitHub runner with both pinned devices booted that timer's
/// event is delivered to a stale client, so the bar never withdraws there and
/// always does on a developer's Mac. The bar is not the app's drawing, so the
/// pixels under it are not the app's golden; they are painted over in the
/// difference image so nobody mistakes them for compared ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemChrome {
    /// What the system draws there, for whoever reads the manifest.
    pub what: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl SystemChrome {
    fn covers(&self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// What the manifest knows about one pinned simulator.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenSimulator {
    #[serde(default)]
    pub system_chrome: Vec<SystemChrome>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flaky: Option<GoldenFlake>,
    pub simulator: String,
    pub appearances: Vec<Appearance>,
    /// Native component examples that now own this state's visual variations.
    /// The historical full-screen capture remains available through `--all`
    /// or its explicit ID, but is not repeated in the routine display suite.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub component_snapshots: Vec<String>,
}

impl GoldenScreen {
    fn flaky_reason(&self, appearance: Appearance) -> Option<&str> {
        self.flaky
            .as_ref()
            .filter(|flake| flake.appearances.contains(&appearance))
            .map(|flake| flake.reason.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenManifest {
    /// Every simulator a screen may name, with the chrome it draws over the
    /// app. Declaring a device is what forces the question of its chrome to
    /// be answered before a screen is captured on it.
    pub simulators: BTreeMap<String, GoldenSimulator>,
    pub screens: Vec<GoldenScreen>,
}

impl GoldenManifest {
    pub fn read(path: &Path) -> Result<Self, GoldenError> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| GoldenError::Io(format!("{}: {error}", path.display())))?;
        let manifest: Self = serde_json::from_str(&text)
            .map_err(|error| GoldenError::Io(format!("{}: {error}", path.display())))?;
        for screen in &manifest.screens {
            if !manifest.simulators.contains_key(&screen.simulator) {
                return Err(GoldenError::Io(format!(
                    "{}: screen {} names simulator {}, which the manifest does not declare",
                    path.display(),
                    screen.id,
                    screen.simulator
                )));
            }
            let Some(flake) = &screen.flaky else {
                continue;
            };
            if flake.reason.trim().is_empty() {
                return Err(GoldenError::Io(format!(
                    "{}: flaky capture {} needs a reason",
                    path.display(),
                    screen.id
                )));
            }
            if flake.appearances.is_empty()
                || flake
                    .appearances
                    .iter()
                    .any(|appearance| !screen.appearances.contains(appearance))
            {
                return Err(GoldenError::Io(format!(
                    "{}: flaky capture {} must name one of its appearances",
                    path.display(),
                    screen.id
                )));
            }
        }
        Ok(manifest)
    }

    pub fn screen(&self, id: &str) -> Option<&GoldenScreen> {
        self.screens.iter().find(|screen| screen.id == id)
    }

    /// The system chrome a screen's simulator draws over it.
    pub fn system_chrome(&self, screen: &GoldenScreen) -> &[SystemChrome] {
        self.simulators
            .get(&screen.simulator)
            .map(|simulator| simulator.system_chrome.as_slice())
            .unwrap_or(&[])
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
    let pixels = if channels == 4 {
        buffer
    } else {
        let mut pixels = Vec::with_capacity((info.width * info.height * 4) as usize);
        for chunk in buffer.chunks_exact(3) {
            pixels.extend_from_slice(chunk);
            pixels.push(255);
        }
        pixels
    };
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

/// Compares two captures, retains the pair, and writes a difference only when
/// the comparison fails.
///
/// The tolerance is per channel, because a capture of the same screen on the
/// same simulator can differ by a value or two where a gradient is dithered,
/// and failing on that would train everybody to update baselines without
/// looking. Anything a person could see differs by far more.
///
/// Pixels under the simulator's system chrome are never counted, whatever
/// they hold: that part of the picture is the system's, not the app's.
pub fn diff(
    expected: &Path,
    actual: &Path,
    out: &Path,
    tolerance: u8,
    max_differing_pixels: u64,
    system_chrome: &[SystemChrome],
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

    // Most captures agree exactly. Compare the normalized bytes first; neither
    // tolerance nor excluded chrome can turn identical pixels into a failure.
    if baseline.pixels == taken.pixels {
        return Ok(GoldenVerdict::Same);
    }
    let mut differences = Vec::new();
    for (index, (expected, actual)) in baseline
        .pixels
        .chunks_exact(4)
        .zip(taken.pixels.chunks_exact(4))
        .enumerate()
    {
        if expected == actual {
            continue;
        }
        let (x, y) = (index as u32 % taken.width, index as u32 / taken.width);
        if system_chrome.iter().any(|chrome| chrome.covers(x, y)) {
            continue;
        }
        if expected[0].abs_diff(actual[0]) > tolerance
            || expected[1].abs_diff(actual[1]) > tolerance
            || expected[2].abs_diff(actual[2]) > tolerance
            || expected[3].abs_diff(actual[3]) > tolerance
        {
            differences.push(index);
        }
    }
    if differences.len() as u64 > max_differing_pixels {
        // Only failures need a picture. Red marks changed pixels; blue marks
        // system chrome that was excluded, whether or not it changed.
        let mut marked = Image {
            width: taken.width,
            height: taken.height,
            pixels: taken.pixels,
        };
        for (index, pixel) in marked.pixels.chunks_exact_mut(4).enumerate() {
            let (x, y) = (index as u32 % taken.width, index as u32 / taken.width);
            if system_chrome.iter().any(|chrome| chrome.covers(x, y)) {
                pixel[0] /= 4;
                pixel[1] = pixel[1] / 4 + 40;
                pixel[2] = pixel[2] / 4 + 150;
                pixel[3] = 255;
            } else {
                for channel in &mut pixel[..3] {
                    *channel = *channel / 3 + 40;
                }
            }
        }
        for index in &differences {
            marked.pixels[index * 4..index * 4 + 4].copy_from_slice(&[255, 32, 32, 255]);
        }
        write_png(&out.join("diff.png"), &marked)?;
        let first = differences[0] as u32;
        return Ok(GoldenVerdict::Different {
            pixels: differences.len() as u64,
            first: (first % taken.width, first / taken.width),
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
    pub flaky: Option<String>,
    pub verdict: GoldenVerdict,
    /// `--update` wrote this capture over its baseline, because the two did
    /// not agree. The verdict is what they disagreed about, kept rather than
    /// swallowed so the run can say which screens it changed.
    pub rewritten: bool,
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
    let mut timings = Vec::new();
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
                // Photographed off the simulator's display rather than drawn
                // by the app into an image: glass is resolved by the render
                // server, and only the render server's own output is stable
                // from one run to the next. The app's own frame capture is
                // still there for a report, which has to freeze what the
                // person was looking at from inside the process.
                requests.push(json!({"kind": "display", "path": taken.to_string_lossy()}));
                about.extend([screen.id.clone(), screen.id.clone(), screen.id.clone()]);
                planned.push((
                    screen.id.clone(),
                    *appearance,
                    screen.flaky_reason(*appearance).map(str::to_string),
                    taken,
                ));
            }
        }
        requests.push(json!({"kind": "shutdown"}));
        about.push(String::new());

        // The manifest names the device each screen belongs on; the one this
        // run was pointed at is only the default.
        let _ = simulator;
        let capture_started = Instant::now();
        let replies = door::door(device, bundle_id, requests, Duration::from_secs(300))?;
        let capture_seconds = capture_started.elapsed().as_secs_f64();
        let capture_count = planned.len();
        eprintln!("goldens: {device}: {capture_count} captures in {capture_seconds:.3}s");
        let comparison_started = Instant::now();

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

        for (id, appearance, flaky, taken) in planned {
            if let Some(message) = refusal.get(&id) {
                outcomes.push(GoldenOutcome {
                    id,
                    appearance,
                    flaky,
                    verdict: GoldenVerdict::CaptureFailed(message.clone()),
                    rewritten: false,
                });
                continue;
            }
            let baseline = baselines.join(format!("{id}.{appearance}.png"));
            let chrome = manifest
                .screen(&id)
                .map(|screen| manifest.system_chrome(screen))
                .unwrap_or(&[]);
            let verdict = diff(
                &baseline,
                &taken,
                &out.join(format!("{id}.{appearance}")),
                tolerance,
                max_differing_pixels,
                chrome,
            )?;
            // Only a baseline that disagrees is replaced. Rewriting the ones
            // that already agree costs a re-encoded PNG for every screen in
            // the run, and buries the handful that actually moved among them.
            let rewritten = update && !verdict.passed();
            if rewritten {
                if let Some(directory) = baseline.parent() {
                    std::fs::create_dir_all(directory)?;
                }
                std::fs::copy(&taken, &baseline)?;
            }
            outcomes.push(GoldenOutcome {
                id,
                appearance,
                flaky,
                verdict,
                rewritten,
            });
        }
        let comparison_seconds = comparison_started.elapsed().as_secs_f64();
        eprintln!("goldens: {device}: comparisons in {comparison_seconds:.3}s");
        timings.push(json!({
            "simulator": device, "captures": capture_count,
            "capture_seconds": capture_seconds, "comparison_seconds": comparison_seconds,
        }));
    }
    std::fs::create_dir_all(out)?;
    std::fs::write(
        out.join("timings.json"),
        serde_json::to_vec_pretty(&timings).unwrap(),
    )?;
    Ok(outcomes)
}

/// The word the door answers with for a screen nobody has built. A refusal
/// that starts with it names work still to come rather than a break.
const UNIMPLEMENTED: &str = "unimplemented: ";

/// What a run amounts to: what broke, and what has not been built yet.
pub struct GoldenReport {
    pub failed: Vec<String>,
    pub flaky: Vec<String>,
    pub unimplemented: Vec<String>,
    /// Baselines `--update` replaced, and what each one had disagreed about.
    pub rewritten: Vec<String>,
    pub total: usize,
}

/// Sorts a run's outcomes into what failed and what is not built yet.
///
/// Two questions are being asked of the same captures at different points in
/// the flight. The whole manifest asks whether every screen the flight owes is
/// drawn and locked, and a screen nobody has built is a failure — that is the
/// contract that keeps the catalogue honest. `built_only` asks the narrower
/// question the branch's own verification asks between milestones: of the
/// screens that exist today, does every one of them still draw what it was
/// locked as. There, an unimplemented screen is reported and counted, and
/// only an explicitly declared simulator pixel-difference flake is non-gating.
/// A missing baseline, failed capture or size change still fails, including on
/// a capture carrying flaky metadata.
pub fn judge(outcomes: &[GoldenOutcome], built_only: bool) -> GoldenReport {
    let mut failed = Vec::new();
    let mut flaky = Vec::new();
    let mut unimplemented = Vec::new();
    let mut rewritten = Vec::new();
    for outcome in outcomes {
        let name = format!("{}.{}", outcome.id, outcome.appearance);
        let unbuilt = matches!(
            &outcome.verdict, GoldenVerdict::CaptureFailed(why) if why.starts_with(UNIMPLEMENTED));
        if built_only && unbuilt {
            unimplemented.push(name);
        } else if outcome.flaky.is_some()
            && matches!(outcome.verdict, GoldenVerdict::Different { .. })
        {
            flaky.push(name);
        } else if outcome.rewritten {
            // Asked for, and done. A baseline the operator has just replaced
            // is not a failure of the run that replaced it.
            rewritten.push(name);
        } else if !outcome.verdict.passed() {
            failed.push(name);
        }
    }
    GoldenReport {
        failed,
        flaky,
        unimplemented,
        rewritten,
        total: outcomes.len(),
    }
}

// MARK: - The command

const MANIFEST: &str = "apps/apple/Goldens/manifest.json";
const BASELINES: &str = "apps/apple/Goldens";
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
                 [--update] [--built] [--all] [IDS...]|perturb [--simulator NAME] [--bundle-id ID] \
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
    let simulator = value(arguments, "--simulator").unwrap_or_else(|| "golden".into());
    let bundle_id = value(arguments, "--bundle-id").unwrap_or_else(|| "sh.amux.app".into());
    let update = arguments.iter().any(|argument| argument == "--update");
    let built_only = arguments.iter().any(|argument| argument == "--built");
    // Locking a baseline is a deliberate act about a screen somebody just
    // looked at. Doing it under a run that forgives unbuilt screens would
    // quietly write baselines for whatever happened to open.
    if update && built_only {
        return Err(
            "--update rewrites baselines and --built forgives unbuilt screens; \
                    name the screens to update instead"
                .into(),
        );
    }
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
    if ids.is_empty() && !arguments.iter().any(|argument| argument == "--all") {
        ids = manifest
            .screens
            .iter()
            .filter(|screen| screen.component_snapshots.is_empty())
            .map(|screen| screen.id.clone())
            .collect();
        if ids.is_empty() {
            return Err("the manifest must retain full-screen composition coverage".into());
        }
    }
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

    let report = judge(&outcomes, built_only);
    let unimplemented: std::collections::BTreeSet<&String> = report.unimplemented.iter().collect();
    let flaky: std::collections::BTreeSet<&String> = report.flaky.iter().collect();
    for outcome in &outcomes {
        let name = format!("{}.{}", outcome.id, outcome.appearance);
        let mark = if outcome.rewritten {
            "rewrote"
        } else if outcome.verdict.passed() {
            "ok"
        } else if unimplemented.contains(&name) {
            "not built"
        } else if flaky.contains(&name) {
            "FLAKY"
        } else {
            "FAILED"
        };
        println!("{mark} {name}: {}", outcome.verdict);
    }
    println!(
        "{} captures, {} failed, {} flaky; triplets under {}",
        report.total,
        report.failed.len(),
        report.flaky.len(),
        out.display()
    );
    if !report.rewritten.is_empty() {
        let count = report.rewritten.len();
        println!(
            "{count} baseline{} replaced; every other capture already agreed and was left alone",
            if count == 1 { "" } else { "s" }
        );
    }
    let declared_flaky: Vec<_> = outcomes
        .iter()
        .filter_map(|outcome| {
            outcome
                .flaky
                .as_ref()
                .map(|reason| (format!("{}.{}", outcome.id, outcome.appearance), reason))
        })
        .collect();
    if !declared_flaky.is_empty() {
        println!("{} captures are marked flaky:", declared_flaky.len());
        for (name, reason) in declared_flaky {
            println!("  {name}: {reason}");
        }
    }
    if built_only {
        println!(
            "{} of {} captures unimplemented",
            report.unimplemented.len(),
            report.total
        );
    }
    if !report.failed.is_empty() {
        return Err(format!("goldens failed: {}", report.failed.join(", ")).into());
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
    let simulator = value(arguments, "--simulator").unwrap_or_else(|| "golden".into());
    let bundle_id = value(arguments, "--bundle-id").unwrap_or_else(|| "sh.amux.app".into());
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
    // The chrome of a named pinned simulator, so a pair of files compares the
    // way the run compares them; without one, every pixel counts.
    let chrome = match value(arguments, "--simulator") {
        Some(name) => GoldenManifest::read(Path::new(MANIFEST))?
            .simulators
            .get(&name)
            .ok_or_else(|| format!("the manifest does not declare simulator {name}"))?
            .system_chrome
            .clone(),
        None => Vec::new(),
    };
    let verdict = diff(
        Path::new(&expected),
        Path::new(&actual),
        Path::new(&out),
        tolerance,
        allowed,
        &chrome,
    )?;
    println!("{verdict}");
    if verdict.passed() {
        Ok(())
    } else {
        Err(format!("{expected} and {actual}: {verdict}").into())
    }
}

fn reference_command(arguments: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let captures =
        value(arguments, "--captures").unwrap_or_else(|| "apps/apple/Goldens/References".into());
    let out = value(arguments, "--out").unwrap_or_else(|| format!("{OUT}/reference"));
    let manifest = GoldenManifest::read(Path::new(MANIFEST))?;
    if !Path::new(&captures).is_dir() {
        println!(
            "{captures} is not here, so there is nothing to pair with; \
             provide the preserved design captures with --captures"
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

    /// A local throughput sample, deliberately separate from correctness tests.
    /// Use the committed images so decoding costs match the real catalogue.
    #[test]
    #[ignore = "manual full-resolution image comparison timing"]
    fn compare_full_resolution_samples() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let manifest = GoldenManifest::read(Path::new("../../apps/apple/Goldens/manifest.json"))
            .expect("the committed manifest");
        for id in ["home", "typing", "ax-composer"] {
            let path = PathBuf::from(format!("../../apps/apple/Goldens/{id}.light.png"));
            let baseline = read_png(&path).expect("the committed image");
            let tolerated = room.path().join(format!("{id}.png"));
            let mut pixels = baseline.pixels.clone();
            for pixel in pixels.chunks_exact_mut(4) {
                pixel[0] = pixel[0].saturating_add(1);
            }
            write_png(&tolerated, &Image { pixels, ..baseline }).expect("a tolerated image");
            for (kind, actual) in [("identical", &path), ("tolerated", &tolerated)] {
                for round in 0..3 {
                    let started = std::time::Instant::now();
                    let verdict = diff(
                        &path,
                        actual,
                        &room.path().join("out"),
                        2,
                        64,
                        manifest.system_chrome(manifest.screen(id).unwrap()),
                    )
                    .expect("a comparison");
                    assert_eq!(verdict, GoldenVerdict::Same);
                    println!(
                        "{id} {kind} round={round} seconds={:.6}",
                        started.elapsed().as_secs_f64()
                    );
                }
            }
        }
    }

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

    fn outcome(id: &str, verdict: GoldenVerdict) -> GoldenOutcome {
        GoldenOutcome {
            id: id.to_string(),
            appearance: Appearance::Light,
            flaky: None,
            verdict,
            rewritten: false,
        }
    }

    fn rewritten_outcome(id: &str, verdict: GoldenVerdict) -> GoldenOutcome {
        GoldenOutcome {
            rewritten: true,
            ..outcome(id, verdict)
        }
    }

    #[test]
    fn a_replaced_baseline_is_reported_rather_than_failed() {
        let report = judge(
            &[
                outcome("probe", GoldenVerdict::Same),
                rewritten_outcome(
                    "home",
                    GoldenVerdict::Different {
                        pixels: 19_553,
                        first: (119, 1380),
                    },
                ),
                outcome(
                    "strip",
                    GoldenVerdict::Different {
                        pixels: 8_542,
                        first: (107, 893),
                    },
                ),
            ],
            false,
        );
        assert_eq!(report.rewritten, ["home.light"]);
        assert_eq!(report.failed, ["strip.light"]);
        assert_eq!(report.total, 3);
    }

    fn flaky_outcome(id: &str, verdict: GoldenVerdict) -> GoldenOutcome {
        GoldenOutcome {
            id: id.to_string(),
            appearance: Appearance::Light,
            flaky: Some("simulator text can settle two pixels apart".into()),
            verdict,
            rewritten: false,
        }
    }

    /// Mid-flight, a screen nobody has built yet is work still to come and is
    /// counted; a screen that opened and has nothing to compare with is a
    /// baseline somebody forgot to lock, and that still fails.
    #[test]
    fn a_run_over_built_screens_counts_the_unbuilt_and_still_fails_the_rest() {
        let outcomes = [
            outcome("probe", GoldenVerdict::Same),
            outcome(
                "home",
                GoldenVerdict::CaptureFailed("unimplemented: home".into()),
            ),
            outcome("strip", GoldenVerdict::MissingBaseline),
            outcome(
                "hosts",
                GoldenVerdict::Different {
                    pixels: 900,
                    first: (1, 2),
                },
            ),
            outcome(
                "you",
                GoldenVerdict::CaptureFailed("no window on screen".into()),
            ),
        ];
        let report = judge(&outcomes, true);
        assert_eq!(report.unimplemented, ["home.light"]);
        assert_eq!(report.failed, ["strip.light", "hosts.light", "you.light"]);
        assert_eq!(report.total, 5);
    }

    /// Over the whole catalogue the question is different: a screen the flight
    /// owes and nobody has built is exactly what the run is there to name.
    #[test]
    fn a_run_over_the_whole_manifest_fails_on_a_screen_nobody_has_built() {
        let outcomes = [outcome(
            "home",
            GoldenVerdict::CaptureFailed("unimplemented: home".into()),
        )];
        let report = judge(&outcomes, false);
        assert!(report.unimplemented.is_empty());
        assert_eq!(report.failed, ["home.light"]);
    }

    #[test]
    fn a_declared_flaky_pixel_difference_is_visible_but_does_not_fail() {
        let outcomes = [flaky_outcome(
            "strip",
            GoldenVerdict::Different {
                pixels: 900,
                first: (1, 2),
            },
        )];
        let report = judge(&outcomes, false);
        assert!(report.failed.is_empty());
        assert_eq!(report.flaky, ["strip.light"]);
    }

    #[test]
    fn flaky_metadata_does_not_forgive_a_broken_capture() {
        let outcomes = [flaky_outcome(
            "strip",
            GoldenVerdict::CaptureFailed("no window on screen".into()),
        )];
        let report = judge(&outcomes, false);
        assert!(report.flaky.is_empty());
        assert_eq!(report.failed, ["strip.light"]);
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
            diff(&expected, &actual, &out, 2, 0, &[]).expect("a verdict"),
            GoldenVerdict::Same
        );
        assert!(out.join("expected.png").is_file());
        assert!(out.join("actual.png").is_file());
        assert!(!out.join("diff.png").exists());
    }

    #[test]
    fn a_change_too_small_to_see_is_inside_the_tolerance() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        let actual = room.path().join("actual.png");
        write(&expected, 4, 4, [10, 20, 30, 255]);
        write(&actual, 4, 4, [11, 21, 31, 255]);
        assert_eq!(
            diff(&expected, &actual, &room.path().join("out"), 2, 0, &[]).expect("a verdict"),
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
        match diff(&expected, &actual, &room.path().join("out"), 2, 0, &[]).expect("a verdict") {
            GoldenVerdict::Different { pixels, first } => {
                assert_eq!(pixels, 16);
                assert_eq!(first, (0, 0));
            }
            other => panic!("expected a difference, got {other}"),
        }
        assert!(room.path().join("out/diff.png").is_file());
    }

    #[test]
    fn a_capture_of_another_size_is_not_compared_pixel_by_pixel() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        let actual = room.path().join("actual.png");
        write(&expected, 4, 4, [10, 20, 30, 255]);
        write(&actual, 8, 4, [10, 20, 30, 255]);
        assert_eq!(
            diff(&expected, &actual, &room.path().join("out"), 2, 0, &[]).expect("a verdict"),
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
                0,
                &[]
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
            &[],
        )
        .expect("a verdict");
        assert!(
            matches!(verdict, GoldenVerdict::CaptureFailed(_)),
            "expected a failed capture, got {verdict}"
        );
    }

    /// Paints one pixel of a solid image another colour.
    fn write_with_speck(path: &Path, width: u32, height: u32, colour: [u8; 4], at: (u32, u32)) {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            pixels.extend_from_slice(&colour);
        }
        let index = ((at.1 * width + at.0) * 4) as usize;
        pixels[index..index + 4].copy_from_slice(&[250, 250, 250, 255]);
        write_png(
            path,
            &Image {
                width,
                height,
                pixels,
            },
        )
        .expect("a PNG");
    }

    fn bar() -> SystemChrome {
        SystemChrome {
            what: "home indicator".into(),
            x: 1,
            y: 2,
            width: 2,
            height: 1,
        }
    }

    #[test]
    fn a_difference_under_system_chrome_is_not_a_difference() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        let actual = room.path().join("actual.png");
        write(&expected, 4, 4, [10, 20, 30, 255]);
        write_with_speck(&actual, 4, 4, [10, 20, 30, 255], (2, 2));
        let out = room.path().join("out");
        assert_eq!(
            diff(&expected, &actual, &out, 2, 0, &[]).expect("a verdict"),
            GoldenVerdict::Different {
                pixels: 1,
                first: (2, 2)
            }
        );
        assert_eq!(
            diff(&expected, &actual, &out, 2, 0, &[bar()]).expect("a verdict"),
            GoldenVerdict::Same
        );
    }

    #[test]
    fn a_difference_beside_system_chrome_still_counts() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        let actual = room.path().join("actual.png");
        write(&expected, 4, 4, [10, 20, 30, 255]);
        write_with_speck(&actual, 4, 4, [10, 20, 30, 255], (3, 2));
        let out = room.path().join("out");
        assert_eq!(
            diff(&expected, &actual, &out, 2, 0, &[bar()]).expect("a verdict"),
            GoldenVerdict::Different {
                pixels: 1,
                first: (3, 2)
            }
        );
        // The chrome is washed blue in the difference image whether or not
        // anything under it differed, so what was not compared is visible.
        let marked = read_png(&out.join("diff.png")).expect("a difference image");
        let at = ((2 * 4 + 1) * 4) as usize;
        assert!(marked.pixels[at + 2] > marked.pixels[at] + 100);
    }

    #[test]
    fn a_screen_on_an_undeclared_simulator_is_refused() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let path = room.path().join("manifest.json");
        std::fs::write(
            &path,
            r#"{"simulators": {}, "screens": [{"id": "probe", "stage": 4, "screen": "probe",
                "fixture": "probe", "origin": "added_state", "reason": "r",
                "simulator": "golden", "appearances": ["light"]}]}"#,
        )
        .expect("a manifest");
        let error = GoldenManifest::read(&path).expect_err("an undeclared simulator");
        assert!(error.to_string().contains("does not declare"), "{error}");
    }

    #[test]
    fn the_committed_manifest_owes_the_catalogue_and_the_added_states() {
        let manifest = GoldenManifest::read(Path::new("../../apps/apple/Goldens/manifest.json"))
            .expect("a manifest");
        let references: Vec<&GoldenScreen> = manifest
            .screens
            .iter()
            .filter(|screen| matches!(screen.origin, GoldenOrigin::Reference { .. }))
            .collect();
        let required_references = [
            "agent-delete",
            "ask-permission",
            "ask-question",
            "comment",
            "delete",
            "diff",
            "dump",
            "exited",
            "first-run",
            "first-run-paid",
            "home",
            "home-quiet",
            "hosts",
            "new-agent",
            "offline",
            "overflow",
            "paywall",
            "pin",
            "plan",
            "plus",
            "profiles",
            "queued",
            "review-cta",
            "run",
            "run-live",
            "settings",
            "shake",
            "sign-in",
            "slash-typing",
            "typing",
            "voices",
            "working",
            "you",
        ];
        let reference_ids: std::collections::BTreeSet<_> =
            references.iter().map(|screen| screen.id.as_str()).collect();
        assert_eq!(reference_ids, required_references.into_iter().collect());
        assert_eq!(
            references.len(),
            required_references.len(),
            "no duplicate references"
        );
        for id in [
            "probe",
            "finished",
            "stale",
            "codex-approval",
            "rename",
            "permissions-claude",
            "permissions-codex",
            "send-refused",
            "strip",
            "tokens",
            "devices",
            "hosts-groups",
            "local-network-refused",
            "code-entry",
            "pair-confirmation",
            "found-host",
            "pair-confirm",
            "delete-blocked",
            "paywall-unconfirmed",
            "sign-in-failed",
            "you-granted",
            "upload-failed",
            "ax-conversation",
            "ax-composer",
            "reduced-glass",
            "ax-home",
            "unreadable-agent",
            "small-home",
            "small-conversation",
        ] {
            assert!(
                matches!(
                    manifest.screen(id).map(|screen| &screen.origin),
                    Some(GoldenOrigin::AddedState { .. })
                ),
                "missing added state: {id}"
            );
        }
        let baseline_notes =
            std::fs::read_to_string("../../apps/apple/Goldens/BASELINE.md").unwrap();
        for screen in &references {
            assert!(
                baseline_notes
                    .lines()
                    .any(|line| line == format!("## {}", screen.id)),
                "{} has no baseline explanation",
                screen.id
            );
            let GoldenOrigin::Reference { capture } = &screen.origin else {
                unreachable!()
            };
            for appearance in &screen.appearances {
                assert!(
                    Path::new(&format!(
                        "../../apps/apple/Goldens/References/{capture}.only.{appearance}.png"
                    ))
                    .is_file(),
                    "missing preserved reference for {}.{appearance}",
                    screen.id
                );
            }
        }
        assert!(
            manifest.screen("notification").is_none(),
            "notifications are out of scope"
        );
        assert!(manifest.screen("probe").is_some(), "the probe is owed");
        for screen in &manifest.screens {
            assert_eq!(
                screen.appearances,
                [Appearance::Light, Appearance::Dark],
                "{} requires both appearances exactly once",
                screen.id
            );
            for appearance in &screen.appearances {
                assert!(
                    Path::new(&format!(
                        "../../apps/apple/Goldens/{}.{appearance}.png",
                        screen.id
                    ))
                    .is_file(),
                    "missing baseline for {}.{appearance}",
                    screen.id
                );
            }
            assert!(
                screen.simulator == "golden" || screen.simulator == "small",
                "{} names an unpinned simulator",
                screen.id
            );
            if let GoldenOrigin::AddedState { reason } = &screen.origin {
                assert!(!reason.is_empty(), "{} says why it is owed", screen.id);
            }
        }
        // Every checked-in baseline is claimed by the manifest. Everything
        // above reads the manifest and looks for the file; this reads the
        // directory and looks for the entry, which is the only direction that
        // catches a screen dropped from the catalogue with its photographs
        // left behind, or a stray capture committed by hand.
        let claimed: std::collections::BTreeSet<String> = manifest
            .screens
            .iter()
            .flat_map(|screen| {
                screen
                    .appearances
                    .iter()
                    .map(move |appearance| format!("{}.{appearance}.png", screen.id))
            })
            .collect();
        let mut orphans: Vec<String> = std::fs::read_dir("../../apps/apple/Goldens")
            .expect("the baselines")
            .map(|entry| {
                entry
                    .expect("a baseline")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.ends_with(".png") && !claimed.contains(name))
            .collect();
        orphans.sort();
        assert_eq!(orphans, Vec::<String>::new(), "baselines nothing claims");
        let mut flaky_captures: Vec<_> = manifest
            .screens
            .iter()
            .flat_map(|screen| {
                screen.flaky.iter().flat_map(|flake| {
                    flake
                        .appearances
                        .iter()
                        .map(|appearance| format!("{}.{}", screen.id, appearance))
                })
            })
            .collect();
        flaky_captures.sort();
        // Nothing is quarantined. A capture that will not repeat itself is a
        // bug in what it photographs, and the three that used to stand here
        // were fixed rather than excused; a new entry has to make that same
        // argument again in the open.
        assert_eq!(flaky_captures, Vec::<String>::new());

        // Every pinned device is declared with the chrome the comparison
        // must look past; only the Face ID phone has a home indicator.
        let golden = manifest.simulators.get("golden").expect("the golden phone");
        let named: Vec<&str> = golden
            .system_chrome
            .iter()
            .map(|chrome| chrome.what.split(':').next().unwrap_or(""))
            .collect();
        assert_eq!(
            named,
            [
                "status bar clock",
                "status bar indicators",
                "home indicator"
            ]
        );
        let bar = &golden.system_chrome[2];
        assert_eq!((bar.x, bar.y, bar.width, bar.height), (384, 2580, 438, 21));
        let small = manifest.simulators.get("small").expect("the small phone");
        assert_eq!(
            small.system_chrome.len(),
            2,
            "a home button, so no indicator"
        );
    }
}
