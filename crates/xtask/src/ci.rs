use std::process::Command;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "error")]
enum CiStatusError {
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
    let result = Command::new("timeout")
        .args(["30", program])
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

fn probe(head: &str) -> Result<CiRun, CiStatusError> {
    let remote = command("git", &["ls-remote", "origin", "refs/heads/nativeapp"])?;
    let remote = remote.split_whitespace().next().unwrap_or("");
    if head != remote {
        return Err(CiStatusError::NotPushed);
    }
    let repo = command(
        "gh",
        &[
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "--jq",
            ".nameWithOwner",
        ],
    )?;
    let runs: Runs = api(&format!(
        "repos/{repo}/actions/workflows/ci.yml/runs?branch=nativeapp&event=push&head_sha={head}&per_page=100"
    ))?;
    // Retries and re-runs must never allow an older green run to mask the latest failure.
    let run = runs
        .workflow_runs
        .iter()
        .filter(|run| run.head_sha == head)
        .max_by_key(|run| run.id);
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
    evaluate(head, remote, run, &jobs)
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
}
