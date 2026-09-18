use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const BASELINE_SCHEMA: u32 = 1;
const BASELINE_ROOT: &str = "crates/testnet/perf/baselines";
pub const DESKTOP_REFERENCE_STATE: &str = "one busy core (cluster warmer)";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Statistic {
    Median,
    P99,
    Worst,
    Peak,
}

impl Statistic {
    fn value(self, sorted: &[f64]) -> f64 {
        match self {
            Self::Median => sorted[sorted.len() / 2],
            Self::P99 => sorted[(sorted.len() * 99).div_ceil(100) - 1],
            Self::Worst | Self::Peak => *sorted.last().expect("non-empty samples"),
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Median => "median",
            Self::P99 => "p99",
            Self::Worst => "worst",
            Self::Peak => "peak",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Milliseconds,
    Microseconds,
    MegabytesPerMinute,
    Bytes,
    Megabytes,
    Percent,
    Ratio,
}

impl Unit {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Milliseconds => "ms",
            Self::Microseconds => "µs",
            Self::MegabytesPerMinute => "MiB/min",
            Self::Bytes => "bytes",
            Self::Megabytes => "MiB",
            Self::Percent => "%",
            Self::Ratio => "×",
        }
    }

    const fn drift_limit(self) -> Option<f64> {
        match self {
            Self::MegabytesPerMinute => None,
            Self::Bytes | Self::Megabytes | Self::Ratio => Some(0.10),
            Self::Milliseconds | Self::Microseconds | Self::Percent => Some(0.15),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Workload {
    pub description: &'static str,
    pub seed: u64,
    pub identity_growth: &'static str,
    pub warm_up: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub struct Metric {
    pub name: &'static str,
    pub statistic: Statistic,
    pub budget: f64,
    pub unit: Unit,
    pub ceiling_only: bool,
    pub workload: Workload,
}

impl Metric {
    const fn drift_limit(self) -> Option<f64> {
        if self.ceiling_only {
            None
        } else {
            self.unit.drift_limit()
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub metric: &'static str,
    pub value: f64,
    pub unit: Unit,
}

#[derive(Clone, Debug)]
pub struct MetricRun {
    pub metric: Metric,
    pub samples: Vec<Sample>,
    /// Raw workload observations represented by `samples`.
    ///
    /// Most metrics retain every observation as a sample. Derived metrics,
    /// such as a regression slope, retain one reportable value while still
    /// naming the full observation count required by the measurement contract.
    pub observation_count: usize,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    pub metric: &'static str,
    pub median: f64,
    pub measured: f64,
    pub statistic: Statistic,
    pub budget: f64,
    pub baseline: Option<f64>,
    pub drift: Option<f64>,
    pub pass: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Baselines {
    schema_version: u32,
    pub machine_model: String,
    profile: String,
    features: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_state: Option<String>,
    pub medians: BTreeMap<String, Option<f64>>,
}

impl Baselines {
    pub fn read(
        path: &Path,
        machine: &Machine,
        reference_state: Option<&str>,
    ) -> Result<Option<Self>, PerfError> {
        if !path.is_file() {
            return Ok(None);
        }
        let value: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        if value.schema_version != BASELINE_SCHEMA {
            return Err(PerfError::Baseline(format!(
                "{} has schema {}, expected {BASELINE_SCHEMA}",
                path.display(),
                value.schema_version
            )));
        }
        if value.machine_model != machine.model {
            return Err(PerfError::Baseline(format!(
                "{} names {}, but this run is {}",
                path.display(),
                value.machine_model,
                machine.model
            )));
        }
        if value.profile != "release" || value.features != "bundled,perf" {
            return Err(PerfError::Baseline(format!(
                "{} records profile={} features={}, expected profile=release features=bundled,perf",
                path.display(),
                value.profile,
                value.features
            )));
        }
        if value.reference_state.as_deref() != reference_state {
            return Err(PerfError::Baseline(format!(
                "{} records reference state {}, but this run uses {}",
                path.display(),
                display_reference_state(value.reference_state.as_deref()),
                display_reference_state(reference_state),
            )));
        }
        Ok(Some(value))
    }

    pub fn project(&self, metrics: &[&str]) -> Result<Self, PerfError> {
        let mut medians = BTreeMap::new();
        for metric in metrics {
            let value = self
                .medians
                .get(*metric)
                .ok_or_else(|| PerfError::Baseline(format!("baseline has no metric {metric:?}")))?;
            medians.insert((*metric).to_owned(), *value);
        }
        Ok(Self {
            schema_version: self.schema_version,
            machine_model: self.machine_model.clone(),
            profile: self.profile.clone(),
            features: self.features.clone(),
            reference_state: self.reference_state.clone(),
            medians,
        })
    }
}

fn display_reference_state(reference_state: Option<&str>) -> &str {
    reference_state.unwrap_or("unspecified")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    pub name: &'static str,
    pub model: String,
    pub os: String,
}

impl Machine {
    pub fn detect() -> Result<Self, PerfError> {
        Self::from_model(observed_model()?, observed_os())
    }

    fn from_model(model: String, os: String) -> Result<Self, PerfError> {
        let row = MACHINES
            .iter()
            .find(|row| row.model == model)
            .ok_or_else(|| PerfError::UnknownMachine(model.clone()))?;
        Ok(Self {
            name: row.name,
            model,
            os,
        })
    }

    pub fn baseline_path(&self) -> PathBuf {
        Path::new(BASELINE_ROOT).join(format!("{}.json", self.model))
    }

    pub fn soak_baseline_path(&self) -> PathBuf {
        Path::new(BASELINE_ROOT).join(format!("{}-soak.json", self.model))
    }
}

#[derive(Clone, Copy)]
struct MachineRow {
    name: &'static str,
    model: &'static str,
}

const MACHINES: &[MachineRow] = &[MachineRow {
    name: "pinned-mac",
    model: "Mac14,6",
}];

#[derive(Clone, Debug)]
pub struct Report {
    pub machine: Machine,
    pub profile: &'static str,
    pub features: &'static str,
    pub verdicts: Vec<Verdict>,
    pub runs: Vec<MetricRun>,
    reference_state: Option<&'static str>,
    diagnostic: bool,
}

impl Report {
    pub fn evaluate(
        machine: Machine,
        runs: Vec<MetricRun>,
        baselines: Option<&Baselines>,
        recording: bool,
    ) -> Result<Self, PerfError> {
        Self::evaluate_mode(
            machine,
            runs,
            baselines,
            recording,
            Some(DESKTOP_REFERENCE_STATE),
            false,
        )
    }

    pub fn evaluate_soak(
        machine: Machine,
        runs: Vec<MetricRun>,
        baselines: Option<&Baselines>,
        recording: bool,
    ) -> Result<Self, PerfError> {
        Self::evaluate_mode(machine, runs, baselines, recording, None, false)
    }

    pub fn evaluate_diagnostic(machine: Machine, runs: Vec<MetricRun>) -> Result<Self, PerfError> {
        Self::evaluate_mode(machine, runs, None, false, None, true)
    }

    fn evaluate_mode(
        machine: Machine,
        runs: Vec<MetricRun>,
        baselines: Option<&Baselines>,
        recording: bool,
        reference_state: Option<&'static str>,
        diagnostic: bool,
    ) -> Result<Self, PerfError> {
        let metric_names = runs
            .iter()
            .map(|run| run.metric.name)
            .collect::<BTreeSet<_>>();
        if metric_names.len() != runs.len() {
            return Err(PerfError::Baseline(
                "performance metric names must be unique".to_owned(),
            ));
        }
        if let Some(baselines) = baselines.filter(|_| !recording) {
            let baseline_names = baselines
                .medians
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            if baseline_names != metric_names {
                return Err(PerfError::Baseline(
                    "baseline metric set does not match this performance run".to_owned(),
                ));
            }
        }
        let mut verdicts = Vec::with_capacity(runs.len());
        for run in &runs {
            if run.samples.is_empty() {
                return Err(PerfError::NoSamples(run.metric.name));
            }
            if run.samples.iter().any(|sample| {
                sample.metric != run.metric.name
                    || sample.unit != run.metric.unit
                    || !sample.value.is_finite()
                    || sample.value < 0.0
            }) {
                return Err(PerfError::InvalidSamples(run.metric.name));
            }
            let mut values = run
                .samples
                .iter()
                .map(|sample| sample.value)
                .collect::<Vec<_>>();
            values.sort_by(f64::total_cmp);
            let median = Statistic::Median.value(&values);
            let measured = run.metric.statistic.value(&values);
            let drift_limit = run.metric.drift_limit();
            let baseline = drift_limit.and_then(|_| {
                baselines
                    .and_then(|values| values.medians.get(run.metric.name))
                    .copied()
                    .flatten()
            });
            let drift = baseline.map(|baseline| median / baseline - 1.0);
            let within_drift = recording
                || drift_limit
                    .zip(drift)
                    .is_none_or(|(limit, drift)| drift <= limit);
            verdicts.push(Verdict {
                metric: run.metric.name,
                median,
                measured,
                statistic: run.metric.statistic,
                budget: run.metric.budget,
                baseline,
                drift,
                pass: measured <= run.metric.budget && within_drift,
            });
        }
        Ok(Self {
            machine,
            profile: "release",
            features: "bundled,perf",
            verdicts,
            runs,
            reference_state,
            diagnostic,
        })
    }

    pub fn validate_recording(recording: bool, diagnostic: bool) -> Result<(), PerfError> {
        if recording && diagnostic {
            return Err(PerfError::Baseline(
                "a shortened diagnostic soak cannot become a baseline".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn passed(&self) -> bool {
        self.verdicts.iter().all(|verdict| verdict.pass)
    }

    pub fn print(&self) {
        println!(
            "machine: {} ({}) · OS: {} · profile: {} · features: {}",
            self.machine.name, self.machine.model, self.machine.os, self.profile, self.features
        );
        if let Some(reference_state) = self.reference_state {
            println!("reference state: {reference_state}");
        }
        if self.diagnostic {
            println!("drift: not applied to diagnostic runs");
        }
        println!("metric | median | measured | budget | baseline | drift | verdict");
        for (run, verdict) in self.runs.iter().zip(&self.verdicts) {
            let unit = run.metric.unit.name();
            let ceiling_only = run.metric.drift_limit().is_none();
            let baseline = if ceiling_only {
                "ceiling only".to_owned()
            } else {
                verdict.baseline.map_or_else(
                    || "unavailable".to_owned(),
                    |value| format!("{value:.3} {unit}"),
                )
            };
            let drift = if ceiling_only {
                "ceiling only".to_owned()
            } else {
                verdict.drift.map_or_else(
                    || "unavailable".to_owned(),
                    |value| format!("{:+.1}%", value * 100.0),
                )
            };
            println!(
                "{} | {:.3} {} | {} {:.3} {} | {:.3} {} | {} | {} | {}",
                verdict.metric,
                verdict.median,
                unit,
                verdict.statistic.name(),
                verdict.measured,
                unit,
                verdict.budget,
                unit,
                baseline,
                drift,
                if verdict.pass { "PASS" } else { "FAIL" }
            );
            println!(
                "  workload={} · seed={} · identities={} · samples={} · warm-up={} · start={} · end={}",
                run.metric.workload.description,
                run.metric.workload.seed,
                run.metric.workload.identity_growth,
                run.observation_count,
                run.metric.workload.warm_up,
                run.started_at.to_rfc3339(),
                run.ended_at.to_rfc3339(),
            );
            if verdict.baseline.is_none() && !ceiling_only && !self.diagnostic {
                println!("  no committed baseline for this workload");
            }
        }
    }

    pub fn write_baseline(&self, path: &Path) -> Result<(), PerfError> {
        if !self
            .verdicts
            .iter()
            .all(|verdict| verdict.measured <= verdict.budget)
        {
            return Err(PerfError::Baseline(
                "a run outside an absolute budget cannot become a baseline".to_owned(),
            ));
        }
        let values = Baselines {
            schema_version: BASELINE_SCHEMA,
            machine_model: self.machine.model.clone(),
            profile: self.profile.to_owned(),
            features: self.features.to_owned(),
            reference_state: self.reference_state.map(str::to_owned),
            medians: self
                .runs
                .iter()
                .zip(&self.verdicts)
                .map(|(run, verdict)| {
                    (
                        verdict.metric.to_owned(),
                        run.metric.drift_limit().map(|_| verdict.median),
                    )
                })
                .collect(),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut encoded = serde_json::to_string_pretty(&values)?;
        encoded.push('\n');
        std::fs::write(path, encoded)?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum PerfError {
    #[error("unknown performance machine {0}; enroll its hardware model before measuring")]
    UnknownMachine(String),
    #[error("metric {0} produced no samples")]
    NoSamples(&'static str),
    #[error("metric {0} produced a mismatched or non-finite sample")]
    InvalidSamples(&'static str),
    #[error("invalid performance baseline: {0}")]
    Baseline(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(target_os = "macos")]
fn observed_model() -> Result<String, PerfError> {
    use std::ffi::CString;

    unsafe extern "C" {
        fn sysctlbyname(
            name: *const libc::c_char,
            oldp: *mut libc::c_void,
            oldlenp: *mut libc::size_t,
            newp: *mut libc::c_void,
            newlen: libc::size_t,
        ) -> libc::c_int;
    }

    let name = CString::new("hw.model").expect("static sysctl name");
    let mut length = 0;
    // SAFETY: the first call requests only the required buffer length.
    if unsafe {
        sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut bytes = vec![0_u8; length];
    // SAFETY: `bytes` owns `length` writable bytes and no new value is supplied.
    if unsafe {
        sysctlbyname(
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    bytes.truncate(length);
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    String::from_utf8(bytes)
        .map_err(|error| PerfError::Baseline(format!("hw.model was not UTF-8: {error}")))
}

#[cfg(target_os = "linux")]
fn observed_model() -> Result<String, PerfError> {
    Ok(
        std::fs::read_to_string("/sys/devices/virtual/dmi/id/product_name")?
            .trim()
            .to_owned(),
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn observed_model() -> Result<String, PerfError> {
    Err(PerfError::UnknownMachine(std::env::consts::OS.to_owned()))
}

fn observed_os() -> String {
    let command = if cfg!(target_os = "macos") {
        std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
    } else {
        std::process::Command::new("uname").arg("-sr").output()
    };
    command
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| std::env::consts::OS.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKLOAD: Workload = Workload {
        description: "report fixture",
        seed: 1,
        identity_growth: "fixed",
        warm_up: "one sample",
    };

    fn run(unit: Unit, statistic: Statistic, values: &[f64]) -> MetricRun {
        named_run("fixture", unit, statistic, values)
    }

    fn named_run(
        name: &'static str,
        unit: Unit,
        statistic: Statistic,
        values: &[f64],
    ) -> MetricRun {
        MetricRun {
            metric: Metric {
                name,
                statistic,
                budget: 20.0,
                unit,
                ceiling_only: false,
                workload: WORKLOAD,
            },
            samples: values
                .iter()
                .map(|value| Sample {
                    metric: name,
                    value: *value,
                    unit,
                })
                .collect(),
            observation_count: values.len(),
            started_at: Utc::now(),
            ended_at: Utc::now(),
        }
    }

    fn machine() -> Machine {
        Machine {
            name: "pinned-mac",
            model: "Mac14,6".to_owned(),
            os: "test".to_owned(),
        }
    }

    #[test]
    fn perf_report_applies_the_named_statistic_budget_and_time_drift() {
        let baseline = Baselines {
            schema_version: BASELINE_SCHEMA,
            machine_model: "Mac14,6".to_owned(),
            profile: "release".to_owned(),
            features: "bundled,perf".to_owned(),
            reference_state: Some(DESKTOP_REFERENCE_STATE.to_owned()),
            medians: BTreeMap::from([("fixture".to_owned(), Some(10.0))]),
        };
        let report = Report::evaluate(
            machine(),
            vec![run(Unit::Milliseconds, Statistic::P99, &[9.0, 11.6, 12.0])],
            Some(&baseline),
            false,
        )
        .unwrap();
        let verdict = &report.verdicts[0];
        assert_eq!(verdict.median, 11.6);
        assert_eq!(verdict.measured, 12.0);
        assert!(!verdict.pass, "16% median drift exceeds the 15% time limit");
    }

    #[test]
    fn perf_baseline_can_be_projected_for_a_focused_run() {
        let baseline = Baselines {
            schema_version: BASELINE_SCHEMA,
            machine_model: "Mac14,6".to_owned(),
            profile: "release".to_owned(),
            features: "bundled,perf".to_owned(),
            reference_state: Some(DESKTOP_REFERENCE_STATE.to_owned()),
            medians: BTreeMap::from([
                ("fixture".to_owned(), Some(10.0)),
                ("other".to_owned(), Some(20.0)),
            ]),
        };
        let projected = baseline.project(&["fixture"]).unwrap();

        assert_eq!(
            projected.medians,
            BTreeMap::from([("fixture".to_owned(), Some(10.0))])
        );
        Report::evaluate(
            machine(),
            vec![run(Unit::Milliseconds, Statistic::Median, &[9.0])],
            Some(&projected),
            false,
        )
        .unwrap();
        assert!(baseline.project(&["missing"]).is_err());
    }

    #[test]
    fn perf_report_uses_the_tighter_memory_drift_limit() {
        let baseline = Baselines {
            schema_version: BASELINE_SCHEMA,
            machine_model: "Mac14,6".to_owned(),
            profile: "release".to_owned(),
            features: "bundled,perf".to_owned(),
            reference_state: Some(DESKTOP_REFERENCE_STATE.to_owned()),
            medians: BTreeMap::from([("fixture".to_owned(), Some(10.0))]),
        };
        let report = Report::evaluate(
            machine(),
            vec![run(Unit::Megabytes, Statistic::Peak, &[10.0, 11.1])],
            Some(&baseline),
            false,
        )
        .unwrap();
        assert!(!report.passed(), "11% memory drift exceeds the 10% limit");

        let passing = Report::evaluate(
            machine(),
            vec![run(Unit::Megabytes, Statistic::Peak, &[10.9])],
            Some(&baseline),
            false,
        )
        .unwrap();
        assert!(
            passing.passed(),
            "9% memory drift remains within the 10% limit"
        );
    }

    #[test]
    fn slope_rows_are_ceiling_only_even_with_a_numeric_baseline() {
        let baseline = Baselines {
            schema_version: BASELINE_SCHEMA,
            machine_model: "Mac14,6".to_owned(),
            profile: "release".to_owned(),
            features: "bundled,perf".to_owned(),
            reference_state: Some(DESKTOP_REFERENCE_STATE.to_owned()),
            medians: BTreeMap::from([("fixture".to_owned(), Some(0.001))]),
        };
        let report = Report::evaluate(
            machine(),
            vec![run(Unit::MegabytesPerMinute, Statistic::Worst, &[0.9])],
            Some(&baseline),
            false,
        )
        .unwrap();
        assert!(report.passed());
        assert_eq!(report.verdicts[0].baseline, None);
        assert_eq!(report.verdicts[0].drift, None);
    }

    #[test]
    fn per_metric_ceiling_only_rows_ignore_and_retire_numeric_baselines() {
        let baseline = Baselines {
            schema_version: BASELINE_SCHEMA,
            machine_model: "Mac14,6".to_owned(),
            profile: "release".to_owned(),
            features: "bundled,perf".to_owned(),
            reference_state: Some(DESKTOP_REFERENCE_STATE.to_owned()),
            medians: BTreeMap::from([("fixture".to_owned(), Some(0.001))]),
        };
        let mut metric_run = run(Unit::Percent, Statistic::Median, &[0.9]);
        metric_run.metric.ceiling_only = true;
        let report = Report::evaluate(machine(), vec![metric_run], Some(&baseline), false).unwrap();

        assert!(report.passed());
        assert_eq!(report.verdicts[0].baseline, None);
        assert_eq!(report.verdicts[0].drift, None);

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Mac14,6.json");
        report.write_baseline(&path).unwrap();
        let recorded = Baselines::read(&path, &machine(), Some(DESKTOP_REFERENCE_STATE))
            .unwrap()
            .unwrap();
        assert_eq!(recorded.medians["fixture"], None);
    }

    #[test]
    fn percent_rows_without_the_flag_keep_the_fifteen_percent_drift_gate() {
        let baseline = Baselines {
            schema_version: BASELINE_SCHEMA,
            machine_model: "Mac14,6".to_owned(),
            profile: "release".to_owned(),
            features: "bundled,perf".to_owned(),
            reference_state: Some(DESKTOP_REFERENCE_STATE.to_owned()),
            medians: BTreeMap::from([("fixture".to_owned(), Some(10.0))]),
        };
        let report = Report::evaluate(
            machine(),
            vec![run(Unit::Percent, Statistic::Median, &[11.6])],
            Some(&baseline),
            false,
        )
        .unwrap();

        assert!(!report.passed());
        assert_eq!(report.verdicts[0].baseline, Some(10.0));
        assert!(report.verdicts[0].drift.unwrap() > 0.15);
    }

    #[test]
    fn soak_baseline_records_null_for_slope_rows() {
        let report = Report::evaluate_soak(
            machine(),
            vec![
                named_run("slope", Unit::MegabytesPerMinute, Statistic::Worst, &[0.2]),
                named_run("peak", Unit::Megabytes, Statistic::Peak, &[12.0]),
            ],
            None,
            true,
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Mac14,6-soak.json");
        report.write_baseline(&path).unwrap();
        let baseline = Baselines::read(&path, &machine(), None).unwrap().unwrap();
        assert_eq!(baseline.reference_state, None);
        assert_eq!(baseline.medians["slope"], None);
        assert_eq!(baseline.medians["peak"], Some(12.0));
    }

    #[test]
    fn diagnostic_soaks_are_refused_before_baseline_recording() {
        assert!(Report::validate_recording(true, true).is_err());
        assert!(Report::validate_recording(false, true).is_ok());
        assert!(Report::validate_recording(true, false).is_ok());
    }

    #[test]
    fn fast_and_soak_baselines_have_distinct_paths() {
        let machine = machine();
        assert_eq!(
            machine.baseline_path(),
            Path::new(BASELINE_ROOT).join("Mac14,6.json")
        );
        assert_eq!(
            machine.soak_baseline_path(),
            Path::new(BASELINE_ROOT).join("Mac14,6-soak.json")
        );
    }

    #[test]
    fn perf_baseline_round_trips_by_hardware_model() {
        let report = Report::evaluate(
            machine(),
            vec![run(Unit::Milliseconds, Statistic::Median, &[1.0, 2.0, 3.0])],
            None,
            true,
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Mac14,6.json");
        report.write_baseline(&path).unwrap();
        let read = Baselines::read(&path, &machine(), Some(DESKTOP_REFERENCE_STATE))
            .unwrap()
            .unwrap();
        assert_eq!(
            read.reference_state.as_deref(),
            Some(DESKTOP_REFERENCE_STATE)
        );
        assert_eq!(read.medians["fixture"], Some(2.0));
    }

    #[test]
    fn perf_baseline_refuses_a_different_reference_state() {
        let report = Report::evaluate(
            machine(),
            vec![run(Unit::Milliseconds, Statistic::Median, &[1.0])],
            None,
            true,
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Mac14,6.json");
        report.write_baseline(&path).unwrap();

        let error = Baselines::read(&path, &machine(), Some("idle machine"))
            .unwrap_err()
            .to_string();
        assert!(error.contains(DESKTOP_REFERENCE_STATE), "{error}");
        assert!(error.contains("idle machine"), "{error}");
    }

    #[test]
    fn perf_report_p99_excludes_one_outlier_in_one_hundred_samples() {
        let values = (1..=100).map(f64::from).collect::<Vec<_>>();
        let report = Report::evaluate(
            machine(),
            vec![run(Unit::Milliseconds, Statistic::P99, &values)],
            None,
            true,
        )
        .unwrap();
        assert_eq!(report.verdicts[0].measured, 99.0);
    }

    #[test]
    fn perf_report_refuses_unknown_machine_models() {
        let error = Machine::from_model("future-mac".to_owned(), "test".to_owned()).unwrap_err();
        assert!(matches!(error, PerfError::UnknownMachine(model) if model == "future-mac"));
    }

    #[test]
    fn perf_report_refuses_an_incomplete_baseline() {
        let baseline = Baselines {
            schema_version: BASELINE_SCHEMA,
            machine_model: "Mac14,6".to_owned(),
            profile: "release".to_owned(),
            features: "bundled,perf".to_owned(),
            reference_state: Some(DESKTOP_REFERENCE_STATE.to_owned()),
            medians: BTreeMap::new(),
        };
        let error = Report::evaluate(
            machine(),
            vec![run(Unit::Milliseconds, Statistic::Median, &[1.0])],
            Some(&baseline),
            false,
        )
        .unwrap_err();
        assert!(matches!(error, PerfError::Baseline(_)));
    }

    #[test]
    fn perf_report_accepts_a_null_baseline_without_disabling_the_budget() {
        let baseline = Baselines {
            schema_version: BASELINE_SCHEMA,
            machine_model: "Mac14,6".to_owned(),
            profile: "release".to_owned(),
            features: "bundled,perf".to_owned(),
            reference_state: Some(DESKTOP_REFERENCE_STATE.to_owned()),
            medians: BTreeMap::from([("fixture".to_owned(), None)]),
        };
        let passing = Report::evaluate(
            machine(),
            vec![run(Unit::Milliseconds, Statistic::Median, &[19.0])],
            Some(&baseline),
            false,
        )
        .unwrap();
        assert!(passing.passed());
        assert_eq!(passing.verdicts[0].baseline, None);
        assert_eq!(passing.verdicts[0].drift, None);

        let failing = Report::evaluate(
            machine(),
            vec![run(Unit::Milliseconds, Statistic::Median, &[21.0])],
            Some(&baseline),
            false,
        )
        .unwrap();
        assert!(
            !failing.passed(),
            "the absolute budget remains authoritative"
        );
    }

    #[test]
    fn baseline_recording_accepts_a_changed_metric_set() {
        let baseline = Baselines {
            schema_version: BASELINE_SCHEMA,
            machine_model: "Mac14,6".to_owned(),
            profile: "release".to_owned(),
            features: "bundled,perf".to_owned(),
            reference_state: Some(DESKTOP_REFERENCE_STATE.to_owned()),
            medians: BTreeMap::new(),
        };
        let report = Report::evaluate(
            machine(),
            vec![run(Unit::Milliseconds, Statistic::Median, &[1.0])],
            Some(&baseline),
            true,
        )
        .unwrap();
        assert!(report.passed());
        assert_eq!(report.verdicts[0].baseline, None);
    }
}
