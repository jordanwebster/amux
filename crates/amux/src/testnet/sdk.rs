//! A host-side stream-JSON peer for the real Claude SDK session and backend.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use claude::sdk::{QueryOptions, Session};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, duplex};
use uuid::Uuid;

/// The provider's initialization reply and the text it answers each prompt with.
/// Unsupported controls receive an error, never an invented acknowledgement.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    pub initialization: claude::sdk::InitializationResult,
    pub reply: String,
}

#[derive(Clone)]
pub struct Provider {
    script: Script,
    inputs: Arc<Mutex<HashMap<Uuid, Vec<Value>>>>,
}

impl Provider {
    pub fn new(script: Script) -> Self {
        Self {
            script,
            inputs: Arc::default(),
        }
    }

    /// Raw stdin received by this provider, including SDK control envelopes.
    /// Absence means this provider never opened that agent's session.
    pub fn observed(&self, agent: Uuid) -> Option<Vec<Value>> {
        self.inputs
            .lock()
            .expect("SDK observations poisoned")
            .get(&agent)
            .cloned()
    }

    pub(crate) async fn open(
        &self,
        agent: Uuid,
        options: QueryOptions,
    ) -> Result<Session, claude::sdk::Error> {
        let (sdk_stdin, provider_stdin) = duplex(65536);
        let (provider_stdout, sdk_stdout) = duplex(65536);
        self.inputs
            .lock()
            .expect("SDK observations poisoned")
            .insert(agent, Vec::new());
        let provider = self.clone();
        let model = options
            .model
            .clone()
            .unwrap_or_else(|| "provider-default".into());
        tokio::spawn(async move {
            if let Err(error) = provider
                .serve(agent, model, provider_stdin, provider_stdout)
                .await
            {
                tracing::debug!(%agent, %error, "scripted SDK transport closed");
            }
        });
        claude::sdk::from_io(BufReader::new(sdk_stdout), sdk_stdin, options).await
    }

    async fn serve(
        &self,
        agent: Uuid,
        mut model: String,
        stdin: DuplexStream,
        mut stdout: DuplexStream,
    ) -> anyhow::Result<()> {
        let mut lines = BufReader::new(stdin).lines();
        while let Some(line) = lines.next_line().await? {
            let input: Value = serde_json::from_str(&line)?;
            self.inputs
                .lock()
                .expect("SDK observations poisoned")
                .get_mut(&agent)
                .expect("registered SDK session")
                .push(input.clone());
            match input["type"].as_str() {
                Some("control_request") => {
                    let response = match input["request"]["subtype"].as_str() {
                        Some("initialize") => {
                            Some(serde_json::to_value(&self.script.initialization)?)
                        }
                        Some("set_model")
                            if input["request"]["model"].is_string()
                                && self.script.initialization.models.iter().any(|entry| {
                                    Some(entry.value.as_str()) == input["request"]["model"].as_str()
                                }) =>
                        {
                            model = input["request"]["model"].as_str().unwrap().to_owned();
                            Some(json!({}))
                        }
                        _ => None,
                    };
                    let response = match response {
                        Some(response) => {
                            json!({"subtype":"success", "request_id":input["request_id"], "response":response})
                        }
                        None => {
                            json!({"subtype":"error", "request_id":input["request_id"], "error":"scripted SDK does not support this control"})
                        }
                    };
                    write(
                        &mut stdout,
                        json!({"type":"control_response", "response":response}),
                    )
                    .await?;
                }
                Some("user") => {
                    write(&mut stdout, json!({"type":"assistant", "uuid":Uuid::new_v4(),
                        "session_id":agent, "parent_tool_use_id":null,
                        "message":{"id":Uuid::new_v4().to_string(), "type":"message", "role":"assistant",
                        "model":model, "content":[{"type":"text", "text":self.script.reply}],
                        "usage":{"input_tokens":1, "output_tokens":1}}})).await?;
                    write(
                        &mut stdout,
                        json!({"type":"result", "subtype":"success", "uuid":Uuid::new_v4(),
                        "session_id":agent, "duration_ms":1, "duration_api_ms":1, "is_error":false,
                        "num_turns":1, "stop_reason":"end_turn", "total_cost_usd":0.0,
                        "result":self.script.reply, "usage":{"input_tokens":1,"output_tokens":1},
                        "permission_denials":[], "modelUsage":{}}),
                    )
                    .await?;
                }
                other => anyhow::bail!("unsupported scripted SDK input: {other:?}"),
            }
        }
        Ok(())
    }
}

async fn write(output: &mut DuplexStream, value: Value) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(&value)?;
    bytes.push(b'\n');
    output.write_all(&bytes).await?;
    Ok(())
}
