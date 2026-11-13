//! en: Per-probe advisory lock (docs/cli.ja.md §3.7). A command holds an exclusive OS file lock
//! (`flock`) on a file in the runtime dir, keyed by the probe's serial (or bus topology), for the
//! duration it uses the probe. Because the lock is an `flock`, the OS releases it automatically
//! when the holder exits - a crashed holder's lock is reclaimed with no manual stale sweep.
//! ja: probe 単位の advisory lock(cli.ja.md §3.7)。probe の serial(無ければ topology)をキーに
//! runtime dir 上のファイルへ排他 flock を取り、probe 使用中だけ保持する。flock なので保持者が
//! 終了すれば OS が自動解放 = 異常終了した保持者の lock も自動回収(stale の手動掃除が不要)。

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
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
        Self::acquire_path(&dir.join(format!("{}.lock", sanitize(key))), timeout)
    }

    /// en: Take the exclusive lock on the file at `path` (created when missing, never removed),
    /// waiting up to `timeout`: flock on unix, LockFileEx on Windows (OEP host guide §7.2's
    /// `<unit_id>.lock`, which every broker implementation takes). ja: `path` のファイルの排他 lock
    /// を `timeout` まで待って取る(無ければ作り、消さない)。
    pub fn acquire_path(path: &Path, timeout: Duration) -> Result<Self, LockError> {
        if crate::replay::active() {
            return Ok(DeviceLock { _file: None });
        }
        let mut o = OpenOptions::new();
        o.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let file = o.open(path)?;

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

/// en: The per-user directory where OEP brokers of any implementation publish themselves (OEP
/// host guide §7.2): `$XDG_RUNTIME_DIR/oep/brokers` on Linux, else `$TMPDIR/oep-<uid>/brokers`
/// (`/tmp` without `TMPDIR`); `$TMPDIR/oep/brokers` on macOS; `%LOCALAPPDATA%\oep\brokers` on
/// Windows. `None` when the place cannot be named (no `LOCALAPPDATA`). Unlike [`runtime_dir`],
/// this is an interface: other tools read it. ja: どの実装の OEP ブローカーも自分を載せる利用者
/// ごとの場所(host ガイド §7.2)。[`runtime_dir`] と違い、ほかの道具も読む取り決めの場所。
pub fn oep_brokers_dir() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    #[cfg(windows)]
    {
        var("LOCALAPPDATA").map(|d| PathBuf::from(d).join("oep").join("brokers"))
    }
    #[cfg(target_os = "macos")]
    {
        Some(match var("TMPDIR") {
            Some(t) => PathBuf::from(t).join("oep").join("brokers"),
            None => brokers_dir_for(None, None, user_id()?),
        })
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Some(brokers_dir_for(
            var("XDG_RUNTIME_DIR"),
            var("TMPDIR"),
            user_id()?,
        ))
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

/// The Linux rule of [`oep_brokers_dir`] (also macOS's without `TMPDIR`).
#[cfg_attr(windows, allow(dead_code))]
fn brokers_dir_for(
    xdg: Option<std::ffi::OsString>,
    tmpdir: Option<std::ffi::OsString>,
    uid: u32,
) -> PathBuf {
    match xdg {
        Some(rt) => PathBuf::from(rt).join("oep").join("brokers"),
        None => PathBuf::from(tmpdir.unwrap_or_else(|| "/tmp".into()))
            .join(format!("oep-{uid}"))
            .join("brokers"),
    }
}

/// en: [`oep_brokers_dir`] when it exists and may be read: on unix it and its parent (`oep`,
/// `oep-<uid>`) are directories of this user's with mode 0700 (host guide §7.2: otherwise it is
/// not used). ja: 読んでよいときの [`oep_brokers_dir`](unix では自分の 0700 のディレクトリ)。
pub fn oep_brokers_dir_checked() -> Option<PathBuf> {
    let dir = oep_brokers_dir()?;
    private_dirs_ok(&dir).then_some(dir)
}

/// en: [`oep_brokers_dir`], created when missing (0700 on unix); refused when it or its parent is
/// not this user's 0700 directory (one another user made in the shared temp dir, or a symlink).
/// ja: [`oep_brokers_dir`] を(無ければ 0700 で)作って返す。自分の 0700 でなければ使わない。
pub fn create_oep_brokers_dir() -> std::io::Result<PathBuf> {
    let dir = oep_brokers_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no per-user directory for OEP brokers (LOCALAPPDATA / XDG_RUNTIME_DIR / TMPDIR)",
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        // The parent first (`oep` / `oep-<uid>`), so both levels we make are 0700.
        if let Some(parent) = dir.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(&dir)?;
    if !private_dirs_ok(&dir) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} (or its parent) is not a 0700 directory of this user's: not used",
                dir.display()
            ),
        ));
    }
    Ok(dir)
}

/// Whether `dir` and its parent are this user's directories with mode 0700 (always on Windows,
/// where `LOCALAPPDATA` is the user's own).
fn private_dirs_ok(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let ok = |p: &Path| {
            std::fs::symlink_metadata(p).is_ok_and(|m| {
                m.is_dir() && Some(m.uid()) == user_id() && m.mode() & 0o777 == 0o700
            })
        };
        ok(dir) && dir.parent().is_none_or(ok)
    }
    #[cfg(not(unix))]
    {
        dir.is_dir()
    }
}

/// en: Whether process `pid` still runs (unix: signal 0; a process of another user counts). Always
/// true elsewhere, where the caller's next step (a connection) decides. ja: プロセス `pid` が
/// まだ動いているか(unix は signal 0)。ほかの OS では常に true(次の接続で決まる)。
pub fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Some(p) = i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        else {
            return false;
        };
        !matches!(
            rustix::process::test_kill_process(p),
            Err(rustix::io::Errno::SRCH)
        )
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
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
    fn the_oep_brokers_dir_follows_host_guide_7_2() {
        assert_eq!(
            brokers_dir_for(Some("/run/user/1000".into()), Some("/var/tmp".into()), 1000),
            PathBuf::from("/run/user/1000/oep/brokers")
        );
        assert_eq!(
            brokers_dir_for(None, Some("/var/tmp".into()), 1000),
            PathBuf::from("/var/tmp/oep-1000/brokers")
        );
        assert_eq!(
            brokers_dir_for(None, None, 1001),
            PathBuf::from("/tmp/oep-1001/brokers")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_brokers_dir_others_can_read_is_not_used() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("ch32rv-brokers-test-{}", std::process::id()));
        let dir = base.join("oep").join("brokers");
        std::fs::create_dir_all(&dir).unwrap();
        for p in [base.join("oep"), dir.clone()] {
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert!(private_dirs_ok(&dir));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!private_dirs_ok(&dir));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(base.join("oep"), std::fs::Permissions::from_mode(0o750)).unwrap();
        assert!(!private_dirs_ok(&dir));
        let _ = std::fs::remove_dir_all(&base);
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
