//! Shared HTTP plumbing for the network providers.
//!
//! Every outgoing request goes through [Http]: an explicit connect and overall
//! timeout, a capped response body, a bounded retry for transient statuses, and
//! error text that is passed through [crate::secrets::redact] and truncated.
//! Request headers and API keys are never logged.
//!
//! Transient statuses (429, 500, 502, 503, 504) are retried at most 3 times -
//! so at most 4 attempts in total - sleeping 1s, 2s then 4s between them.

use std::io::Read;
use std::time::Duration;

use serde_json::Value;

use crate::error::{Error, ErrorCode, Result};
use crate::secrets;

/// Hard cap on the number of response bytes buffered from a provider (~4 MiB).
pub const MAX_BODY_BYTES: u64 = 4 * 1024 * 1024;
/// Error bodies are truncated to this many characters before entering a message.
pub const MAX_ERROR_CHARS: usize = 600;
/// Backoff slept before each retry; its length is also the retry budget.
pub const RETRY_BACKOFF_SECS: [u64; 3] = [1, 2, 4];

/// Transient statuses a provider may recover from: rate limiting and gateway
/// failures. Every other status is reported to the caller on the first attempt.
pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

/// Backoff schedule, exposed so tests and diagnostics can assert it.
pub fn backoff_schedule() -> &'static [u64] {
    &RETRY_BACKOFF_SECS
}

/// The URL without its query string. Gemini puts the API key in the query, so
/// nothing that leaves this module may echo a raw URL.
pub fn public_url(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

/// Redact known secrets, flatten newlines and truncate to [MAX_ERROR_CHARS].
pub fn shorten(text: &str) -> String {
    let clean = secrets::redact(text);
    let trimmed = clean.trim();
    let mut out = String::new();
    for (index, ch) in trimmed.chars().enumerate() {
        if index >= MAX_ERROR_CHARS {
            out.push_str("...[truncated]");
            break;
        }
        out.push(if ch == '\n' || ch == '\r' { ' ' } else { ch });
    }
    out
}

/// A safe, redacted, truncated description of an HTTP failure.
pub fn redacted_error_message(status: u16, body: &str) -> String {
    let detail = shorten(body);
    if detail.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {detail}")
    }
}

/// One HTTP exchange. Any status is returned as data; only a transport failure
/// is an error, because the caller decides which statuses are acceptable.
pub(crate) struct Raw {
    pub(crate) status: u16,
    pub(crate) body: String,
}

pub struct Http {
    agent: ureq::Agent,
}

impl Http {
    pub fn new() -> Http {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(180))
            .user_agent(concat!("cinvar/", env!("CARGO_PKG_VERSION")))
            .build();
        Http { agent }
    }

    /// Perform one exchange, retrying transient statuses with the documented
    /// backoff. The returned status is whatever the final attempt produced.
    pub(crate) fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Raw> {
        let mut attempt = 0usize;
        loop {
            let raw = match self.send(method, url, headers, body) {
                Ok(raw) => raw,
                Err(detail) => {
                    return Err(Error::new(
                        ErrorCode::Provider,
                        format!("could not reach {}: {}", public_url(url), shorten(&detail)),
                    ))
                }
            };
            if is_retryable_status(raw.status) && attempt < RETRY_BACKOFF_SECS.len() {
                std::thread::sleep(Duration::from_secs(RETRY_BACKOFF_SECS[attempt]));
                attempt += 1;
                continue;
            }
            return Ok(raw);
        }
    }

    fn send(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Option<&Value>,
    ) -> std::result::Result<Raw, String> {
        let mut request = if method.eq_ignore_ascii_case("GET") {
            self.agent.get(url)
        } else {
            self.agent.post(url)
        };
        for (name, value) in headers {
            request = request.set(name, value);
        }
        let outcome = match body {
            Some(value) => match serde_json::to_string(value) {
                Ok(text) => request
                    .set("content-type", "application/json")
                    .send_string(&text),
                Err(err) => return Err(format!("could not encode the request body: {err}")),
            },
            None => request.call(),
        };
        match outcome {
            Ok(response) => Ok(Raw {
                status: response.status(),
                body: read_body(response),
            }),
            Err(ureq::Error::Status(status, response)) => Ok(Raw {
                status,
                body: read_body(response),
            }),
            Err(ureq::Error::Transport(transport)) => Err(transport.to_string()),
        }
    }
}

fn read_body(response: ureq::Response) -> String {
    let mut buffer: Vec<u8> = Vec::new();
    let mut reader = response.into_reader().take(MAX_BODY_BYTES);
    if reader.read_to_end(&mut buffer).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&buffer).to_string()
}

/// Require a 2xx and decode the JSON body, or describe the failure safely.
pub(crate) fn decode_ok(raw: &Raw, what: &str, url: &str) -> Result<Value> {
    if !(200..300).contains(&raw.status) {
        let mut error = Error::new(
            ErrorCode::Provider,
            format!(
                "{what} failed: {} ({})",
                redacted_error_message(raw.status, &raw.body),
                public_url(url)
            ),
        );
        if raw.status == 401 || raw.status == 403 {
            error = error.with_hint("check that the API key is valid and has access to this model");
        }
        return Err(error);
    }
    serde_json::from_str::<Value>(&raw.body).map_err(|err| {
        Error::new(
            ErrorCode::Protocol,
            format!(
                "{what} returned malformed JSON from {}: {err}",
                public_url(url)
            ),
        )
    })
}

/// Health probes treat an authenticated 4xx as reachable: the credentials were
/// accepted and the server answered, the endpoint or parameters were just not
/// what we guessed. Only transport failures, 401, 403 and server errors fail.
pub(crate) fn health_status(raw: &Raw, what: &str, url: &str) -> Result<()> {
    let status = raw.status;
    if (200..400).contains(&status) {
        return Ok(());
    }
    if status == 401 || status == 403 {
        return Err(Error::new(
            ErrorCode::Provider,
            format!(
                "{what} rejected the credentials: {}",
                redacted_error_message(status, &raw.body)
            ),
        )
        .with_hint("store a valid key with: cinvar models key <profile>"));
    }
    if (400..500).contains(&status) {
        return Ok(());
    }
    Err(Error::new(
        ErrorCode::Provider,
        format!(
            "{what} at {} is unavailable: {}",
            public_url(url),
            redacted_error_message(status, &raw.body)
        ),
    ))
}
