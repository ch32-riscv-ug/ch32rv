//! en: NDJSON progress events (docs/contract/events.schema.json).
//! With `--progress ndjson` one event per line goes to stderr; the same type flows through
//! the library's ProgressSink.
//!
//! ja: NDJSON progress event(docs/contract/events.schema.json)。
//! `--progress ndjson` 時に stderr へ 1 行 1 event。library の ProgressSink にも同じ型を流す。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ev", rename_all = "lowercase")]
pub enum Event {
    /// Start of a phase (erase / program / verify / reset / confirm-run, ...).
    Phase {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
    },
    /// Progress within a phase.
    Progress {
        phase: String,
        done: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
    },
    /// en: A retry happened. Retries MUST be visible (docs/cli.ja.md §3.5).
    /// ja: 再試行の発生。再試行は必ず可視化する(docs/cli.ja.md §3.5)。
    Retry {
        phase: String,
        attempt: u32,
        cause: String,
    },
    /// Warning with a stable code (e.g. fw-known-bad, target-unverified).
    Warn { code: String, msg: String },
    /// Free-form log line.
    Log { level: LogLevel, msg: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn event_tagging_matches_schema() {
        let ev = Event::Retry {
            phase: "program".into(),
            attempt: 2,
            cause: "transport-timeout".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(
            json,
            r#"{"ev":"retry","phase":"program","attempt":2,"cause":"transport-timeout"}"#
        );
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn progress_omits_missing_total() {
        let ev = Event::Progress {
            phase: "program".into(),
            done: 8192,
            total: None,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(json, r#"{"ev":"progress","phase":"program","done":8192}"#);
    }
}
