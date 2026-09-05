use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "error")]
enum CiStatusError {
    WrongBranch { expected: String },
    DirtyTree,
    NotPushed,
    NoRunForHead,
    StillRunning { run_id: u64 },
    Failed { run_id: u64, job: String },
    JobAbsent { run_id: u64 },
    ToolFailure { message: String },
}

#[derive(Debug, PartialEq, Serialize)]
struct CiRun {
    run_id: u64,
    url: String,
    head: String,
    ios_job_duration_secs: u64,
}

#[derive(Clone, Deserialize)]
struct Run {
    id: u64,
    html_url: String,
    head_sha: String,
    status: String,
    conclusion: Option<String>,
}

#[derive(Deserialize)]
struct Runs {
    workflow_runs: Vec<Run>,
}

#[derive(Clone, Deserialize)]
struct Job {
    name: String,
    status: String,
    conclusion: Option<String>,
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
    steps: Vec<Step>,
}

#[derive(Clone, Deserialize)]
struct Step {
    name: String,
    conclusion: Option<String>,
}

#[derive(Deserialize)]
struct Jobs {
    jobs: Vec<Job>,
}

fn evaluate(
    head: &str,
    remote: &str,
    run: Option<&Run>,
    jobs: &[Job],
) -> Result<CiRun, CiStatusError> {
    if head != remote {
        return Err(CiStatusError::NotPushed);
    }
    let run = run
        .filter(|run| run.head_sha == head)
        .ok_or(CiStatusError::NoRunForHead)?;
    let failure = |job: &str| CiStatusError::Failed {
        run_id: run.id,
        job: job.into(),
    };
    // A failed job is actionable even while other matrix jobs are still running.
    if let Some(job) = jobs.iter().find(|job| {
        job.status == "completed"
            && !matches!(job.conclusion.as_deref(), Some("success" | "skipped"))
    }) {
        return Err(failure(&job.name));
    }
    if run.status != "completed" {
        return Err(CiStatusError::StillRunning { run_id: run.id });
    }
    let ios = jobs
        .iter()
        .find(|job| job.name == "ios")
        .ok_or(CiStatusError::JobAbsent { run_id: run.id })?;
    if ios.status != "completed" {
        return Err(CiStatusError::StillRunning { run_id: run.id });
    }
    if ios.conclusion.as_deref() != Some("success")
        || !ios.steps.iter().any(|step| {
            step.name == "Run iOS verification" && step.conclusion.as_deref() == Some("success")
        })
    {
        return Err(failure("ios"));
    }
    if run.conclusion.as_deref() != Some("success") {
        return Err(failure("workflow"));
    }
    let duration = ios
        .started_at
        .zip(ios.completed_at)
        .map(|(start, end)| (end - start).num_seconds())
        .filter(|seconds| *seconds >= 0)
        .ok_or_else(|| failure("ios: missing or invalid duration"))?;
    Ok(CiRun {
        run_id: run.id,
        url: run.html_url.clone(),
        head: head.into(),
        ios_job_duration_secs: duration as u64,
    })
}

fn command(program: &str, args: &[&str]) -> Result<String, CiStatusError> {
    command_timeout(program, args, "30")
}

fn command_timeout(program: &str, args: &[&str], seconds: &str) -> Result<String, CiStatusError> {
    let result = Command::new("timeout")
        .args([seconds, program])
        .args(args)
        .output()
        .map_err(|err| CiStatusError::ToolFailure {
            message: format!("{program}: {err}"),
        })?;
    if !result.status.success() {
        return Err(CiStatusError::ToolFailure {
            message: format!(
                "{program}: {}",
                String::from_utf8_lossy(&result.stderr).trim()
            ),
        });
    }
    Ok(String::from_utf8_lossy(&result.stdout).trim().to_owned())
}

fn api<T: serde::de::DeserializeOwned>(endpoint: &str) -> Result<T, CiStatusError> {
    serde_json::from_str(&command("gh", &["api", endpoint])?).map_err(|err| {
        CiStatusError::ToolFailure {
            message: format!("GitHub response: {err}"),
        }
    })
}

fn repository() -> Result<String, CiStatusError> {
    command(
        "gh",
        &[
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "--jq",
            ".nameWithOwner",
        ],
    )
}

fn head_run(repo: &str, head: &str) -> Result<Option<Run>, CiStatusError> {
    let runs: Runs = api(&format!(
        "repos/{repo}/actions/workflows/ci.yml/runs?branch=nativeapp&event=push&head_sha={head}&per_page=100"
    ))?;
    // Retries and re-runs must never allow an older green run to mask the latest failure.
    Ok(runs
        .workflow_runs
        .into_iter()
        .filter(|run| run.head_sha == head)
        .max_by_key(|run| run.id))
}

fn run_jobs(repo: &str, run: Option<&Run>) -> Result<Vec<Job>, CiStatusError> {
    let mut jobs = Vec::new();
    if let Some(run) = run {
        for page in 1.. {
            let result: Jobs = api(&format!(
                "repos/{repo}/actions/runs/{}/jobs?filter=latest&per_page=100&page={page}",
                run.id
            ))?;
            let count = result.jobs.len();
            jobs.extend(result.jobs);
            if count < 100 {
                break;
            }
        }
    }
    Ok(jobs)
}

fn pushed(head: &str) -> Result<(), CiStatusError> {
    let remote = command("git", &["ls-remote", "origin", "refs/heads/nativeapp"])?;
    if remote.split_whitespace().next() != Some(head) {
        return Err(CiStatusError::NotPushed);
    }
    Ok(())
}

fn probe(head: &str) -> Result<CiRun, CiStatusError> {
    pushed(head)?;
    let repo = repository()?;
    let run = head_run(&repo, head)?;
    let jobs = run_jobs(&repo, run.as_ref())?;
    evaluate(head, head, run.as_ref(), &jobs)
}

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(2).collect();
    let wait = match args.as_slice() {
        [] => 0,
        [flag, secs] if flag == "--wait" => secs.parse::<u64>()?,
        _ => return Err("usage: xtask ci-status [--wait SECS]".into()),
    };
    let result = status(wait);
    match result {
        Ok(run) => println!("{}", serde_json::to_string(&run)?),
        Err(err) => {
            println!("{}", serde_json::to_string(&err)?);
            std::process::exit(1);
        }
    }
    Ok(())
}

fn status(wait: u64) -> Result<CiRun, CiStatusError> {
    let head = command("git", &["rev-parse", "HEAD"])?;
    let start = Instant::now();
    loop {
        let result = probe(&head);
        if !matches!(
            result,
            Err(CiStatusError::NoRunForHead | CiStatusError::StillRunning { .. })
        ) {
            return result;
        }
        let remaining = Duration::from_secs(wait).saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return result;
        }
        eprintln!(
            "{}",
            serde_json::to_string(&result.unwrap_err()).expect("serializable status")
        );
        std::thread::sleep(remaining.min(Duration::from_secs(15)));
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct PriorRun {
    head: String,
    run_id: u64,
    url: String,
    conclusion: Option<String>,
}

impl From<&Run> for PriorRun {
    fn from(run: &Run) -> Self {
        Self {
            head: run.head_sha.clone(),
            run_id: run.id,
            url: run.html_url.clone(),
            conclusion: run.conclusion.clone(),
        }
    }
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ObservationStatus {
    Pending,
    Success,
    Failed,
    Missing,
}

#[derive(Debug, Serialize)]
struct Observation {
    head: String,
    run_id: Option<u64>,
    url: Option<String>,
    status: ObservationStatus,
    prior: Option<PriorRun>,
    observed_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<CiStatusError>,
}

impl Observation {
    fn new(head: &str) -> Self {
        Self {
            head: head.into(),
            run_id: None,
            url: None,
            status: ObservationStatus::Failed,
            prior: None,
            observed_at: Utc::now(),
            error: None,
        }
    }
}

struct ObservationOptions {
    settle: Duration,
    wait: Duration,
    record: Option<PathBuf>,
}

impl ObservationOptions {
    fn parse(args: &[String]) -> Result<Self, Box<dyn std::error::Error>> {
        let mut options = Self {
            settle: Duration::from_secs(180),
            wait: Duration::from_secs(3000),
            record: None,
        };
        let mut args = args.iter();
        while let Some(flag) = args.next() {
            let value = args.next().ok_or("expected a value after option")?;
            match flag.as_str() {
                "--settle" => options.settle = Duration::from_secs(value.parse()?),
                "--wait" => options.wait = Duration::from_secs(value.parse()?),
                "--record" => options.record = Some(value.into()),
                _ => {
                    return Err(
                        "usage: xtask ci-observe [--settle SECS] [--wait SECS] [--record PATH]"
                            .into(),
                    );
                }
            }
        }
        Ok(options)
    }
}

struct ObservationSnapshot {
    run: Option<Run>,
    jobs: Vec<Job>,
    prior: Option<Run>,
}

impl ObservationSnapshot {
    fn evaluate(&self, head: &str) -> Result<CiRun, CiStatusError> {
        // Observation stops on any completed non-success immediately, even if
        // another job is pending. The blocking status evaluator permits skipped
        // ancillary jobs; observation also surfaces those non-success outcomes.
        if let Some(run) = self.run.as_ref().filter(|run| run.head_sha == head) {
            if let Some(job) = self.jobs.iter().find(|job| {
                job.status == "completed" && job.conclusion.as_deref() != Some("success")
            }) {
                return Err(CiStatusError::Failed {
                    run_id: run.id,
                    job: job.name.clone(),
                });
            }
            if run.status == "completed" && run.conclusion.as_deref() != Some("success") {
                return Err(CiStatusError::Failed {
                    run_id: run.id,
                    job: "workflow".into(),
                });
            }
        }
        evaluate(head, head, self.run.as_ref(), &self.jobs)
    }
}

fn prior_run(repo: &str, head: &str) -> Result<Option<Run>, CiStatusError> {
    // GitHub lists newest runs first. Paginate past repeated runs of this head
    // rather than treating a full first page as evidence there is no prior push.
    for page in 1.. {
        let runs: Runs = api(&format!(
            "repos/{repo}/actions/workflows/ci.yml/runs?branch=nativeapp&event=push&per_page=100&page={page}"
        ))?;
        let count = runs.workflow_runs.len();
        if let Some(run) = runs
            .workflow_runs
            .into_iter()
            .filter(|run| run.head_sha != head)
            .max_by_key(|run| run.id)
        {
            return Ok(Some(run));
        }
        if count < 100 {
            return Ok(None);
        }
    }
    unreachable!()
}

fn observation_probe(repo: &str, head: &str) -> Result<ObservationSnapshot, CiStatusError> {
    pushed(head)?;
    let run = head_run(repo, head)?;
    let jobs = run_jobs(repo, run.as_ref())?;
    let mut snapshot = ObservationSnapshot {
        run,
        jobs,
        prior: None,
    };
    if matches!(
        snapshot.evaluate(head),
        Err(CiStatusError::StillRunning { .. })
    ) {
        snapshot.prior = prior_run(repo, head)?;
    }
    Ok(snapshot)
}

struct Observer {
    settle: Duration,
    wait: Duration,
    red_since: Option<Duration>,
    prior: Option<PriorRun>,
}

impl Observer {
    fn new(options: &ObservationOptions) -> Self {
        Self {
            settle: options.settle,
            wait: options.wait,
            red_since: None,
            prior: None,
        }
    }

    /// Returns a final observation or the bounded delay before the next probe.
    fn step(
        &mut self,
        head: &str,
        snapshot: Result<ObservationSnapshot, CiStatusError>,
        elapsed: Duration,
    ) -> (Observation, Option<Duration>) {
        let mut observation = Observation::new(head);
        observation.prior = self.prior.clone();
        let result = match snapshot {
            Ok(snapshot) => {
                if let Some(run) = snapshot.run.as_ref().filter(|run| run.head_sha == head) {
                    observation.run_id = Some(run.id);
                    observation.url = Some(run.html_url.clone());
                }
                if let Some(prior) = snapshot.prior.as_ref() {
                    self.prior = Some(prior.into());
                    observation.prior = self.prior.clone();
                    if prior.status == "completed" && prior.conclusion.as_deref() != Some("success")
                    {
                        self.red_since.get_or_insert(elapsed);
                    }
                }
                snapshot.evaluate(head)
            }
            Err(error) => Err(error),
        };
        let error = match result {
            Ok(_) => {
                observation.status = ObservationStatus::Success;
                return (observation, None);
            }
            Err(error) => error,
        };
        let deadline = match &error {
            CiStatusError::NoRunForHead => {
                observation.status = ObservationStatus::Missing;
                Some(
                    self.red_since
                        .map_or(self.settle, |start| start.saturating_add(self.wait)),
                )
            }
            CiStatusError::StillRunning { .. } => {
                observation.status = ObservationStatus::Pending;
                self.red_since.map(|start| start.saturating_add(self.wait))
            }
            _ => {
                observation.error = Some(error);
                return (observation, None);
            }
        };
        // A pending run with no failed prior run is an observation, not a pass.
        let Some(deadline) = deadline else {
            return (observation, None);
        };
        let remaining = deadline.saturating_sub(elapsed);
        observation.error = Some(error);
        (
            observation,
            (!remaining.is_zero()).then_some(remaining.min(Duration::from_secs(15))),
        )
    }
}

fn observe(head: &str, repo: &str, options: &ObservationOptions) -> Observation {
    let mut observer = Observer::new(options);
    let start = Instant::now();
    loop {
        let snapshot = observation_probe(repo, head);
        let (observation, delay) = observer.step(head, snapshot, start.elapsed());
        let Some(delay) = delay else {
            return observation;
        };
        eprintln!(
            "{}",
            serde_json::to_string(&observation).expect("serializable observation")
        );
        std::thread::sleep(delay);
    }
}

fn prepare_observation(head: &mut String) -> Result<String, CiStatusError> {
    *head = command("git", &["rev-parse", "HEAD"])?;
    if command("git", &["branch", "--show-current"])? != "nativeapp" {
        return Err(CiStatusError::WrongBranch {
            expected: "nativeapp".into(),
        });
    }
    if !command(
        "git",
        &["status", "--porcelain", "--untracked-files=normal"],
    )?
    .is_empty()
    {
        return Err(CiStatusError::DirtyTree);
    }
    command_timeout("git", &["push", "origin", "HEAD:nativeapp"], "120")?;
    repository()
}

fn append_record(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")
}

pub fn observe_main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(2).collect();
    let options = match ObservationOptions::parse(&args) {
        Ok(options) => options,
        Err(error) => {
            let mut observation = Observation::new("");
            observation.error = Some(CiStatusError::ToolFailure {
                message: error.to_string(),
            });
            println!("{}", serde_json::to_string(&observation)?);
            std::process::exit(1);
        }
    };
    let mut head = String::new();
    let mut observation = match prepare_observation(&mut head) {
        Ok(repo) => observe(&head, &repo, &options),
        Err(error) => {
            let mut observation = Observation::new(&head);
            observation.error = Some(error);
            observation
        }
    };
    let line = serde_json::to_string(&observation)?;
    if let Some(path) = &options.record
        && let Err(error) = append_record(path, &line)
    {
        observation.status = ObservationStatus::Failed;
        observation.error = Some(CiStatusError::ToolFailure {
            message: format!("record {}: {error}", path.display()),
        });
    }
    println!("{}", serde_json::to_string(&observation)?);
    if observation.error.is_some() {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Run, Vec<Job>) {
        (
            Run {
                id: 42,
                html_url: "https://github.com/owner/repo/actions/runs/42".into(),
                head_sha: "head".into(),
                status: "completed".into(),
                conclusion: Some("success".into()),
            },
            vec![Job {
                name: "ios".into(),
                status: "completed".into(),
                conclusion: Some("success".into()),
                started_at: Some("2026-09-05T00:00:00Z".parse().unwrap()),
                completed_at: Some("2026-09-05T00:02:00Z".parse().unwrap()),
                steps: vec![Step {
                    name: "Run iOS verification".into(),
                    conclusion: Some("success".into()),
                }],
            }],
        )
    }

    #[test]
    fn ci_status_requires_pushed_exact_head() {
        let (run, jobs) = fixture();
        assert_eq!(
            evaluate("head", "old", Some(&run), &jobs),
            Err(CiStatusError::NotPushed)
        );
        assert_eq!(
            evaluate("new", "new", Some(&run), &jobs),
            Err(CiStatusError::NoRunForHead)
        );
        assert_eq!(
            evaluate("head", "head", None, &jobs),
            Err(CiStatusError::NoRunForHead)
        );
    }

    #[test]
    fn ci_status_reports_running_and_absent_job() {
        let (mut run, _) = fixture();
        run.status = "in_progress".into();
        assert_eq!(
            evaluate("head", "head", Some(&run), &[]),
            Err(CiStatusError::StillRunning { run_id: 42 })
        );
        run.status = "completed".into();
        assert_eq!(
            evaluate("head", "head", Some(&run), &[]),
            Err(CiStatusError::JobAbsent { run_id: 42 })
        );
    }

    #[test]
    fn ci_status_rejects_failed_skipped_or_unexecuted_ios() {
        for conclusion in ["failure", "cancelled", "skipped", "timed_out"] {
            let (run, mut jobs) = fixture();
            jobs[0].conclusion = Some(conclusion.into());
            assert!(
                matches!(evaluate("head", "head", Some(&run), &jobs), Err(CiStatusError::Failed { job, .. }) if job == "ios")
            );
        }
        let (run, mut jobs) = fixture();
        jobs[0].steps.clear();
        assert!(matches!(
            evaluate("head", "head", Some(&run), &jobs),
            Err(CiStatusError::Failed { .. })
        ));
        let (_, skipped) = fixture();
        jobs[0].steps = skipped[0].steps.clone();
        jobs[0].steps[0].conclusion = Some("skipped".into());
        assert!(matches!(
            evaluate("head", "head", Some(&run), &jobs),
            Err(CiStatusError::Failed { .. })
        ));
    }

    #[test]
    fn ci_status_requires_whole_workflow_and_reports_job_failure_early() {
        let (mut run, mut jobs) = fixture();
        run.status = "in_progress".into();
        let mut failed = jobs[0].clone();
        failed.name = "Test (ubuntu-latest)".into();
        failed.conclusion = Some("failure".into());
        jobs.push(failed);
        assert_eq!(
            evaluate("head", "head", Some(&run), &jobs),
            Err(CiStatusError::Failed {
                run_id: 42,
                job: "Test (ubuntu-latest)".into()
            })
        );
        jobs.pop();
        run.status = "completed".into();
        run.conclusion = Some("cancelled".into());
        assert!(matches!(
            evaluate("head", "head", Some(&run), &jobs),
            Err(CiStatusError::Failed { .. })
        ));
    }

    #[test]
    fn ci_status_green_has_url_head_and_measured_duration() {
        let (run, jobs) = fixture();
        assert_eq!(
            evaluate("head", "head", Some(&run), &jobs).unwrap(),
            CiRun {
                run_id: 42,
                url: run.html_url,
                head: "head".into(),
                ios_job_duration_secs: 120
            }
        );
        assert_eq!(
            serde_json::to_value(CiStatusError::NotPushed).unwrap(),
            serde_json::json!({"error":"NotPushed"})
        );
    }

    fn observer(settle: u64, wait: u64) -> Observer {
        Observer::new(&ObservationOptions {
            settle: Duration::from_secs(settle),
            wait: Duration::from_secs(wait),
            record: None,
        })
    }

    fn pending(prior_conclusion: Option<&str>) -> ObservationSnapshot {
        let (mut run, _) = fixture();
        run.status = "in_progress".into();
        run.conclusion = None;
        let prior = prior_conclusion.map(|conclusion| {
            let (mut prior, _) = fixture();
            prior.id = 41;
            prior.head_sha = "previous".into();
            prior.html_url = "https://github.com/owner/repo/actions/runs/41".into();
            prior.conclusion = Some(conclusion.into());
            prior
        });
        ObservationSnapshot {
            run: Some(run),
            jobs: vec![],
            prior,
        }
    }

    #[test]
    fn ci_observe_pending_with_green_absent_or_running_prior_returns_pending() {
        for prior in [Some("success"), None, Some("running")] {
            let mut snapshot = pending(prior);
            if prior == Some("running") {
                let prior = snapshot.prior.as_mut().unwrap();
                prior.status = "in_progress".into();
                prior.conclusion = None;
            }
            let (result, delay) = observer(180, 3000).step("head", Ok(snapshot), Duration::ZERO);
            assert_eq!(result.status, ObservationStatus::Pending);
            assert!(result.error.is_none());
            assert!(delay.is_none());
            assert_eq!(result.run_id, Some(42));
            assert_eq!(result.prior.is_some(), prior.is_some());
        }
    }

    #[test]
    fn ci_observe_pending_with_red_prior_waits_until_success_or_failure() {
        for success in [true, false] {
            let mut observer = observer(180, 30);
            let (result, delay) =
                observer.step("head", Ok(pending(Some("failure"))), Duration::from_secs(5));
            assert_eq!(result.status, ObservationStatus::Pending);
            assert_eq!(delay, Some(Duration::from_secs(15)));
            let (mut run, jobs) = fixture();
            if !success {
                run.conclusion = Some("failure".into());
            }
            let snapshot = ObservationSnapshot {
                run: Some(run),
                jobs,
                prior: None,
            };
            let (result, delay) = observer.step("head", Ok(snapshot), Duration::from_secs(20));
            assert_eq!(
                result.status,
                if success {
                    ObservationStatus::Success
                } else {
                    ObservationStatus::Failed
                }
            );
            assert_eq!(result.error.is_none(), success);
            assert!(delay.is_none());
            assert_eq!(result.prior.unwrap().head, "previous");
        }
    }

    #[test]
    fn ci_observe_red_prior_deadline_retains_both_records_and_never_passes_pending() {
        let mut observer = observer(180, 30);
        observer.step(
            "head",
            Ok(pending(Some("cancelled"))),
            Duration::from_secs(5),
        );
        let (_, delay) = observer.step(
            "head",
            Ok(pending(Some("cancelled"))),
            Duration::from_secs(34),
        );
        assert_eq!(delay, Some(Duration::from_secs(1)));
        let (result, delay) = observer.step(
            "head",
            Ok(pending(Some("cancelled"))),
            Duration::from_secs(35),
        );
        assert_eq!(result.status, ObservationStatus::Pending);
        assert_eq!(
            result.error,
            Some(CiStatusError::StillRunning { run_id: 42 })
        );
        assert!(delay.is_none());
        assert_eq!(result.run_id, Some(42));
        let prior = result.prior.unwrap();
        assert_eq!(prior.run_id, 41);
        assert_eq!(prior.conclusion.as_deref(), Some("cancelled"));
    }

    #[test]
    fn ci_observe_success_uses_exact_head_ios_step_and_duration_rules() {
        for valid in [true, false] {
            let (run, mut jobs) = fixture();
            if !valid {
                jobs[0].steps.clear();
            }
            let snapshot = ObservationSnapshot {
                run: Some(run),
                jobs,
                prior: None,
            };
            let (result, delay) = observer(0, 0).step("head", Ok(snapshot), Duration::ZERO);
            assert_eq!(
                result.status,
                if valid {
                    ObservationStatus::Success
                } else {
                    ObservationStatus::Failed
                }
            );
            assert_eq!(result.error.is_none(), valid);
            assert!(delay.is_none());
        }
    }

    #[test]
    fn ci_observe_fails_completed_non_success_before_other_jobs_finish() {
        for conclusion in ["failure", "cancelled", "timed_out", "skipped"] {
            for workflow in [true, false] {
                let mut snapshot = pending(None);
                if workflow {
                    let run = snapshot.run.as_mut().unwrap();
                    run.status = "completed".into();
                    run.conclusion = Some(conclusion.into());
                } else {
                    let (_, mut jobs) = fixture();
                    jobs[0].name = "linux".into();
                    jobs[0].conclusion = Some(conclusion.into());
                    snapshot.jobs = jobs;
                }
                let (result, delay) =
                    observer(180, 3000).step("head", Ok(snapshot), Duration::ZERO);
                assert_eq!(result.status, ObservationStatus::Failed);
                assert!(matches!(result.error, Some(CiStatusError::Failed { .. })));
                assert!(delay.is_none());
            }
        }
    }

    #[test]
    fn ci_observe_missing_settles_then_fails_or_accepts_the_exact_head() {
        for appears in [true, false] {
            let mut observer = observer(10, 30);
            let (mut wrong_head, jobs) = fixture();
            wrong_head.head_sha = "different".into();
            let wrong = ObservationSnapshot {
                run: Some(wrong_head),
                jobs,
                prior: None,
            };
            let (result, delay) = observer.step("head", Ok(wrong), Duration::ZERO);
            assert_eq!(result.status, ObservationStatus::Missing);
            assert_eq!(result.run_id, None);
            assert_eq!(delay, Some(Duration::from_secs(10)));
            let snapshot = if appears {
                let (run, jobs) = fixture();
                ObservationSnapshot {
                    run: Some(run),
                    jobs,
                    prior: None,
                }
            } else {
                ObservationSnapshot {
                    run: None,
                    jobs: vec![],
                    prior: None,
                }
            };
            let (result, delay) = observer.step("head", Ok(snapshot), Duration::from_secs(10));
            assert!(delay.is_none());
            if appears {
                assert_eq!(result.status, ObservationStatus::Success);
                assert!(result.error.is_none());
            } else {
                assert_eq!(result.status, ObservationStatus::Missing);
                assert_eq!(result.error, Some(CiStatusError::NoRunForHead));
            }
        }
    }

    #[test]
    fn ci_observe_options_default_and_override() {
        let options = ObservationOptions::parse(&[]).unwrap();
        assert_eq!(options.settle.as_secs(), 180);
        assert_eq!(options.wait.as_secs(), 3000);
        assert!(options.record.is_none());
        let args = [
            "--wait",
            "0",
            "--record",
            "results/log.jsonl",
            "--settle",
            "0",
        ]
        .map(str::to_owned);
        let options = ObservationOptions::parse(&args).unwrap();
        assert_eq!(options.settle, Duration::ZERO);
        assert_eq!(options.wait, Duration::ZERO);
        assert_eq!(options.record, Some(PathBuf::from("results/log.jsonl")));
        for args in [vec!["--wait"], vec!["--wait", "no"], vec!["--other", "0"]] {
            assert!(
                ObservationOptions::parse(&args.into_iter().map(str::to_owned).collect::<Vec<_>>())
                    .is_err()
            );
        }
    }
}
