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

/// en: A held advisory lock on one probe. Releasing happens on drop (the file closes and the OS
/// drops the `flock`). ja: probe 1 台の advisory lock。drop でファイルが閉じ OS が flock を解放。
#[derive(Debug)]
pub struct DeviceLock {
    // Held only to keep the flock alive for this guard's lifetime; closed (released) on drop.
    // `None` under replay, where there is no physical probe to serialize against.
    _file: Option<File>,
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
        // en: Replay serves recorded transfers from a file - there is no probe on the other end to
        // serialize against. Taking the real lock would make an offline replay fail with
        // `device-busy` (exit 13) whenever some other process happens to hold that probe, which is
        // exactly what happened when the test suite ran beside a bench session.
        // ja: replay は記録済み転送をファイルから供給するだけで、直列化すべき実機が居ない。ここで
        // 本物の lock を取ると、別プロセスがその probe を握っているだけでオフラインの replay が
        // `device-busy`(exit 13)で落ちる(実機テストと並走したときに実際に踏んだ)。
        if crate::replay::active() {
            return Ok(DeviceLock { _file: None });
        }
        let dir = create_runtime_dir()?;
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
            // en: fs4's, named in full: from Rust 1.89 std's own `File::try_lock` would win the
            // method lookup, and the MSRV is 1.88.
            // ja: fs4 のものを明示して呼ぶ(Rust 1.89 からは std の `File::try_lock` がメソッド解決で
            // 勝つ。MSRV は 1.88)。
            match fs4::FileExt::try_lock(&file) {
                Ok(()) => return Ok(DeviceLock { _file: Some(file) }),
                Err(fs4::TryLockError::WouldBlock) => {}
                Err(fs4::TryLockError::Error(e)) => return Err(e.into()),
            }
            if Instant::now() >= deadline {
                return Err(LockError::Timeout(timeout.as_secs_f64()));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// en: The per-user runtime directory ch32rv keeps its lock files and the broker's files in
/// (internal: their names and contents are no interface, docs/cli.ja.md §4.12):
/// `$XDG_RUNTIME_DIR/ch32rv` when set (per-user, tmpfs, cleared on logout), else
/// `<temp>/ch32rv-<uid>` (0700) on unix - one per user, so users of one machine never meet each
/// other's files - and `<temp>/ch32rv-locks` elsewhere (the temp dir is the user's own there).
/// ja: ch32rv が lock ファイルとブローカーのファイルを置く利用者ごとの dir(内部のもの)。
/// `$XDG_RUNTIME_DIR/ch32rv`、無ければ unix では `<temp>/ch32rv-<uid>`(0700。利用者どうしで
/// 共有しない)、ほかでは `<temp>/ch32rv-locks`(temp がもともと利用者ごと)。
pub fn runtime_dir() -> PathBuf {
    dir_for(
        std::env::var_os("XDG_RUNTIME_DIR"),
        std::env::temp_dir(),
        user_id(),
    )
}

fn dir_for(xdg: Option<std::ffi::OsString>, temp: PathBuf, uid: Option<u32>) -> PathBuf {
    if let Some(rt) = xdg.filter(|r| !r.is_empty()) {
        return PathBuf::from(rt).join("ch32rv");
    }
    match uid {
        Some(u) => temp.join(format!("ch32rv-{u}")),
        None => temp.join("ch32rv-locks"),
    }
}

#[cfg(unix)]
fn user_id() -> Option<u32> {
    Some(rustix::process::getuid().as_raw())
}

#[cfg(not(unix))]
fn user_id() -> Option<u32> {
    None
}

/// en: [`runtime_dir`], created when missing (0700 on unix). On unix it must be a directory of
/// this user's: one another user made in the shared temp dir (or a symlink there) is refused
/// rather than used, and one of ours that others can read is made 0700.
/// ja: [`runtime_dir`] を(無ければ 0700 で)作って返す。unix では自分の dir でなければ使わない
/// (共有の temp に他の利用者が作った dir や symlink は断る)。他人が読める自分の dir は 0700 にする。
pub fn create_runtime_dir() -> std::io::Result<PathBuf> {
    let dir = runtime_dir();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        let m = std::fs::symlink_metadata(&dir)?;
        if !m.is_dir() || Some(m.uid()) != user_id() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "{} is not a directory of this user's (uid {}); remove it or set XDG_RUNTIME_DIR",
                    dir.display(),
                    m.uid()
                ),
            ));
        }
        if m.mode() & 0o077 != 0 {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Make `key` a safe single path component: keep `[A-Za-z0-9._-]`, replace the rest with `_`.
pub fn sanitize(key: &str) -> String {
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
    fn the_runtime_dir_is_per_user_without_xdg_runtime_dir() {
        let tmp = PathBuf::from("/tmp");
        assert_eq!(
            dir_for(Some("/run/user/1000".into()), tmp.clone(), Some(1000)),
            PathBuf::from("/run/user/1000/ch32rv")
        );
        assert_eq!(
            dir_for(Some("".into()), tmp.clone(), Some(1000)),
            PathBuf::from("/tmp/ch32rv-1000")
        );
        assert_eq!(
            dir_for(None, tmp.clone(), Some(1001)),
            PathBuf::from("/tmp/ch32rv-1001")
        );
        assert_eq!(dir_for(None, tmp, None), PathBuf::from("/tmp/ch32rv-locks"));
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
