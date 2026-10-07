//! en: `uv run` of oep-client-python for ch32rv's tests, in ch32rv's own environment
//! (`target/oep-client-venv`), synced once - under a file lock, so the test processes running in
//! parallel never install into it at the same time (they did into the client's shared `.venv`, and
//! a virtual probe that died while uv reinstalled failed the test, 2026-10-01).
//! The virtual bench (oep-client-python's simulated probe, targets and fixture wiring; called the
//! fake until 472ec96) is taken at `BENCH_REV`, a commit on the spec ch32rv speaks (oep-spec
//! f8bb2de, oep-probe-arduino 0.0.29-dev+f32a3ef and up): it moves with the spec ahead of the
//! probes, so ch32rv raises it when it follows. `$OEP_CLIENT_PYTHON` names a checkout used as it
//! is.
//! ja: テスト用の oep-client-python の `uv run`。ch32rv 専用の環境で、file lock の下で 1 回だけ sync
//! する(並行するテストが同時に install しないように)。virtual bench(旧 fake)は ch32rv の話す仕様の
//! commit `BENCH_REV` で取り出す(ch32rv が追従するときに上げる)。
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// oep-client-python on oep-spec f8bb2de: the virtual bench, with `lose` and `reboot`.
const BENCH_REV: &str = "b9e4a00";

fn target_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    exe.ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "target"))
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir)
}

fn venv() -> PathBuf {
    target_dir().join(format!("oep-client-venv-{BENCH_REV}"))
}

/// en: The oep-client-python to run the virtual bench from: `$OEP_CLIENT_PYTHON` as it is, else the
/// sibling checkout's `BENCH_REV` extracted (`git archive`, the checkout itself untouched) under
/// `target/`. None, with a note, when neither is there.
/// ja: virtual bench を動かす oep-client-python。`$OEP_CLIENT_PYTHON` はそのまま、無ければ隣の checkout の
/// `BENCH_REV` を target/ の下に取り出す(checkout には触らない)。どちらも無ければ None。
pub fn client_dir() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        if let Some(d) = std::env::var_os("OEP_CLIENT_PYTHON") {
            return Some(PathBuf::from(d));
        }
        let checkout = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .find(|p| p.join("Cargo.lock").exists())
            .map(|r| r.join("../../dev_oep/oep-client-python"))?;
        let out = target_dir().join(format!("oep-client-{BENCH_REV}"));
        let _guard = Lock::take(&out.with_extension("lock"));
        if !out.join("src/oep_client/virtual_bench_serve.py").exists() {
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir_all(&out).ok()?;
            let archive = Command::new("git")
                .arg("-C")
                .arg(&checkout)
                .args(["archive", "--format=tar", BENCH_REV])
                .output()
                .ok()
                .filter(|o| o.status.success())?;
            let mut tar = Command::new("tar")
                .arg("-x")
                .arg("-C")
                .arg(&out)
                .stdin(std::process::Stdio::piped())
                .spawn()
                .ok()?;
            std::io::Write::write_all(tar.stdin.as_mut()?, &archive.stdout).ok()?;
            drop(tar.stdin.take());
            tar.wait().ok().filter(|s| s.success())?;
        }
        Some(out)
    })
    .clone()
    .filter(|d| d.join("src/oep_client/virtual_bench_serve.py").exists())
    .or_else(|| {
        eprintln!("skip: no oep-client-python at {BENCH_REV} (set OEP_CLIENT_PYTHON or check out ../dev_oep/oep-client-python)");
        None
    })
}

/// A lock file made with create_new (MSRV 1.88 has no File::lock); a stale one (a test process
/// killed while it held it) is taken over after 2 minutes.
struct Lock(PathBuf);

impl Lock {
    fn take(path: &Path) -> Self {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(_) => return Self(path.to_path_buf()),
                Err(_) => {
                    let stale = std::fs::metadata(path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age.as_secs() > 120);
                    if stale {
                        let _ = std::fs::remove_file(path);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn uv_run(dir: &Path) -> Command {
    static SYNCED: OnceLock<()> = OnceLock::new();
    let env = venv();
    SYNCED.get_or_init(|| {
        let _guard = Lock::take(&env.with_extension("lock"));
        let _ = Command::new("uv")
            .args(["sync", "--quiet", "--project"])
            .arg(dir)
            .env("UV_PROJECT_ENVIRONMENT", &env)
            .status();
    });
    let mut c = Command::new("uv");
    c.args(["run", "--no-sync", "--project"])
        .arg(dir)
        .env("UV_PROJECT_ENVIRONMENT", &env);
    c
}
