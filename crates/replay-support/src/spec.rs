use std::collections::HashMap;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

#[derive(Debug, Deserialize)]
pub struct Spec {
    pub name: String,
    pub description: String,
    pub setup: Setup,
    pub steps: Vec<Step>,
    pub postconditions: Option<Postconditions>,
}

#[derive(Debug, Deserialize)]
pub struct Setup {
    pub model: String,
    pub permission_mode: Option<String>,
    pub approval_policy: Option<String>,
    pub sandbox: Option<String>,
    pub collaboration_mode: Option<String>,
    pub ephemeral: Option<bool>,
    pub system_prompt: Option<String>,
    pub max_turns: Option<u32>,
    pub max_budget_usd: Option<f64>,
    pub files: Option<HashMap<String, String>>,
}

#[derive(Debug)]
pub enum Step {
    Send(serde_json::Value),
    Expect(serde_json::Value),
    ExpectEventually(serde_json::Value),
    ExpectAny(Vec<serde_json::Value>),
    ExpectEventuallyAny(Vec<serde_json::Value>),
    DelayMs(u64),
}

impl<'de> Deserialize<'de> for Step {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(StepVisitor)
    }
}

struct StepVisitor;

impl<'de> Visitor<'de> for StepVisitor {
    type Value = Step;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a map with one key: send, expect, expect_eventually, expect_any, expect_eventually_any, or delay_ms"
        )
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let key: String = map
            .next_key()?
            .ok_or_else(|| de::Error::custom("empty step map"))?;
        let value: serde_yaml::Value = map.next_value()?;
        let json_value = yaml_to_json(&value);

        match key.as_str() {
            "send" => Ok(Step::Send(json_value)),
            "expect" => Ok(Step::Expect(json_value)),
            "expect_eventually" => Ok(Step::ExpectEventually(json_value)),
            "expect_any" => {
                let arr = match json_value {
                    serde_json::Value::Array(a) => a,
                    other => {
                        return Err(de::Error::custom(format!(
                            "expect_any must be an array, got: {other}"
                        )));
                    }
                };
                Ok(Step::ExpectAny(arr))
            }
            "expect_eventually_any" => {
                let arr = match json_value {
                    serde_json::Value::Array(a) => a,
                    other => {
                        return Err(de::Error::custom(format!(
                            "expect_eventually_any must be an array, got: {other}"
                        )));
                    }
                };
                Ok(Step::ExpectEventuallyAny(arr))
            }
            "delay_ms" => {
                let ms = match &json_value {
                    serde_json::Value::Number(n) => n
                        .as_u64()
                        .ok_or_else(|| de::Error::custom("delay_ms must be a positive integer"))?,
                    _ => return Err(de::Error::custom("delay_ms must be a number")),
                };
                Ok(Step::DelayMs(ms))
            }
            other => Err(de::Error::custom(format!("unknown step type: {other}"))),
        }
    }
}

fn yaml_to_json(v: &serde_yaml::Value) -> serde_json::Value {
    match v {
        serde_yaml::Value::Null => serde_json::Value::Null,
        serde_yaml::Value::Bool(b) => serde_json::Value::Bool(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_json::Value::Number(i.into())
            } else if let Some(u) = n.as_u64() {
                serde_json::Value::Number(u.into())
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            } else {
                serde_json::Value::Null
            }
        }
        serde_yaml::Value::String(s) => serde_json::Value::String(s.clone()),
        serde_yaml::Value::Sequence(seq) => {
            serde_json::Value::Array(seq.iter().map(yaml_to_json).collect())
        }
        serde_yaml::Value::Mapping(map) => {
            let json_map: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .filter_map(|(k, v)| {
                    let key = match k {
                        serde_yaml::Value::String(s) => s.clone(),
                        other => serde_json::to_string(&yaml_to_json(other)).ok()?,
                    };
                    Some((key, yaml_to_json(v)))
                })
                .collect();
            serde_json::Value::Object(json_map)
        }
        serde_yaml::Value::Tagged(tagged) => yaml_to_json(&tagged.value),
    }
}

#[derive(Debug, Deserialize)]
pub struct Postconditions {
    pub files: Option<HashMap<String, FileCheck>>,
}

#[derive(Debug, Deserialize)]
pub struct FileCheck {
    pub exists: Option<bool>,
    pub contains: Option<String>,
    pub not_contains: Option<String>,
}
