//! Prompt publication is observed through the same live and replay log readers.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use tokio::io::{AsyncWrite, BufReader, DuplexStream, duplex};
use tokio::sync::oneshot;

use super::tests::{read_json_line, record, write_json_line};
use super::*;

struct PromptWriter {
    inner: DuplexStream,
    armed: Arc<AtomicBool>,
    fail: bool,
    release: Option<oneshot::Receiver<()>>,
}

impl AsyncWrite for PromptWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.fail && self.armed.load(Ordering::SeqCst) {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "prompt refused",
            )));
        }
        Pin::new(&mut self.inner).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        if self.armed.load(Ordering::SeqCst)
            && let Some(release) = &mut self.release
        {
            if Pin::new(release).poll(cx).is_pending() {
                return Poll::Pending;
            }
            self.release = None;
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[tokio::test]
async fn claude_sdk_accepted_prompts_precede_fast_replies_for_every_reader_and_reconnect() {
    tokio::time::timeout(Duration::from_secs(10), prompt_publication(false))
        .await
        .unwrap();
}

#[tokio::test]
async fn claude_sdk_failed_prompt_write_publishes_no_accepted_prompt() {
    tokio::time::timeout(Duration::from_secs(10), prompt_publication(true))
        .await
        .unwrap();
}

async fn prompt_publication(fail: bool) {
    let (sdk_stdin, server_stdin) = duplex(32768);
    let (mut stdout, sdk_stdout) = duplex(32768);
    let armed = Arc::new(AtomicBool::new(false));
    let (release_tx, release_rx) = oneshot::channel();
    let (replied_tx, replied_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut stdin = BufReader::new(server_stdin);
        let init = read_json_line(&mut stdin).await;
        write_json_line(
            &mut stdout,
            json!({"type":"control_response", "response":{
                "subtype":"success", "request_id":init["request_id"], "response":{
                    "commands":[], "agents":[], "models":[], "account":{},
                    "output_style":"default", "available_output_styles":[]
                }
            }}),
        )
        .await;
        if !fail {
            let prompt = read_json_line(&mut stdin).await;
            assert_eq!(prompt["message"]["content"][0]["text"], "Look at the image");
            assert_eq!(prompt["message"]["content"][1]["type"], "image");
            let reply: Value =
                include_str!("../../../tests/fixtures/rows/claude-sdk/text_turn.rows.jsonl")
                    .lines()
                    .map(|line| serde_json::from_str::<Value>(line).unwrap())
                    .find(|row| row["type"] == "assistant")
                    .unwrap();
            write_json_line(&mut stdout, reply).await;
            replied_tx.send(()).unwrap();
            let second = read_json_line(&mut stdin).await;
            assert_eq!(second["message"]["content"], "Look at the image");
        }
        let _ = finish_rx.await;
    });
    let session = claude::sdk::from_io(
        BufReader::new(sdk_stdout),
        PromptWriter {
            inner: sdk_stdin,
            armed: armed.clone(),
            fail,
            release: Some(release_rx),
        },
        QueryOptions {
            session_id: Some(Uuid::nil().to_string()),
            ..QueryOptions::default()
        },
    )
    .await
    .unwrap();
    let mut backend = ClaudeSdkBackend::with_session(record(Uuid::nil()), session);
    let Plane::Structured { log, input } = backend.plane(Protocol::ClaudeSdkV1).unwrap() else {
        panic!()
    };
    let mut first = log.subscribe().await.unwrap();
    let mut second = log.subscribe().await.unwrap();
    let (events, _rx) = mpsc::channel(8);
    let ingest = backend.start(&events).unwrap();
    for _ in 0..2 {
        first.read().await.unwrap();
        second.read().await.unwrap();
    }
    let id = Uuid::from_u128(1);
    let metadata = crate::agents::attachments::attachments_row(
        Some(id.as_bytes()),
        &[crate::ArtifactRef {
            id: amux_artifacts::id_of(b"image"),
            kind: crate::ArtifactKind::Image,
            name: "screen.png".into(),
            mime: "image/png".into(),
            size: 5,
        }],
    );
    log.write(metadata.clone()).await;
    assert_eq!(first.read().await.unwrap().payload, metadata);
    assert_eq!(second.read().await.unwrap().payload, metadata);
    armed.store(true, Ordering::SeqCst);
    let command = StructuredInputEvent::ClaudeSdk {
        input_id: id.as_bytes().to_vec(),
        input: ClaudeSdkV1Input::Prompt {
            text: "Look at the image".into(),
            image_blocks: vec![
                serde_json::from_value(json!({"type":"image", "source":{
                    "type":"base64", "media_type":"image/png", "data":"aW1hZ2U="
                }}))
                .unwrap(),
            ],
        },
    };
    if fail {
        input.send(command).await.unwrap();
        let receipt = first.read().await.unwrap().payload;
        assert_eq!(receipt["type"], "amux.claude_sdk.input_result");
        assert_ne!(receipt["outcome"], "ok");
        assert_eq!(second.read().await.unwrap().payload, receipt);
        let (mut replay, count) = log.subscribe_with_query(None).await.unwrap();
        for _ in 0..count {
            assert_ne!(replay.read().await.unwrap().payload["type"], "user");
        }
    } else {
        let before = log.current_seq().await;
        let (sent, ()) = tokio::join!(input.send(command), async {
            replied_rx.await.unwrap();
            // The provider has already replied while its prompt flush is held.
            // Give ingestion an opportunity to publish that row prematurely.
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
            assert_eq!(log.current_seq().await, before);
            release_tx.send(()).unwrap();
        });
        sent.unwrap();
        let accepted = first.read().await.unwrap().payload;
        assert_eq!(accepted["type"], "user");
        assert_eq!(accepted["uuid"], id.to_string());
        assert_eq!(accepted["input_id"], metadata["input_id"]);
        assert_eq!(accepted["message"]["content"][1]["type"], "image");
        assert_eq!(second.read().await.unwrap().payload, accepted);
        for expected in [
            "amux.claude_sdk.input_result",
            "assistant",
            "amux.claude_sdk.session_facts",
        ] {
            let row = first.read().await.unwrap().payload;
            assert_eq!(row["type"], expected);
            assert_eq!(second.read().await.unwrap().payload, row);
        }
        input
            .send(StructuredInputEvent::ClaudeSdk {
                input_id: Uuid::from_u128(2).as_bytes().to_vec(),
                input: ClaudeSdkV1Input::Prompt {
                    text: "Look at the image".into(),
                    image_blocks: vec![],
                },
            })
            .await
            .unwrap();
        let (mut replay, count) = log.subscribe_with_query(None).await.unwrap();
        let mut rows = Vec::new();
        for _ in 0..count {
            rows.push(replay.read().await.unwrap().payload);
        }
        let prompts: Vec<_> = rows.iter().filter(|row| row["type"] == "user").collect();
        assert_eq!(
            prompts.len(),
            2,
            "identical text is two separate accepted inputs"
        );
        assert_eq!(*prompts[0], accepted);
        assert_eq!(prompts[1]["uuid"], Uuid::from_u128(2).to_string());
        assert_eq!(prompts[1]["message"]["content"], "Look at the image");
        if let Some(path) = std::env::var_os("CLAUDE_SDK_PROMPT_EVIDENCE") {
            std::fs::write(
                path,
                rows.iter()
                    .map(|row| format!("{row}\n"))
                    .collect::<String>(),
            )
            .unwrap();
        }
    }
    let _ = finish_tx.send(());
    server.await.unwrap();
    backend.stop(StopPolicy::Interrupt).await;
    let _ = ingest.await;
}
