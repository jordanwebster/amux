use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const BASELINE_SCHEMA: u32 = 1;
const BASELINE_ROOT: &str = "crates/testnet/perf/baselines";

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
            Self::Bytes => "bytes",
            Self::Megabytes => "MiB",
            Self::Percent => "%",
            Self::Ratio => "×",
        }
    }

    const fn drift_limit(self) -> f64 {
        match self {
            Self::Bytes | Self::Megabytes | Self::Ratio => 0.10,
            Self::Milliseconds | Self::Microseconds | Self::Percent => 0.15,
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
    pub workload: Workload,
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
    pub medians: BTreeMap<String, f64>,
}

impl Baselines {
    pub fn read(path: &Path, machine: &Machine) -> Result<Option<Self>, PerfError> {
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
        Ok(Some(value))
    }
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
}

impl Report {
    pub fn evaluate(
        machine: Machine,
        runs: Vec<MetricRun>,
        baselines: Option<&Baselines>,
        recording: bool,
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
        if let Some(baselines) = baselines {
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
            let baseline =
                baselines.and_then(|values| values.medians.get(run.metric.name).copied());
            let drift = baseline.map(|baseline| median / baseline - 1.0);
            let within_drift =
                recording || drift.is_none_or(|drift| drift <= run.metric.unit.drift_limit());
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
        })
    }

    pub fn passed(&self) -> bool {
        self.verdicts.iter().all(|verdict| verdict.pass)
    }

    pub fn print(&self) {
        println!(
            "machine: {} ({}) · OS: {} · profile: {} · features: {}",
            self.machine.name, self.machine.model, self.machine.os, self.profile, self.features
        );
        println!("metric | median | measured | budget | baseline | drift | verdict");
        for (run, verdict) in self.runs.iter().zip(&self.verdicts) {
            let unit = run.metric.unit.name();
            let baseline = verdict
                .baseline
                .map_or_else(|| "—".to_owned(), |value| format!("{value:.3} {unit}"));
            let drift = verdict
                .drift
                .map_or_else(|| "—".to_owned(), |value| format!("{:+.1}%", value * 100.0));
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
                run.samples.len(),
                run.metric.workload.warm_up,
                run.started_at.to_rfc3339(),
                run.ended_at.to_rfc3339(),
            );
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
            medians: self
                .verdicts
                .iter()
                .map(|verdict| (verdict.metric.to_owned(), verdict.median))
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
        MetricRun {
            metric: Metric {
                name: "fixture",
                statistic,
                budget: 20.0,
                unit,
                workload: WORKLOAD,
            },
            samples: values
                .iter()
                .map(|value| Sample {
                    metric: "fixture",
                    value: *value,
                    unit,
                })
                .collect(),
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
            medians: BTreeMap::from([("fixture".to_owned(), 10.0)]),
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
    fn perf_report_uses_the_tighter_memory_drift_limit() {
        let baseline = Baselines {
            schema_version: BASELINE_SCHEMA,
            machine_model: "Mac14,6".to_owned(),
            profile: "release".to_owned(),
            features: "bundled,perf".to_owned(),
            medians: BTreeMap::from([("fixture".to_owned(), 10.0)]),
        };
        let report = Report::evaluate(
            machine(),
            vec![run(Unit::Megabytes, Statistic::Peak, &[10.0, 11.1])],
            Some(&baseline),
            false,
        )
        .unwrap();
        assert!(!report.passed(), "11% memory drift exceeds the 10% limit");
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
        let read = Baselines::read(&path, &machine()).unwrap().unwrap();
        assert_eq!(read.medians["fixture"], 2.0);
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
}
