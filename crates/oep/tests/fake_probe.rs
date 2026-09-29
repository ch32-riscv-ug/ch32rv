//! en: The link and session layers against the spec side's fake probe (oep-client-python
//! `v1.endpoint.Endpoint`), served over TCP by `tests/fake/serve.py`. The fake is the shared
//! "working spec" (ArduinoCore-CH32 decision, 2026-09-29); Python runs only here, through `uv`.
//! Skipped, with a note, when uv or the client checkout is missing ($OEP_CLIENT_PYTHON, default
//! `../dev_oep/oep-client-python` next to the ch32rv checkout's parent).
//! ja: link と session を spec 側の偽の probe で試験する。uv か client が無ければ skip。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use ch32rv_oep::link::{Framing, Link};
use ch32rv_oep::registry;
use ch32rv_oep::session::{OepError, Probe, random_session_id};

struct Fake {
    child: Child,
    port: u16,
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn client_dir() -> PathBuf {
    std::env::var_os("OEP_CLIENT_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../dev_oep/oep-client-python")
        })
}

/// Start the fake, or `None` (skip) when it cannot run here.
fn fake(args: &[&str]) -> Option<Fake> {
    let dir = client_dir();
    if !dir.join("src/oep_client/v1/endpoint.py").exists() {
        eprintln!("skip: no oep-client-python at {}", dir.display());
        return None;
    }
    let serve = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fake/serve.py");
    let mut child = match Command::new("uv")
        .arg("run")
        .arg("--project")
        .arg(&dir)
        .arg("python")
        .arg(&serve)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skip: cannot run uv: {e}");
            return None;
        }
    };
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port = line
        .trim()
        .strip_prefix("PORT ")
        .unwrap_or_else(|| panic!("serve.py said {line:?}"))
        .parse()
        .unwrap();
    Some(Fake { child, port })
}

fn probe(f: &Fake, framing: Framing, timeout: Duration) -> Probe {
    let s = TcpStream::connect(("127.0.0.1", f.port)).unwrap();
    s.set_nodelay(true).unwrap();
    let mut link = Link::new(Box::new(s), framing);
    link.set_timeout(timeout);
    Probe::connect(link).unwrap()
}

#[test]
fn discovery_and_session_over_cobs_with_console_noise() {
    let Some(f) = fake(&["--framing", "cobs", "--noise", "uptime 3 s\r\n"]) else {
        return;
    };
    let mut p = probe(&f, Framing::Cobs, Duration::from_secs(2));
    let l = p.limits();
    assert_eq!(l.revision, 1);
    assert_eq!(l.max_frame, 1024);

    let all = p.list("").unwrap();
    assert_eq!(all[0].name, "oep.core");
    assert!(all.iter().any(|i| i.name == "oep.target.riscv-dm"));
    assert!(all.iter().any(|i| i.name == "oep.wire.rvswd"));
    // list matches on label boundaries (core §7.2): `oep` finds every standard interface, `oep.`
    // (a trailing dot) finds nothing, `oep.wire` finds the wires only.
    let std = p.list("oep").unwrap();
    assert!(
        std.iter()
            .all(|i| i.name == "oep" || i.name.starts_with("oep."))
    );
    assert!(std.iter().any(|i| i.name == "oep.target.riscv-dm"));
    assert!(p.list("oep.").unwrap().is_empty());
    let wires = p.list("oep.wire").unwrap();
    assert!(!wires.is_empty() && wires.iter().all(|i| i.name.starts_with("oep.wire.")));
    let dm = p.interface("oep.target.riscv-dm").unwrap();
    assert_ne!(dm.func, 0);

    let core = p.describe(registry::core::FN).unwrap();
    assert!(
        core.iter()
            .any(|t| t.tag == registry::core::tlvs::describe::UNIT_ID)
    );

    let sid = random_session_id();
    let o = p.open(sid, 3000, false).unwrap();
    assert!(!o.resumed);
    assert!(p.lock_state().unwrap().0);
    p.keepalive().unwrap();
    p.end().unwrap();
    assert!(!p.lock_state().unwrap().0);
    // The same id takes the lock back and is told so.
    assert!(p.open(sid, 3000, false).unwrap().resumed);
    p.end().unwrap();
    // Noise and the leading 0x00 of every answer were filtered, never mistaken for answers.
    assert_eq!(p.link().resyncs, 0);
}

#[test]
fn a_lost_answer_is_resent_once_with_the_same_corr_on_length_framing() {
    // The 2nd request (the first after confirm) goes unanswered once.
    let Some(f) = fake(&["--framing", "length", "--drop", "2"]) else {
        return;
    };
    let mut p = probe(&f, Framing::Length, Duration::from_millis(500));
    let all = p.list("").unwrap();
    assert!(!all.is_empty());
    assert_eq!(p.link().resyncs, 1);
}

#[test]
fn a_lost_answer_is_resent_on_cobs_without_resync() {
    let Some(f) = fake(&["--framing", "cobs", "--drop", "2"]) else {
        return;
    };
    let mut p = probe(&f, Framing::Cobs, Duration::from_millis(500));
    assert!(!p.list("").unwrap().is_empty());
    assert_eq!(p.link().resyncs, 0);
}

#[test]
fn another_session_is_locked_out_until_it_forces() {
    let Some(f) = fake(&["--framing", "cobs"]) else {
        return;
    };
    let mut p = probe(&f, Framing::Cobs, Duration::from_secs(2));
    p.open(0x1111_1111, 5000, false).unwrap();
    match p.open(0x2222_2222, 5000, false) {
        Err(OepError::Locked { remaining_ms }) => assert!(remaining_ms > 0 && remaining_ms <= 5000),
        other => panic!("expected Locked, got {other:?}"),
    }
    let o = p.open(0x2222_2222, 5000, true).unwrap();
    assert!(!o.resumed);
    p.end().unwrap();
}

#[test]
fn pipelined_requests_answer_in_order() {
    let Some(f) = fake(&["--framing", "length"]) else {
        return;
    };
    let mut p = probe(&f, Framing::Length, Duration::from_secs(2));
    p.open(random_session_id(), 3000, false).unwrap();
    // More than max_inflight (4) requests: the link must hold back and keep the order.
    let calls = (0..10)
        .map(|i| {
            let mut pl = registry::core::FN.to_le_bytes().to_vec();
            pl.extend_from_slice(&(i as u16 % 3).to_le_bytes());
            (registry::core::FN, registry::core::op::DESCRIBE, pl)
        })
        .collect();
    let r = p.exchange(calls).unwrap();
    assert_eq!(r.len(), 10);
    assert!(r.iter().all(|x| x.succeeded()));
    assert_eq!(r[0].payload, r[3].payload);
    assert_ne!(r[0].payload, r[1].payload);
    p.end().unwrap();
}

// ---- wire + riscv-dm ----

use ch32rv_dmi::{DebugModule, RegName, ResetMode, TargetAccess, resume_ch32};
use ch32rv_oep::target::{AttachOptions, OepDtm, WireKind, attach, detach};

#[test]
fn attach_reports_the_wch_chip_id_and_blocks_round_trip() {
    let Some(f) = fake(&["--framing", "cobs", "--target-id", "0x20310500"]) else {
        return;
    };
    let mut p = probe(&f, Framing::Cobs, Duration::from_secs(2));
    p.open(random_session_id(), 3000, false).unwrap();
    let a = attach(
        &mut p,
        WireKind::Rvswd,
        AttachOptions {
            halt: true,
            max_speed_hz: Some(1_000_000),
            pins: None,
        },
    )
    .unwrap();
    assert_eq!(a.wch_chip_id, Some(0x2031_0500));
    assert!(a.speed_hz <= 1_000_000);
    assert!(!a.existing);
    // Attaching again gets the same connection back, marked existing.
    let again = attach(&mut p, WireKind::Rvswd, AttachOptions::default()).unwrap();
    assert_eq!(again.connection, a.connection);
    assert!(again.existing);

    let mut t = OepDtm::new(&mut p, a.connection).unwrap();
    let max = t.max_block_words();
    assert!((1..=256).contains(&max));
    // More words than one block holds: split and pipelined, read back the same.
    let words: Vec<u32> = (0..(max as u32 * 2 + 7))
        .map(|i| i.wrapping_mul(0x9E37_79B9))
        .collect();
    t.write_words(0x2000_0000, &words).unwrap();
    assert_eq!(t.read_words(0x2000_0000, words.len()).unwrap(), words);

    // The Debug Module code runs on it unchanged: the pc through an abstract command.
    let pc = DebugModule::new(&mut t).read_reg(RegName::Pc).unwrap();
    assert_eq!(pc, 0x100);

    let r = t
        .run_until_halt(
            0x2000_0000,
            &[(ch32rv_dmi::access::regno::A0, 5)],
            &[ch32rv_dmi::access::regno::A0],
            Duration::from_millis(200),
        )
        .unwrap();
    assert!(r.stopped);
    assert_eq!(r.dpc, 0x2000_0010);
    assert_eq!(r.outs, vec![5]);

    let rr = t.reset(ResetMode::HaltAtReset).unwrap();
    assert_eq!(rr.pc, 0);
    detach(&mut p, WireKind::Rvswd, a.connection, false).unwrap();
    p.end().unwrap();
}

#[test]
fn a_resume_that_does_not_take_is_issued_again() {
    // Two misses, like a CH32V006 now and then: the CH32 rule re-issues while dpc is unmoved.
    let Some(f) = fake(&["--framing", "cobs", "--resume-misses", "2"]) else {
        return;
    };
    let mut p = probe(&f, Framing::Cobs, Duration::from_secs(2));
    p.open(random_session_id(), 3000, false).unwrap();
    let a = attach(
        &mut p,
        WireKind::Rvswd,
        AttachOptions {
            halt: true,
            ..AttachOptions::default()
        },
    )
    .unwrap();
    let mut t = OepDtm::new(&mut p, a.connection).unwrap();
    let ran = resume_ch32(&mut t, |t| DebugModule::new(t).read_reg(RegName::Pc)).unwrap();
    assert!(ran);
}
