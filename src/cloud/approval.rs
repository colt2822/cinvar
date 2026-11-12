//! Human-controlled authorization for billable cloud operations.
//!
//! An AI agent can request a quote, echo its id and set every flag in the tool call, so none of
//! those arguments is an authorization. The authorization is a record that only a human can
//! create: `cinvar cloud approve <quote-id>` shows the quote and requires an interactive
//! terminal and a typed confirmation. The record lives in the CINVAR config directory (not in
//! the project, and hidden from plugins by the sandbox), names exactly one quote id (which
//! hashes the manifest, image, price ceiling and runtime), expires after [`TTL_SECS`] and is
//! consumed by the render that uses it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::cloud::orchestrator::Quote;
use crate::error::{Error, ErrorCode, Result};
use crate::util::paths;

pub const TTL_SECS: u64 = 15 * 60;

fn dir(kind: &str) -> PathBuf {
    paths::config_dir().join("cloud").join(kind)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Pending {
    pub quote: Quote,
    pub project: String,
    pub requested_at: u64,
}

/// Remember a quote an agent asked for, so a human can review and approve it by id.
pub fn record_pending(quote: &Quote, project: &std::path::Path) -> Result<()> {
    if !valid_id(&quote.id) {
        return Err(Error::invalid("quote id has an unexpected form"));
    }
    let d = dir("pending");
    paths::ensure_dir(&d)?;
    let rec = Pending {
        quote: quote.clone(),
        project: project.to_string_lossy().to_string(),
        requested_at: now(),
    };
    paths::write_atomic(
        &d.join(format!("{}.json", quote.id)),
        &serde_json::to_vec_pretty(&rec)?,
    )?;
    Ok(())
}

pub fn load_pending(id: &str) -> Result<Pending> {
    if !valid_id(id) {
        return Err(Error::invalid("invalid quote id"));
    }
    let text =
        std::fs::read_to_string(dir("pending").join(format!("{id}.json"))).map_err(|_| {
            Error::not_found(format!(
                "no pending quote {id}; ask the agent to request a quote first"
            ))
        })?;
    Ok(serde_json::from_str(&text)?)
}

pub fn list_pending() -> Vec<Pending> {
    let Ok(rd) = std::fs::read_dir(dir("pending")) else {
        return Vec::new();
    };
    let mut out: Vec<Pending> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str(&t).ok())
        .collect();
    out.sort_by_key(|p| std::cmp::Reverse(p.requested_at));
    out
}

/// Record the human's approval. Callers (the CLI) must have obtained it interactively.
pub fn approve(id: &str) -> Result<()> {
    load_pending(id)?;
    let d = dir("approved");
    paths::ensure_dir(&d)?;
    paths::write_atomic(&d.join(id), now().to_string().as_bytes())?;
    Ok(())
}

/// Called by the render tool: succeeds once per approval, within the TTL.
pub fn consume(id: &str) -> Result<()> {
    let denied = || {
        Error::new(
            ErrorCode::PermissionDenied,
            "billable cloud rendering needs approval from a human; nothing was started",
        )
        .with_hint(format!(
            "ask the user to review and approve this quote in a terminal: cinvar cloud approve {id}"
        ))
    };
    if !valid_id(id) {
        return Err(denied());
    }
    let file = dir("approved").join(id);
    let at: u64 = std::fs::read_to_string(&file)
        .ok()
        .and_then(|t| t.trim().parse().ok())
        .ok_or_else(denied)?;
    // Single use: remove first so two racing renders cannot share one approval.
    std::fs::remove_file(&file).map_err(|_| denied())?;
    if now().saturating_sub(at) > TTL_SECS {
        return Err(Error::new(
            ErrorCode::PermissionDenied,
            "the approval for this quote expired; nothing was started",
        )
        .with_hint(format!("approve it again: cinvar cloud approve {id}")));
    }
    Ok(())
}
