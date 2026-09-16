//! Daemon-owned standing folds for local structured agents.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use fold::{AgentFold, Baseline, Input};
use model::{AgentKind, ClaudeDriver, StructuredProtocol, Summary};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{BroadcastRead, MultiplexStructuredReader, SequencedReplayQuery, StructuredLogSource};

const SUMMARY_COALESCE: Duration = Duration::from_millis(100);
const TICK_CADENCE: Duration = Duration::from_secs(1);
const PROGRESS_CADENCE: Duration = Duration::from_secs(2);
const PROGRESS_ROWS: u64 = 200;
const STALE_AFTER: Duration = Duration::from_secs(10);
const STALE_ROWS: u64 = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SummaryCut {
    pub(crate) through: u64,
    pub(crate) producer_version: u32,
    pub(crate) observed_at: DateTime<Utc>,
    pub(crate) stale: bool,
    pub(crate) summary: Summary,
}

pub(crate) struct SummarizerPublication {
    pub(crate) agent_id: Uuid,
    pub(crate) cut: SummaryCut,
    pub(crate) publish_progress: bool,
    pub(crate) acknowledged: Option<oneshot::Sender<()>>,
}

#[derive(Clone)]
struct SharedSummary(Arc<Mutex<SummaryCut>>);

impl SharedSummary {
    fn get(&self) -> SummaryCut {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    fn replace(&self, cut: SummaryCut) {
        *self.0.lock().unwrap_or_else(|poison| poison.into_inner()) = cut;
    }

    fn set_stale(&self, stale: bool) -> Option<SummaryCut> {
        let mut cut = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        if cut.stale == stale {
            return None;
        }
        cut.stale = stale;
        Some(cut.clone())
    }
}

enum Control {
    ProcessExited {
        exit_code: Option<i32>,
        acknowledged: oneshot::Sender<()>,
    },
}

pub(crate) struct SummarizerHandle {
    shared: SharedSummary,
    control_tx: mpsc::UnboundedSender<Control>,
    activate: Arc<tokio::sync::Notify>,
    cancel: CancellationToken,
}

impl SummarizerHandle {
    pub(crate) async fn attach(
        agent_id: Uuid,
        protocol: StructuredProtocol,
        source: StructuredLogSource,
        publisher: mpsc::UnboundedSender<SummarizerPublication>,
    ) -> Option<Self> {
        let (reader, facts) = source
            .subscribe_with_query(Some(SequencedReplayQuery::TailCount {
                count: 0,
                tail_bound: None,
            }))
            .await?;
        let fold = initial_fold(protocol, facts.through);
        let now = Utc::now();
        let shared = SharedSummary(Arc::new(Mutex::new(SummaryCut {
            through: facts.through,
            producer_version: fold.tip_version(),
            observed_at: now,
            stale: false,
            summary: fold.summary(),
        })));
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let activate = Arc::new(tokio::sync::Notify::new());
        let cancel = CancellationToken::new();
        tokio::spawn(supervise(
            agent_id,
            protocol,
            source,
            reader,
            facts.through,
            fold,
            shared.clone(),
            publisher,
            control_rx,
            activate.clone(),
            cancel.clone(),
        ));
        Some(Self {
            shared,
            control_tx,
            activate,
            cancel,
        })
    }

    pub(crate) fn activate(&self) {
        self.activate.notify_one();
    }

    pub(crate) fn snapshot(&self) -> SummaryCut {
        self.shared.get()
    }

    pub(crate) fn process_exited(&self, exit_code: Option<i32>) -> Option<oneshot::Receiver<()>> {
        let (tx, rx) = oneshot::channel();
        if self
            .control_tx
            .send(Control::ProcessExited {
                exit_code,
                acknowledged: tx,
            })
            .is_ok()
        {
            Some(rx)
        } else {
            None
        }
    }
}

impl Drop for SummarizerHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub(crate) fn summarizer_protocol(kind: AgentKind) -> Option<StructuredProtocol> {
    match kind {
        AgentKind::Claude {
            driver: ClaudeDriver::Pty,
        } => Some(StructuredProtocol::ClaudePtyTranscript),
        AgentKind::Claude {
            driver: ClaudeDriver::Sdk,
        } => Some(StructuredProtocol::ClaudeSdk),
        AgentKind::Codex => Some(StructuredProtocol::Codex),
        AgentKind::TestAgent => None,
    }
}

fn initial_fold(protocol: StructuredProtocol, through: u64) -> AgentFold {
    let mut fold = AgentFold::for_protocol(protocol);
    let baseline = if through == 0 {
        Baseline::Start
    } else {
        Baseline::Truncated {
            from: through.saturating_add(1),
        }
    };
    fold.begin(1, baseline);
    fold
}

#[allow(clippy::too_many_arguments)]
async fn supervise(
    agent_id: Uuid,
    protocol: StructuredProtocol,
    source: StructuredLogSource,
    initial_reader: MultiplexStructuredReader,
    initial_through: u64,
    first_fold: AgentFold,
    shared: SharedSummary,
    publisher: mpsc::UnboundedSender<SummarizerPublication>,
    mut control_rx: mpsc::UnboundedReceiver<Control>,
    activate: Arc<tokio::sync::Notify>,
    cancel: CancellationToken,
) {
    tokio::select! {
        () = activate.notified() => {}
        () = cancel.cancelled() => return,
    }
    let _ = publisher.send(SummarizerPublication {
        agent_id,
        cut: shared.get(),
        publish_progress: false,
        acknowledged: None,
    });

    let (mut worker_control_tx, worker_control_rx) = mpsc::unbounded_channel();
    let mut worker = tokio::spawn(run_worker(
        agent_id,
        initial_reader,
        initial_through,
        first_fold,
        shared.clone(),
        publisher.clone(),
        worker_control_rx,
        cancel.clone(),
    ));
    let mut health =
        tokio::time::interval_at(tokio::time::Instant::now() + TICK_CADENCE, TICK_CADENCE);
    health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                worker.abort();
                return;
            }
            control = control_rx.recv() => {
                let Some(control) = control else {
                    worker.abort();
                    return;
                };
                let _ = worker_control_tx.send(control);
            }
            _ = health.tick() => {
                let snapshot = shared.get();
                let pending = source.pending_after(snapshot.through).await;
                let too_old = pending.oldest_published_at_unix_ms.is_some_and(|published| {
                    Utc::now().timestamp_millis().saturating_sub(published)
                        > STALE_AFTER.as_millis() as i64
                });
                let stale = worker.is_finished() || pending.rows > STALE_ROWS || too_old;
                if let Some(cut) = shared.set_stale(stale) {
                    let _ = publisher.send(SummarizerPublication {
                        agent_id,
                        cut,
                        publish_progress: false,
                        acknowledged: None,
                    });
                }
            }
            _ = &mut worker => {
                if let Some(cut) = shared.set_stale(true) {
                    let _ = publisher.send(SummarizerPublication {
                        agent_id,
                        cut,
                        publish_progress: false,
                        acknowledged: None,
                    });
                }
                let Some((reader, facts)) = source
                    .subscribe_with_query(Some(SequencedReplayQuery::TailCount {
                        count: 0,
                        tail_bound: None,
                    }))
                    .await
                else {
                    return;
                };
                let through = facts.through;
                let fold = initial_fold(protocol, through);
                shared.replace(SummaryCut {
                    through,
                    producer_version: fold.tip_version(),
                    observed_at: Utc::now(),
                    stale: true,
                    summary: fold.summary(),
                });
                let _ = publisher.send(SummarizerPublication {
                    agent_id,
                    cut: shared.get(),
                    publish_progress: false,
                    acknowledged: None,
                });
                let (next_tx, next_rx) = mpsc::unbounded_channel();
                worker_control_tx = next_tx;
                worker = tokio::spawn(run_worker(
                    agent_id,
                    reader,
                    through,
                    fold,
                    shared.clone(),
                    publisher.clone(),
                    next_rx,
                    cancel.clone(),
                ));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_worker(
    agent_id: Uuid,
    mut reader: MultiplexStructuredReader,
    replay_through: u64,
    mut fold: AgentFold,
    shared: SharedSummary,
    publisher: mpsc::UnboundedSender<SummarizerPublication>,
    mut control_rx: mpsc::UnboundedReceiver<Control>,
    cancel: CancellationToken,
) {
    let mut dirty = false;
    let mut rows_since_progress = 0u64;
    let mut observed_at = shared.get().observed_at;
    let now = tokio::time::Instant::now();
    let mut tick = tokio::time::interval_at(now + TICK_CADENCE, TICK_CADENCE);
    let mut progress = tokio::time::interval_at(now + PROGRESS_CADENCE, PROGRESS_CADENCE);
    let mut flush = tokio::time::interval_at(now + SUMMARY_COALESCE, SUMMARY_COALESCE);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    progress.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            control = control_rx.recv() => {
                let Some(Control::ProcessExited { exit_code, acknowledged }) = control else {
                    return;
                };
                let now = Utc::now();
                let changes = fold.apply_summary(Input::ProcessExited { exit_code, at: now });
                observed_at = now;
                replace_cut(&shared, &fold, changes.through, observed_at);
                let _ = publisher.send(SummarizerPublication {
                    agent_id,
                    cut: shared.get(),
                    publish_progress: false,
                    acknowledged: Some(acknowledged),
                });
                dirty = false;
            }
            event = reader.read_event() => {
                match event {
                    Some(BroadcastRead::ReplayItem(row) | BroadcastRead::LiveItem(row)) => {
                        let Some(published_at) = Utc.timestamp_millis_opt(row.published_at_unix_ms).single() else {
                            continue;
                        };
                        let activity_at = row.activity_at_unix_ms.and_then(|at| Utc.timestamp_millis_opt(at).single());
                        let Ok(payload) = serde_json::to_vec(&row.payload) else { continue; };
                        let changes = fold.apply_summary(Input::Row {
                            seq: row.seq,
                            published_at,
                            activity_at,
                            historical: row.historical,
                            payload: &payload,
                        });
                        observed_at = published_at;
                        dirty |= changes.changed;
                        rows_since_progress = rows_since_progress.saturating_add(1);
                        replace_cut(&shared, &fold, changes.through, observed_at);
                        if rows_since_progress >= PROGRESS_ROWS {
                            publish_cut(agent_id, &shared, &publisher, true);
                            rows_since_progress = 0;
                            dirty = false;
                        }
                    }
                    Some(BroadcastRead::ReplayComplete) => {
                        let now = Utc::now();
                        let changes = fold.apply_summary(Input::ReplayComplete { through: replay_through, at: now });
                        observed_at = now;
                        dirty |= changes.changed;
                        replace_cut(&shared, &fold, changes.through, observed_at);
                    }
                    Some(BroadcastRead::Lagged | BroadcastRead::Reset) | None => {
                        let now = Utc::now();
                        let changes = fold.apply_summary(Input::ObserverLost { at: now });
                        replace_cut(&shared, &fold, changes.through, now);
                        return;
                    }
                }
            }
            _ = tick.tick() => {
                let changes = fold.apply_summary(Input::Tick { now: Utc::now() });
                dirty |= changes.changed;
                replace_cut(&shared, &fold, changes.through, observed_at);
            }
            _ = flush.tick(), if dirty => {
                publish_cut(agent_id, &shared, &publisher, false);
                dirty = false;
            }
            _ = progress.tick() => {
                publish_cut(agent_id, &shared, &publisher, true);
                rows_since_progress = 0;
                dirty = false;
            }
        }
    }
}

fn replace_cut(shared: &SharedSummary, fold: &AgentFold, through: u64, observed_at: DateTime<Utc>) {
    let stale = shared.get().stale;
    shared.replace(SummaryCut {
        through,
        producer_version: fold.tip_version(),
        observed_at,
        stale,
        summary: fold.summary(),
    });
}

fn publish_cut(
    agent_id: Uuid,
    shared: &SharedSummary,
    publisher: &mpsc::UnboundedSender<SummarizerPublication>,
    publish_progress: bool,
) {
    let _ = publisher.send(SummarizerPublication {
        agent_id,
        cut: shared.get(),
        publish_progress,
        acknowledged: None,
    });
}

#[cfg(test)]
mod tests {
    use fold::{AgentFold, Baseline, Input};
    use model::{AgentPhase, StructuredProtocol, SummaryField};
    use serde_json::json;
    use tokio::time::{Duration, timeout};

    use super::*;
    use crate::agents::RingPolicy;

    async fn next_matching(
        rx: &mut mpsc::UnboundedReceiver<SummarizerPublication>,
        predicate: impl Fn(&SummarizerPublication) -> bool,
    ) -> SummarizerPublication {
        timeout(Duration::from_secs(8), async {
            loop {
                let publication = rx.recv().await.expect("summarizer remains connected");
                if predicate(&publication) {
                    return publication;
                }
            }
        })
        .await
        .expect("matching summarizer publication")
    }

    #[tokio::test]
    async fn daemon_summarizer_attached_before_start_observes_row_one() {
        let source = StructuredLogSource::new(8);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = SummarizerHandle::attach(
            Uuid::from_u128(1),
            StructuredProtocol::Codex,
            source.clone(),
            tx,
        )
        .await
        .unwrap();

        source
            .write(json!({"type":"turn/started","turn":{"id":"t1","status":"inProgress"}}))
            .await;
        handle.activate();

        let publication = next_matching(&mut rx, |event| event.cut.through == 1).await;
        assert_eq!(publication.cut.through, 1);
        assert!(
            !publication
                .cut
                .summary
                .unknown
                .contains(&SummaryField::Attention)
        );
    }

    #[tokio::test]
    async fn daemon_summarizer_provider_exit_publishes_exited() {
        let source = StructuredLogSource::new(8);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = SummarizerHandle::attach(
            Uuid::from_u128(2),
            StructuredProtocol::ClaudePtyTranscript,
            source,
            tx,
        )
        .await
        .unwrap();
        handle.activate();
        let acknowledged = handle.process_exited(Some(17)).unwrap();
        let mut publication = next_matching(&mut rx, |event| {
            matches!(
                event.cut.summary.phase,
                AgentPhase::Exited {
                    exit_code: Some(17)
                }
            )
        })
        .await;
        publication.acknowledged.take().unwrap().send(()).unwrap();
        acknowledged.await.unwrap();
    }

    #[tokio::test]
    async fn daemon_summarizer_resume_starts_at_unknown_sealed_cut() {
        let source = StructuredLogSource::resuming_with_policy(RingPolicy::test(8), 41);
        let (tx, _rx) = mpsc::unbounded_channel();
        let handle = SummarizerHandle::attach(
            Uuid::from_u128(3),
            StructuredProtocol::ClaudeSdk,
            source,
            tx,
        )
        .await
        .unwrap();
        let cut = handle.snapshot();
        assert_eq!(cut.through, 41);
        assert_eq!(
            cut.summary.unknown,
            vec![
                SummaryField::Attention,
                SummaryField::Phase,
                SummaryField::LastActivity,
                SummaryField::Todo,
                SummaryField::Context,
                SummaryField::Model,
                SummaryField::Outstanding,
            ]
        );
    }

    #[tokio::test]
    async fn daemon_summarizer_backpressure_stales_then_recovers_at_current_cut() {
        let source = StructuredLogSource::new(1);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = SummarizerHandle::attach(
            Uuid::from_u128(4),
            StructuredProtocol::Codex,
            source.clone(),
            tx,
        )
        .await
        .unwrap();
        for index in 0..700 {
            source
                .write(json!({"type":"turn/started","turn":{"id":format!("t{index}"),"status":"inProgress"}}))
                .await;
        }
        handle.activate();

        let stale = next_matching(&mut rx, |event| event.cut.stale).await;
        assert!(stale.cut.through <= 700);
        let recovered = next_matching(&mut rx, |event| {
            event.cut.through == 700 && !event.cut.stale
        })
        .await;
        assert_eq!(recovered.cut.summary.unknown.len(), 7);

        source
            .write(json!({"type":"turn/started","turn":{"id":"recovered","status":"inProgress"}}))
            .await;
        let live = next_matching(&mut rx, |event| event.cut.through == 701).await;
        assert!(!live.cut.stale);
        assert!(!live.cut.summary.unknown.contains(&SummaryField::Attention));
    }

    #[tokio::test]
    async fn daemon_summarizer_matches_client_fold_at_every_through() {
        let source = StructuredLogSource::new(16);
        let (tx, _rx) = mpsc::unbounded_channel();
        let handle = SummarizerHandle::attach(
            Uuid::from_u128(5),
            StructuredProtocol::Codex,
            source.clone(),
            tx,
        )
        .await
        .unwrap();
        handle.activate();
        let rows = include_str!("../../../codex-specs/fixtures/codex/turn_round_trip.rows.jsonl")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap());
        let mut client = AgentFold::for_protocol(StructuredProtocol::Codex);
        client.begin(1, Baseline::Start);
        for (index, row) in rows.enumerate() {
            source.write(row.clone()).await;
            let seq = index as u64 + 1;
            timeout(Duration::from_secs(2), async {
                while handle.snapshot().through != seq {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let at = handle.snapshot().observed_at;
            let payload = serde_json::to_vec(&row).unwrap();
            let expected = client.apply_summary(Input::Row {
                seq,
                published_at: at,
                activity_at: None,
                historical: false,
                payload: &payload,
            });
            let actual = handle.snapshot();
            assert_eq!(actual.through, expected.through);
            assert_eq!(actual.producer_version, client.tip_version());
            assert_eq!(actual.observed_at, at);
            assert!(!actual.stale);
            assert_eq!(actual.summary, expected.summary);
        }
    }
}
