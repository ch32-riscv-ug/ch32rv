//! en: `uv run` of oep-client-python for ch32rv's tests, in ch32rv's own environment
//! (`target/oep-client-venv`), synced once - under a file lock, so the test processes running in
//! parallel never install into it at the same time (they did into the client's shared `.venv`, and
//! a fake that died while uv reinstalled failed the test, 2026-10-01).
//! ja: テスト用の oep-client-python の `uv run`。ch32rv 専用の環境で、file lock の下で 1 回だけ sync
//! する(並行するテストが同時に install しないように)。
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn venv() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let target = exe
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "target"))
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    target.join("oep-client-venv")
}

pub fn uv_run(dir: &Path) -> Command {
    static SYNCED: OnceLock<()> = OnceLock::new();
    let env = venv();
    SYNCED.get_or_init(|| {
        if let Some(parent) = env.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // A lock file made with create_new (MSRV 1.88 has no File::lock); a stale one (a test
        // process killed while it held it) is taken over after 2 minutes.
        let lock = env.with_extension("lock");
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock)
            {
                Ok(_) => break,
                Err(_) => {
                    let stale = std::fs::metadata(&lock)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age.as_secs() > 120);
                    if stale {
                        let _ = std::fs::remove_file(&lock);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
        let _ = Command::new("uv")
            .args(["sync", "--quiet", "--project"])
            .arg(dir)
            .env("UV_PROJECT_ENVIRONMENT", &env)
            .status();
        let _ = std::fs::remove_file(&lock);
    });
    let mut c = Command::new("uv");
    c.args(["run", "--no-sync", "--project"])
        .arg(dir)
        .env("UV_PROJECT_ENVIRONMENT", &env);
    c
}
