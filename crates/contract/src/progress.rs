//! en: Progress and cancellation plumbing (docs/architecture.ja.md §2.2).
//! Every long-running library operation takes `&dyn ProgressSink` and `&CancelToken`.
//! The event type is identical to the NDJSON one ([`crate::Event`]), so the CLI, GUIs, and CI
//! never diverge in vocabulary.
//!
//! ja: 進捗と中断の受け渡し(docs/architecture.ja.md §2.2)。library の全長時間操作は
//! `&dyn ProgressSink` と `&CancelToken` を受ける。event の型は NDJSON と同一なので語彙が割れない。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::event::Event;

/// en: Receiver for progress events. The CLI feeds NDJSON / a progress bar; a GUI feeds its
/// own event loop.
/// ja: 進捗 event の受け口。CLI は NDJSON/progress bar へ、GUI は自分のイベントループへ流す。
pub trait ProgressSink: Send + Sync {
    fn event(&self, ev: &Event);
}

/// A sink that does nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl ProgressSink for NullSink {
    fn event(&self, _ev: &Event) {}
}

/// en: Cooperative cancellation. Long operations check [`CancelToken::is_cancelled`] at chunk
/// boundaries.
/// ja: 協調的キャンセル。長時間操作は chunk 境界で確認する。
#[derive(Debug, Default, Clone)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_token_roundtrip() {
        let t = CancelToken::new();
        let t2 = t.clone();
        assert!(!t2.is_cancelled());
        t.cancel();
        assert!(t2.is_cancelled());
    }
}
