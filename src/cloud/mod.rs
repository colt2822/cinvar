//! Cloud rendering: portable job manifests, a content-addressed/resumable transfer protocol,
//! a RunPod provider (docs reviewed 2026-11-11), the worker that runs inside the pod, and the
//! orchestrator that adds cost gating, lifecycle cleanup and crash-safe bookkeeping.
//!
//! Provider facts used (from docs.runpod.io and https://rest.runpod.io/v1/openapi.json):
//! * REST base `https://rest.runpod.io/v1`, `Authorization: Bearer <key>`.
//! * `POST /pods`, `GET /pods`, `GET|DELETE /pods/{podId}`, `POST /pods/{podId}/stop`.
//! * No REST endpoint lists GPU prices; `costPerHr` is only known on the created Pod, so the
//!   price ceiling is enforced *after* creation by immediate termination, and the pre-creation
//!   quote is a worst case computed from the user's ceiling and runtime limit.
//! * HTTP ports are reached through `https://[POD_ID]-[PORT].proxy.runpod.net`; Cloudflare cuts
//!   requests at 100 s, so every request in the worker protocol is short and transfers are chunked.
//! * Serverless endpoints (`https://api.runpod.ai/v2/<id>/run|status|cancel|health`) have
//!   different billing/lifecycle (per-request, 30 min result retention) and are NOT implemented
//!   here; this provider is pod-based only.

pub mod approval;
pub mod manifest;
pub mod orchestrator;
pub mod runpod;
pub mod worker;

/// Chunk size for uploads/downloads (kept far under any proxy body limit; docs state none).
pub const CHUNK_BYTES: u64 = 8 * 1024 * 1024;
/// Port the CINVAR worker listens on inside the pod.
pub const WORKER_PORT: u16 = 8000;
