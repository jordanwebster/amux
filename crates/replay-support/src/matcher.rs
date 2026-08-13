use std::collections::HashMap;

/// Match an expected JSON value against an actual JSON value.
pub fn match_value(
    expected: &serde_json::Value,
    actual: &serde_json::Value,
    captures: &mut HashMap<String, serde_json::Value>,
) -> Result<(), String> {
    match (expected, actual) {
        (serde_json::Value::String(pattern), actual_val) => {
            if let Some(var_name) = pattern.strip_prefix('$') {
                captures.insert(var_name.to_string(), actual_val.clone());
                return Ok(());
            }

            if pattern == "*" {
                return Ok(());
            }

            if pattern.contains('*') {
                let actual_str = value_to_string(actual_val);
                if glob_match(pattern, &actual_str) {
                    return Ok(());
                }
                return Err(format!(
                    "glob mismatch: pattern '{pattern}' did not match '{actual_str}'"
                ));
            }

            let actual_str = match actual_val {
                serde_json::Value::String(s) => s.clone(),
                _ => value_to_string(actual_val),
            };
            if pattern == &actual_str {
                Ok(())
            } else {
                Err(format!(
                    "exact mismatch: expected '{pattern}', got '{actual_str}'"
                ))
            }
        }
        (serde_json::Value::Null, serde_json::Value::Null) => Ok(()),
        (serde_json::Value::Null, other) => Err(format!("expected null, got {other}")),
        (serde_json::Value::Object(exp_map), serde_json::Value::String(act_str)) => {
            if exp_map.len() == 1 {
                let (key, val) = exp_map.iter().next().expect("single key object");
                if key == act_str
                    && (val.is_null()
                        || (val.is_object() && val.as_object().is_some_and(|m| m.is_empty())))
                {
                    return Ok(());
                }
            }
            Err(format!("expected object, got string \"{act_str}\""))
        }
        (serde_json::Value::Object(exp_map), serde_json::Value::Object(act_map)) => {
            for (key, exp_val) in exp_map {
                let act_val = act_map
                    .get(key)
                    .ok_or_else(|| format!("missing key '{key}' in actual object"))?;
                match_value(exp_val, act_val, captures)
                    .map_err(|e| format!("at key '{key}': {e}"))?;
            }
            Ok(())
        }
        (serde_json::Value::Object(_), other) => {
            Err(format!("expected object, got {}", value_type_name(other)))
        }
        (serde_json::Value::Array(exp_arr), serde_json::Value::Array(act_arr)) => {
            if exp_arr.len() == 1 && exp_arr[0] == serde_json::Value::String("*".to_string()) {
                return Ok(());
            }
            if exp_arr.len() != act_arr.len() {
                return Err(format!(
                    "array length mismatch: expected {}, got {}",
                    exp_arr.len(),
                    act_arr.len()
                ));
            }
            for (i, (e, a)) in exp_arr.iter().zip(act_arr.iter()).enumerate() {
                match_value(e, a, captures).map_err(|err| format!("at index {i}: {err}"))?;
            }
            Ok(())
        }
        (serde_json::Value::Array(_), other) => {
            Err(format!("expected array, got {}", value_type_name(other)))
        }
        (serde_json::Value::Number(exp), serde_json::Value::Number(act)) => {
            if exp == act {
                Ok(())
            } else {
                Err(format!("number mismatch: expected {exp}, got {act}"))
            }
        }
        (serde_json::Value::Bool(exp), serde_json::Value::Bool(act)) => {
            if exp == act {
                Ok(())
            } else {
                Err(format!("bool mismatch: expected {exp}, got {act}"))
            }
        }
        (exp, act) => Err(format!(
            "type mismatch: expected {}, got {}",
            value_type_name(exp),
            value_type_name(act)
        )),
    }
}

fn value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn value_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();

    if parts.len() == 1 {
        return pattern == text;
    }

    let mut pos = 0;

    if !parts[0].is_empty() {
        if !text.starts_with(parts[0]) {
            return false;
        }
        pos = parts[0].len();
    }

    for part in &parts[1..parts.len() - 1] {
        if part.is_empty() {
            continue;
        }
        if let Some(found) = text[pos..].find(part) {
            pos += found + part.len();
        } else {
            return false;
        }
    }

    let last = parts[parts.len() - 1];
    if !last.is_empty() && !text[pos..].ends_with(last) {
        return false;
    }

    true
}
