//! Read the retained Claude SDK rows from an existing daemon, through the
//! same subscription boundary as a chat. No input is sent to the agent.
//! Usage: timeout 15 target/debug/examples/sdk_rows CONFIG AGENT

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use amux::claude_sdk_io::{CLAUDE_SDK_V1, decode_claude_sdk_v1_output};
use amux::{AgentIdentifier, Config, Server, SubscribeSessionEvent, SubscribeSessionRequest};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        bail!("usage: sdk_rows CONFIG AGENT");
    }
    let path = PathBuf::from(&args[0]);
    let mut config: Config = serde_yaml::from_slice(&std::fs::read(&path)?)?;
    config.path = Some(path);
    let agent = args[1].to_str().context("agent name must be UTF-8")?;
    tokio::time::timeout(Duration::from_secs(10), async {
        let client = Server::builder().config(config).daemon().open().await?;
        let mut stream = client
            .subscribe_session(SubscribeSessionRequest {
                agent: AgentIdentifier::Name(agent.to_owned()),
                io_protocol: CLAUDE_SDK_V1.to_owned(),
                args: None,
            })
            .await?;
        let mut stdout = std::io::stdout().lock();
        let mut expected = 0;
        loop {
            match stream.recv().await? {
                SubscribeSessionEvent::Output { payload } => {
                    let row = decode_claude_sdk_v1_output(&payload)?;
                    if row.seq_id != expected {
                        bail!(
                            "incomplete retained rows: expected {}, got {}",
                            expected,
                            row.seq_id
                        );
                    }
                    expected += 1;
                    let payload: Value = serde_json::from_slice(&row.payload)?;
                    writeln!(stdout, "{}", json!({"seq": row.seq_id, "payload": payload}))?;
                }
                SubscribeSessionEvent::ReplayComplete { .. } => return Ok(()),
                SubscribeSessionEvent::Closed { reason } => bail!("replay closed: {reason}"),
                SubscribeSessionEvent::Opened => {}
            }
        }
    })
    .await
    .context("timed out reading retained SDK rows")?
}
