//! An isolated, locked Python virtual probe for the Rust tests. Missing dependencies fail
//! preparation; tests must not return successfully without exercising OEP. An explicitly
//! selected OEP_CLIENT_PYTHON checkout overrides only this test backend, never physical firmware.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// oep-client-python on oep-spec f8bb2de and after: the virtual bench, with `lose`, `reboot` and
/// `--announce` (DNS-SD).
const BENCH_REV: &str = "068c6bed7571d637a5b3843ab27d58c50efcf589";

fn target_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    exe.ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "target"))
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir)
}

fn venv(dir: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    dir.canonicalize()
        .expect("configured virtual project exists")
        .hash(&mut hash);
    target_dir().join(format!(
        "oep-client-venv-{BENCH_REV}-{:016x}",
        hash.finish()
    ))
}

fn workspace_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|p| p.join("Cargo.lock").exists())
        .expect("test workspace root")
        .join("tests/oep-virtual")
}

fn configured_dir(explicit: Option<PathBuf>, workspace: PathBuf) -> PathBuf {
    let dir = explicit.unwrap_or(workspace);
    assert!(
        dir.join("pyproject.toml").is_file() && dir.join("uv.lock").is_file(),
        "virtual probe requires an explicit locked Python project: {}",
        dir.display()
    );
    dir
}

/// Always prepares a configured backend. None is retained only for existing caller signatures;
/// a missing or malformed project is a preparation failure, never a successful test skip.
pub fn client_dir() -> Option<PathBuf> {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    Some(
        DIR.get_or_init(|| {
            configured_dir(
                std::env::var_os("OEP_CLIENT_PYTHON").map(PathBuf::from),
                workspace_dir(),
            )
        })
        .clone(),
    )
}

/// Coordinate installation between test processes. A timeout fails preparation and does not
/// unlink a lock another live process might still own.
struct Lock(PathBuf);

impl Lock {
    fn take(path: &Path) -> Self {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let started = std::time::Instant::now();
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(_) => return Self(path.to_path_buf()),
                Err(error) => {
                    assert_eq!(
                        error.kind(),
                        std::io::ErrorKind::AlreadyExists,
                        "cannot create virtual environment lock: {error}"
                    );
                    assert!(
                        started.elapsed().as_secs() < 180,
                        "virtual environment lock is busy: {}",
                        path.display()
                    );
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
    let env = venv(dir);
    SYNCED.get_or_init(|| {
        let _guard = Lock::take(&env.with_extension("lock"));
        let status = Command::new("uv")
            .args(["sync", "--locked", "--quiet", "--project"])
            .arg(dir)
            .env("UV_PROJECT_ENVIRONMENT", &env)
            .status()
            .expect("uv is required for OEP virtual integration tests");
        assert!(status.success(), "locked virtual probe installation failed");
    });
    let mut c = Command::new("uv");
    c.args(["run", "--locked", "--no-sync", "--project"])
        .arg(dir)
        .env("UV_PROJECT_ENVIRONMENT", &env);
    c
}

/// en: A runtime directory of this test process's own (`XDG_RUNTIME_DIR` for the ch32rv it starts,
/// and the brokers that one starts): the brokers, endpoint and session files of the virtual
/// probes stay out of the user's, and go with `target/`. Directories older than an hour (earlier
/// runs) are removed the first time.
/// ja: この試験プロセス専用の runtime の場所。virtual probe のブローカーのファイルを利用者の場所に残さない。
#[allow(dead_code)]
pub fn test_runtime_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let base = target_dir().join("ch32rv-test-runtime");
        if let Ok(old) = std::fs::read_dir(&base) {
            for e in old.flatten() {
                let stale = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|a| a.as_secs() > 3600);
                if stale {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        let d = base.join(std::process::id().to_string());
        let _ = std::fs::create_dir_all(&d);
        d
    })
    .clone()
}

/// `c` with this test process's runtime directory (see [`test_runtime_dir`]).
#[allow(dead_code)]
pub fn with_runtime(mut c: Command) -> Command {
    // ch32rv uses `$XDG_RUNTIME_DIR/ch32rv`.
    c.env("XDG_RUNTIME_DIR", test_runtime_dir());
    c
}

#[cfg(test)]
mod setup_tests {
    use super::*;

    #[test]
    fn default_is_the_consumers_locked_workspace() {
        let dir = workspace_dir();
        assert_eq!(configured_dir(None, dir.clone()), dir);
    }

    #[test]
    fn invalid_explicit_project_never_falls_back() {
        let missing =
            std::env::temp_dir().join(format!("missing-oep-project-{}", std::process::id()));
        assert!(
            std::panic::catch_unwind(|| configured_dir(Some(missing), workspace_dir())).is_err()
        );
    }
}
