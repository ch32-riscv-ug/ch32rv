//! en: The per-probe broker (docs/oep-host.ja.md §7.2) on the spec side's fake probe (pty): two
//! clients at once, each with its own corr numbering; a connection both use survives one client's
//! detach and is detached on the probe only by its last user; the broker ends, and its endpoint
//! goes, once the last client has left. Skipped when uv or the client checkout is missing.
//! ja: ブローカーを偽の probe(pty)で試験する。2 つの client、共有の接続の detach、最後の client で終わる。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../crates/oep/tests/fake/uv.rs"]
mod uv;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ch32rv_dmi::TargetAccess;
use ch32rv_oep::link::open_tcp;
use ch32rv_oep::session::Probe;
use ch32rv_oep::target::{AttachOptions, OepDtm, WireKind, attach, detach};

struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The fake on a pty: (process, pty path).
fn fake_pty() -> Option<(Kill, String)> {
    let dir = std::env::var_os("OEP_CLIENT_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("../../dev_oep/oep-client-python"));
    if !dir.join("src/oep_client/fake_serve.py").exists() {
        eprintln!(
            "skip: no oep-client-python with fake_serve at {}",
            dir.display()
        );
        return None;
    }
    let mut child = uv::uv_run(&dir)
        .args(["python", "-m", "oep_client.fake_serve", "--pty"])
        .args(["--target-id", "0x20310500"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let pty = line.trim().strip_prefix("PTY ").unwrap().to_owned();
    Some((Kill(child), pty))
}

fn ch32rv(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ch32rv"))
        .args(args)
        .output()
        .unwrap()
}

/// `broker endpoint --json`'s endpoint, or None.
fn endpoint(pty: &str) -> Option<String> {
    let out = ch32rv(&[
        "broker",
        "endpoint",
        "--probe",
        &format!("port:{pty}"),
        "--json",
    ]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    v["result"]["endpoint"].as_str().map(str::to_owned)
}

fn client(ep: &str) -> Probe {
    let mut p = Probe::connect(open_tcp(ep).unwrap()).unwrap();
    p.open(
        ch32rv_oep::session::random_session_id(),
        3000,
        false,
        Some("test"),
    )
    .unwrap();
    p
}

fn halted(p: &mut Probe, conn: u16) -> Result<(), String> {
    let mut t = OepDtm::new(p, conn).map_err(|e| e.to_string())?;
    t.halt().map_err(|e| e.to_string())
}

#[test]
fn two_clients_share_one_broker_and_it_ends_with_the_last() {
    let Some((_fake, pty)) = fake_pty() else {
        return;
    };
    // Start the broker the way a client does, and wait for its endpoint.
    let _broker = Kill(
        Command::new(env!("CARGO_BIN_EXE_ch32rv"))
            .args(["broker", "serve", "--probe", &format!("port:{pty}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let ep = loop {
        if let Some(ep) = endpoint(&pty) {
            break ep;
        }
        assert!(
            Instant::now() < deadline,
            "the broker published no endpoint"
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    let mut a = client(&ep);
    let mut b = client(&ep);
    let opt = AttachOptions {
        halt: true,
        ..AttachOptions::default()
    };
    let ca = attach(&mut a, WireKind::Rvswd, opt).unwrap();
    let cb = attach(&mut b, WireKind::Rvswd, opt).unwrap();
    assert_eq!(ca.connection, cb.connection);
    let conn = ca.connection;

    // A lets go: B's use keeps the connection on the probe.
    detach(&mut a, WireKind::Rvswd, conn, false).unwrap();
    halted(&mut b, conn).unwrap();
    drop(a);

    // B, the last user, detaches: now the probe closes it.
    detach(&mut b, WireKind::Rvswd, conn, false).unwrap();
    let err = halted(&mut b, conn).unwrap_err();
    assert!(err.contains("0x0a") || err.contains("reason 0x0a"), "{err}");
    drop(b);

    // The last client left: after its linger (3 s, for an IDE's monitor-then-upload) the broker
    // ends and takes its endpoint with it. `broker endpoint` only connects, so it does not keep
    // the broker up.
    let deadline = Instant::now() + Duration::from_secs(6);
    while endpoint(&pty).is_some() {
        assert!(
            Instant::now() < deadline,
            "the broker outlived its last client"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
