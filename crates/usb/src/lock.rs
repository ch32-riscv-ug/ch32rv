//! en: Per-probe advisory lock (docs/cli.ja.md §3.7). A command holds an exclusive OS file lock
//! (`flock`) on a file in the runtime dir, keyed by the probe's serial (or bus topology), for the
//! duration it uses the probe. Because the lock is an `flock`, the OS releases it automatically
//! when the holder exits - a crashed holder's lock is reclaimed with no manual stale sweep.
//! ja: probe 単位の advisory lock(cli.ja.md §3.7)。probe の serial(無ければ topology)をキーに
//! runtime dir 上のファイルへ排他 flock を取り、probe 使用中だけ保持する。flock なので保持者が
//! 終了すれば OS が自動解放 = 異常終了した保持者の lock も自動回収(stale の手動掃除が不要)。

use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fs4::fs_std::FileExt;

/// en: A held advisory lock on one probe. Releasing happens on drop (the file closes and the OS
/// drops the `flock`). ja: probe 1 台の advisory lock。drop でファイルが閉じ OS が flock を解放。
#[derive(Debug)]
pub struct DeviceLock {
    // Held only to keep the flock alive for this guard's lifetime; closed (released) on drop.
    _file: File,
}

/// Why a lock could not be taken.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another process held the probe past `--lock-timeout`.
    #[error("the device is in use by another process (waited {0:.1}s)")]
    Timeout(f64),
    /// The lock file / runtime dir could not be opened.
    #[error("could not open the device lock file: {0}")]
    Io(#[from] std::io::Error),
}

impl DeviceLock {
    /// en: Take the exclusive lock for `key` (a probe serial or bus topology), waiting up to
    /// `timeout`. `LockError::Timeout` when another holder does not release in time.
    /// ja: `key`(probe serial か topology)の排他 lock を `timeout` まで待って取る。
    pub fn acquire(key: &str, timeout: Duration) -> Result<Self, LockError> {
        let dir = lock_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.lock", sanitize(key)));
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)?;

        let deadline = Instant::now() + timeout;
        loop {
            // `flock` is advisory and per-open-file-description: an exclusive lock succeeds only
            // when no other process holds one on this path.
            if file.try_lock_exclusive()? {
                return Ok(DeviceLock { _file: file });
            }
            if Instant::now() >= deadline {
                return Err(LockError::Timeout(timeout.as_secs_f64()));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// The directory that holds the lock files: `$XDG_RUNTIME_DIR/ch32rv` when set (per-user, tmpfs,
/// cleared on logout), else a stable subdir of the system temp dir.
fn lock_dir() -> PathBuf {
    if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR")
        && !rt.is_empty()
    {
        return PathBuf::from(rt).join("ch32rv");
    }
    std::env::temp_dir().join("ch32rv-locks")
}

/// Make `key` a safe single path component: keep `[A-Za-z0-9._-]`, replace the rest with `_`.
fn sanitize(key: &str) -> String {
    let mut out: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out.push_str("unknown");
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn sanitize_keeps_safe_chars_and_replaces_others() {
        assert_eq!(sanitize("FBC18F0680B0"), "FBC18F0680B0");
        assert_eq!(sanitize("1a86:8010:AB"), "1a86_8010_AB");
        assert_eq!(sanitize("bus1-port2.3"), "bus1-port2.3");
        assert_eq!(sanitize(""), "unknown");
        assert_eq!(sanitize("../evil"), ".._evil");
    }

    #[test]
    fn second_acquire_times_out_while_first_is_held() {
        let key = format!("ch32rv-test-{}", std::process::id());
        let held = DeviceLock::acquire(&key, Duration::from_millis(0)).unwrap();
        // A second acquire in the SAME process on a different open file description also contends
        // (flock is per-open-file-description), so this must time out quickly.
        let start = Instant::now();
        let again = DeviceLock::acquire(&key, Duration::from_millis(150));
        assert!(matches!(again, Err(LockError::Timeout(_))));
        assert!(start.elapsed() >= Duration::from_millis(150));
        drop(held);
        // Once released, it can be taken again.
        assert!(DeviceLock::acquire(&key, Duration::from_millis(200)).is_ok());
    }
}
