use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub tool: String,
    #[serde(default)]
    pub args: serde_json::Value,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditPlan {
    pub goal: String,
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub steps: Vec<PlanStep>,
}

impl EditPlan {
    pub fn from_json_str(raw: &str) -> Result<EditPlan> {
        let trimmed = extract_json(raw)
            .ok_or_else(|| Error::invalid("no JSON object found in the model output"))?;
        let value: serde_json::Value = serde_json::from_str(&trimmed)?;
        let plan: EditPlan = serde_json::from_value(value)?;
        if plan.steps.is_empty() {
            return Err(Error::invalid("plan has no steps"));
        }
        Ok(plan)
    }
}

pub fn extract_json(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let start = raw.find('{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for idx in start..bytes.len() {
        let ch = bytes[idx] as char;
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(raw[start..=idx].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_nested_json() {
        let raw = "here is the plan: {\"goal\": \"a {b}\", \"steps\": [{\"tool\": \"x\"}]} done";
        let json = extract_json(raw).unwrap();
        let plan = EditPlan::from_json_str(raw).unwrap();
        assert_eq!(plan.goal, "a {b}");
        assert_eq!(
            json,
            "{\"goal\": \"a {b}\", \"steps\": [{\"tool\": \"x\"}]}"
        );
    }
}
