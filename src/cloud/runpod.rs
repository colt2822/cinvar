//! RunPod REST client (pods). Endpoints are exactly those listed in
//! https://rest.runpod.io/v1/openapi.json (read 2026-11-11). The base URL is configurable so the
//! whole lifecycle can be exercised against a local mock; a live call needs a user-owned key.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, ErrorCode};

pub const DEFAULT_BASE: &str = "https://rest.runpod.io/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudErrorKind {
    /// 401/403: bad or unauthorised key.
    Auth,
    /// 400/404/409/422: the request itself is wrong; retrying will not help.
    Rejected,
    NotFound,
    /// 429/5xx: safe to retry idempotent calls.
    Transient,
    /// No response: the outcome of a mutating call is UNKNOWN.
    Timeout,
}

#[derive(Debug, Clone, Serialize)]
pub struct CloudError {
    pub kind: CloudErrorKind,
    pub status: Option<u16>,
    pub message: String,
}

impl std::fmt::Display for CloudError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} ({:?}): {}", self.kind, self.status, self.message)
    }
}

impl From<CloudError> for Error {
    fn from(e: CloudError) -> Error {
        let code = match e.kind {
            CloudErrorKind::Auth => ErrorCode::PermissionDenied,
            CloudErrorKind::NotFound => ErrorCode::NotFound,
            CloudErrorKind::Rejected => ErrorCode::InvalidInput,
            _ => ErrorCode::Provider,
        };
        Error::new(code, format!("RunPod: {}", e.message))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Pod {
    pub id: String,
    pub name: String,
    pub desired_status: String,
    pub cost_per_hr: Option<f64>,
    pub public_ip: Option<String>,
    pub port_mappings: Option<Value>,
    pub image: Option<String>,
    pub gpu: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePod {
    pub name: String,
    pub image_name: String,
    pub gpu_type_ids: Vec<String>,
    pub gpu_count: u32,
    pub container_disk_in_gb: u32,
    pub volume_in_gb: u32,
    pub ports: Vec<String>,
    pub env: serde_json::Map<String, Value>,
    pub cloud_type: String,
    pub interruptible: bool,
}

#[derive(Clone)]
pub struct RunpodClient {
    base: String,
    key: String,
    agent: ureq::Agent,
}

impl RunpodClient {
    pub fn new(base: &str, key: &str) -> RunpodClient {
        RunpodClient {
            base: base.trim_end_matches('/').to_string(),
            key: key.to_string(),
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .build(),
        }
    }

    /// Same client with a different overall request timeout (tests and slow links).
    pub fn with_timeout(mut self, secs: u64) -> RunpodClient {
        self.agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(secs.max(1)))
            .build();
        self
    }

    /// Never include the key in any message.
    fn scrub(&self, text: &str) -> String {
        let t = if self.key.len() >= 6 {
            text.replace(&self.key, "[redacted]")
        } else {
            text.to_string()
        };
        crate::secrets::redact(&t)
    }

    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, CloudError> {
        let url = format!("{}{}", self.base, path);
        let req = self
            .agent
            .request(method, &url)
            .set("Authorization", &format!("Bearer {}", self.key))
            .set("Accept", "application/json");
        let result = match body {
            Some(b) => req.send_json(b.clone()),
            None => req.call(),
        };
        match result {
            Ok(resp) => {
                let text = resp.into_string().unwrap_or_default();
                if text.trim().is_empty() {
                    return Ok(Value::Null);
                }
                serde_json::from_str(&text).map_err(|e| CloudError {
                    kind: CloudErrorKind::Transient,
                    status: None,
                    message: format!("unreadable response: {e}"),
                })
            }
            Err(ureq::Error::Status(code, resp)) => {
                let text = self.scrub(&resp.into_string().unwrap_or_default());
                let short: String = text.chars().take(300).collect();
                let kind = match code {
                    401 | 403 => CloudErrorKind::Auth,
                    404 => CloudErrorKind::NotFound,
                    429 | 500..=599 => CloudErrorKind::Transient,
                    _ => CloudErrorKind::Rejected,
                };
                Err(CloudError {
                    kind,
                    status: Some(code),
                    message: format!("HTTP {code}: {short}"),
                })
            }
            Err(ureq::Error::Transport(t)) => {
                let msg = self.scrub(&t.to_string());
                let timed_out = msg.to_ascii_lowercase().contains("timed out")
                    || msg.to_ascii_lowercase().contains("timeout");
                Err(CloudError {
                    kind: if timed_out {
                        CloudErrorKind::Timeout
                    } else {
                        CloudErrorKind::Transient
                    },
                    status: None,
                    message: msg,
                })
            }
        }
    }

    /// Cheapest authenticated read: confirms the key works before any billable step.
    pub fn validate_credentials(&self) -> Result<(), CloudError> {
        self.call("GET", "/pods", None).map(|_| ())
    }

    pub fn list_pods(&self) -> Result<Vec<Pod>, CloudError> {
        let v = self.call("GET", "/pods", None)?;
        serde_json::from_value(v).map_err(|e| CloudError {
            kind: CloudErrorKind::Transient,
            status: None,
            message: format!("unexpected pod list shape: {e}"),
        })
    }

    pub fn get_pod(&self, id: &str) -> Result<Pod, CloudError> {
        let v = self.call("GET", &format!("/pods/{}", enc(id)), None)?;
        serde_json::from_value(v).map_err(|e| CloudError {
            kind: CloudErrorKind::Transient,
            status: None,
            message: format!("unexpected pod shape: {e}"),
        })
    }

    /// BILLABLE. Callers must have passed the cost gate.
    pub fn create_pod(&self, spec: &CreatePod) -> Result<Pod, CloudError> {
        let v = self.call("POST", "/pods", Some(&json!(spec)))?;
        serde_json::from_value(v).map_err(|e| CloudError {
            kind: CloudErrorKind::Timeout,
            status: None,
            message: format!("pod create returned an unreadable body (pod may exist): {e}"),
        })
    }

    pub fn terminate_pod(&self, id: &str) -> Result<(), CloudError> {
        match self.call("DELETE", &format!("/pods/{}", enc(id)), None) {
            Ok(_) => Ok(()),
            // Already gone is success for cleanup purposes.
            Err(e) if e.kind == CloudErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn stop_pod(&self, id: &str) -> Result<(), CloudError> {
        self.call("POST", &format!("/pods/{}/stop", enc(id)), None)
            .map(|_| ())
    }
}

fn enc(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_string()
            } else {
                format!("%{:02X}", c as u32)
            }
        })
        .collect()
}

/// Reference VRAM (GB) for names in the documented `gpuTypeIds` enum. This is a static table
/// for recommendations only; it is not a live availability or price feed.
pub const GPU_TABLE: &[(&str, u32)] = &[
    ("NVIDIA GeForce RTX 3090", 24),
    ("NVIDIA GeForce RTX 4090", 24),
    ("NVIDIA GeForce RTX 5090", 32),
    ("NVIDIA RTX A5000", 24),
    ("NVIDIA RTX A6000", 48),
    ("NVIDIA A40", 48),
    ("NVIDIA L40S", 48),
    ("NVIDIA RTX 6000 Ada Generation", 48),
    ("NVIDIA A100 80GB PCIe", 80),
    ("NVIDIA H100 80GB HBM3", 80),
    ("NVIDIA L4", 24),
    ("NVIDIA RTX A4000", 16),
];

/// Candidate GPUs with at least `min_vram_gb`, smallest sufficient first.
pub fn recommend_gpus(min_vram_gb: u32) -> Vec<String> {
    let mut ok: Vec<&(&str, u32)> = GPU_TABLE
        .iter()
        .filter(|(_, v)| *v >= min_vram_gb)
        .collect();
    ok.sort_by_key(|(n, v)| (*v, n.to_string()));
    ok.into_iter().map(|(n, _)| n.to_string()).collect()
}

pub fn proxy_url(pod_id: &str, port: u16) -> String {
    format!("https://{pod_id}-{port}.proxy.runpod.net")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommends_smallest_sufficient_first() {
        let g = recommend_gpus(20);
        assert!(g
            .iter()
            .all(|n| GPU_TABLE.iter().any(|(m, v)| m == n && *v >= 20)));
        assert_eq!(GPU_TABLE.iter().find(|(n, _)| *n == g[0]).unwrap().1, 24);
        assert!(recommend_gpus(200).is_empty());
    }

    #[test]
    fn proxy_url_format_matches_docs() {
        assert_eq!(
            proxy_url("abc123xyz", 4000),
            "https://abc123xyz-4000.proxy.runpod.net"
        );
    }

    #[test]
    fn ids_are_path_encoded() {
        assert_eq!(enc("abc/../x"), "abc%2F%2E%2E%2Fx");
    }
}
