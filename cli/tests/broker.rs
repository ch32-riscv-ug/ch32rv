//! en: The per-probe broker (docs/oep-host.ja.md §7.2) on the spec side's fake probe (pty): two
//! clients at once, each with its own corr numbering; a connection both use survives one client's
//! detach and is detached on the probe only by its last user; the broker ends, and its endpoint
//! goes, once the last client has left. Skipped when uv or the client checkout is missing.
//! ja: ブローカーを偽の probe(pty)で試験する。2 つの client、共有の接続の detach、最後の client で終わる。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../crates/oep/tests/virtual_bench/uv.rs"]
mod uv;
use std::io::{BufRead, BufReader};
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

/// The fake on a pty: (process, pty path).
fn fake_pty() -> Option<(Kill, String)> {
    let dir = uv::client_dir()?;
    let mut child = uv::uv_run(&dir)
        .args(["python", "-m", "oep_client.virtual_bench_serve", "--pty"])
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
    uv::with_runtime(Command::new(env!("CARGO_BIN_EXE_ch32rv")))
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
        uv::with_runtime(Command::new(env!("CARGO_BIN_EXE_ch32rv")))
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

#[test]
fn the_broker_opens_a_new_session_after_the_probe_restarts() {
    // oep-client-python's virtual_bench_serve restarts on a `reboot` line on its stdin (new boot_id, every
    // session forgotten). The broker's next upstream request gets no_session: it passes that on,
    // opens a new session, and the client's next attach goes through (CHANGELOG, 2026-10-06).
    let Some(dir) = uv::client_dir() else {
        return;
    };
    let mut child = uv::uv_run(&dir)
        .args(["python", "-m", "oep_client.virtual_bench_serve", "--pty"])
        .args(["--target-id", "0x20310500"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut fake_in = child.stdin.take().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let pty = line.trim().strip_prefix("PTY ").unwrap().to_owned();
    let (said, heard) = std::sync::mpsc::channel::<String>();
    let err = child.stderr.take().unwrap();
    std::thread::spawn(move || {
        for l in BufReader::new(err).lines().map_while(Result::ok) {
            let _ = said.send(l);
        }
    });
    let _fake = Kill(child);
    let _broker = Kill(
        uv::with_runtime(Command::new(env!("CARGO_BIN_EXE_ch32rv")))
            .args(["broker", "serve", "--probe", &format!("port:{pty}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
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
    let opt = AttachOptions {
        halt: true,
        ..AttachOptions::default()
    };
    let conn = attach(&mut a, WireKind::Rvswd, opt).unwrap().connection;
    halted(&mut a, conn).unwrap();

    // The probe restarts under the broker.
    use std::io::Write;
    writeln!(fake_in, "reboot").unwrap();
    fake_in.flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let l = heard
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("virtual_bench_serve did not say it rebooted");
        if l.contains("rebooted") {
            break;
        }
    }

    // The old connection is gone with the restart, and so is the client's session (core §9:
    // the request meets no_session, from the probe or the broker) ...
    assert!(halted(&mut a, conn).is_err());
    // ... and the broker is still there, with a new session: the client opens again and a fresh
    // attach works.
    let deadline = Instant::now() + Duration::from_secs(5);
    let again = loop {
        let opened = a.open(
            ch32rv_oep::session::random_session_id(),
            3000,
            false,
            Some("test"),
        );
        match opened.and_then(|_| attach(&mut a, WireKind::Rvswd, opt)) {
            Ok(at) => break at,
            Err(e) => {
                assert!(
                    Instant::now() < deadline,
                    "no attach after the restart: {e}"
                );
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    };
    halted(&mut a, again.connection).unwrap();
}

#[test]
fn a_client_restarts_the_probe_through_the_broker() {
    // oep.probe.restart's restart from a client (oep-if-restart §3): the broker forwards it under
    // its own session, passes the success answer on, then closes its transport to the probe and
    // ends (transports §1). The client waits for the probe and starts over: a new broker finds
    // the probe with a new boot_id, and attach works there.
    let Some((_fake, pty)) = fake_pty() else {
        return;
    };
    let start = || {
        Kill(
            uv::with_runtime(Command::new(env!("CARGO_BIN_EXE_ch32rv")))
                .args(["broker", "serve", "--probe", &format!("port:{pty}")])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        )
    };
    let wait_endpoint = || {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(ep) = endpoint(&pty) {
                break ep;
            }
            assert!(
                Instant::now() < deadline,
                "the broker published no endpoint"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let mut first = start();
    let mut a = client(&wait_endpoint());
    let opt = AttachOptions {
        halt: true,
        ..AttachOptions::default()
    };
    attach(&mut a, WireKind::Rvswd, opt).unwrap();
    let before = a.limits().boot_id;
    let wait = a
        .restart_max_ms()
        .unwrap()
        .expect("the fake offers oep.probe.restart");
    a.request_restart().unwrap();
    // The broker ends on its own after passing the answer on.
    let deadline = Instant::now() + Duration::from_secs(5);
    while first.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the broker did not end");
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(a);
    std::thread::sleep(Duration::from_millis(u64::from(wait)));
    let _second = start();
    let mut b = client(&wait_endpoint());
    assert_ne!(b.limits().boot_id, before, "the boot_id did not change");
    let again = attach(&mut b, WireKind::Rvswd, opt).unwrap();
    halted(&mut b, again.connection).unwrap();
}

#[test]
fn a_probe_announced_by_dns_sd_is_found_and_used_by_its_unit_id() {
    // virtual_bench_serve --tcp --announce answers `_oep._tcp` DNS-SD on this host (oep-spec
    // transports §3). `probe list` lists it under `network`, and `--probe tcp:<unit_id>` finds it,
    // opens it over TCP and checks describe's unit_id.
    let Some(dir) = uv::client_dir() else {
        return;
    };
    let unit = format!("{:012x}", u64::from(std::process::id()) << 8 | 0xab);
    let mut child = uv::uv_run(&dir)
        .args([
            "python",
            "-m",
            "oep_client.virtual_bench_serve",
            "--tcp",
            "0",
        ])
        .args(["--announce", "--profile", "esp32-v003", "--unit-id", &unit])
        .args(["--target-id", "0x00300500"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let _bench = Kill(child);
    let port = line.trim().strip_prefix("PORT ").unwrap().to_owned();

    let out = ch32rv(&["probe", "list", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let net = v["result"]["network"].as_array().unwrap();
    let Some(found) = net.iter().find(|n| n["unit_id"] == unit.as_str()) else {
        // A host with no multicast route at all (a bare container): nothing to test here.
        eprintln!("skip: no DNS-SD answer on this host ({net:?})");
        return;
    };
    assert!(
        found["tcp"]
            .as_str()
            .unwrap()
            .ends_with(&format!(":{port}")),
        "{found}"
    );

    // The unit id, and the URL forms the other OEP clients take (also behind `port:`, an IDE's
    // port address).
    // By address first (a broker keyed by the address), then by unit id (one keyed by it, as a USB
    // probe of that unit id would be), whose broker `broker endpoint` names below.
    for probe in [
        format!("tcp://127.0.0.1:{port}"),
        format!("port:tcp://127.0.0.1:{port}"),
        format!("tcp:{unit}"),
        format!("tcp://{unit}"),
    ] {
        let out = ch32rv(&["target", "info", "--probe", &probe, "--json"]);
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["ok"], true, "{probe}: {v}");
        assert_eq!(
            v["result"]["target"]["chip_id"], "0x00300500",
            "{probe}: {v}"
        );
    }
    // A probe on TCP is used through its broker like one on a serial port: the broker the
    // commands started is still lingering, and `broker endpoint` names it.
    let out = ch32rv(&[
        "broker",
        "endpoint",
        "--probe",
        &format!("tcp:{unit}"),
        "--json",
    ]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let by_unit = v["result"]["endpoint"].as_str().map(str::to_owned);
    assert!(by_unit.is_some(), "{v}");
    // Named by address it is the same broker: the address's probe says its unit id, and the
    // broker is keyed by it (one owner per probe on this host, also where mDNS does not reach).
    let out = ch32rv(&[
        "broker",
        "endpoint",
        "--probe",
        &format!("tcp:127.0.0.1:{port}"),
        "--json",
    ]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["result"]["endpoint"].as_str().map(str::to_owned),
        by_unit,
        "{v}"
    );
}
