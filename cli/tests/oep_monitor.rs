//! en: `ch32rv arduino monitor` on an OEP probe's serial port (the fake probe's pty), the test
//! playing arduino-cli: a debug source goes through the probe's broker to its console stream, a
//! wrong `chip` fails OPEN, and closing stdin ends it and its broker. The locked virtual backend is required.
//! ja: OEP の probe の serial port での `arduino monitor`(試験が arduino-cli の役)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../crates/oep/tests/virtual_bench/uv.rs"]
mod uv;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn fake_pty(args: &[&str]) -> Option<(Kill, String)> {
    let dir = uv::client_dir()?;
    let mut child = uv::uv_run(&dir)
        .args(["python", "-m", "oep_client.virtual_bench_serve", "--pty"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to launch the required virtual probe");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let pty = line.trim().strip_prefix("PTY ").unwrap().to_owned();
    Some((Kill(child), pty))
}

struct Monitor {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Monitor {
    fn start() -> Self {
        let mut child = uv::with_runtime(Command::new(env!("CARGO_BIN_EXE_ch32rv")))
            .args(["arduino", "monitor", "--protocol", "serial"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Monitor {
            child,
            stdin,
            stdout,
        }
    }

    fn cmd(&mut self, line: &str) -> serde_json::Value {
        let i = self.stdin.as_mut().unwrap();
        writeln!(i, "{line}").unwrap();
        i.flush().unwrap();
        let mut out = String::new();
        self.stdout.read_line(&mut out).unwrap();
        serde_json::from_str(&out).unwrap()
    }
}

#[test]
fn a_debug_source_streams_the_oep_console_through_the_broker() {
    let Some((_fake, pty)) = fake_pty(&[
        "--target-id",
        "0x20310500",
        "--console",
        "uptime %d\r\n",
        "--every",
        "50",
    ]) else {
        return;
    };
    let mut m = Monitor::start();
    m.cmd("HELLO 1 \"test\"");
    assert_eq!(m.cmd("CONFIGURE source dmseq")["message"], "OK");

    // A wrong board chip fails OPEN, before anything streams.
    assert_eq!(m.cmd("CONFIGURE chip CH32X035")["message"], "OK");
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let at = l.local_addr().unwrap();
    let r = m.cmd(&format!("OPEN {at} {pty}"));
    assert_eq!(r["error"], true, "{r}");
    assert!(r["message"].as_str().unwrap().contains("conflicts"), "{r}");

    assert_eq!(m.cmd("CONFIGURE chip CH32V20x")["message"], "OK");
    let t0 = Instant::now();
    let r = m.cmd(&format!("OPEN {at} {pty}"));
    assert_eq!(r["message"], "OK", "{r}");
    // Well inside arduino-cli's wait, broker start included.
    assert!(t0.elapsed() < Duration::from_secs(3));
    let (mut sock, _) = l.accept().unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 256];
    while !String::from_utf8_lossy(&got).contains("uptime") {
        let n = sock.read(&mut buf).unwrap();
        assert!(n > 0, "the monitor closed");
        got.extend_from_slice(&buf[..n]);
    }
    // No WCH-Link here: no clock caveat.
    assert!(!String::from_utf8_lossy(&got).contains("clock"));

    // Closing stdin ends the monitor, and with its only client gone, the broker.
    drop(m.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if m.child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the monitor did not end on stdin EOF"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // The broker lingers 3 s after its last client (an IDE's monitor-then-upload), then ends.
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let out = uv::with_runtime(Command::new(env!("CARGO_BIN_EXE_ch32rv")))
            .args([
                "broker",
                "endpoint",
                "--probe",
                &format!("port:{pty}"),
                "--json",
            ])
            .output()
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        if v["result"]["endpoint"].is_null() {
            break;
        }
        assert!(Instant::now() < deadline, "the broker outlived the monitor");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn fixture_uart_streams_at_the_monitors_baud() {
    let Some((_fake, pty)) = fake_pty(&["--uart-plan", "--uart-rx", "rx %d\r\n", "--every", "50"])
    else {
        return;
    };
    let mut m = Monitor::start();
    m.cmd("HELLO 1 \"test\"");
    assert_eq!(m.cmd("CONFIGURE source fixture-uart")["message"], "OK");
    assert_eq!(m.cmd("CONFIGURE baudrate 115200")["message"], "OK");
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let at = l.local_addr().unwrap();
    let r = m.cmd(&format!("OPEN {at} {pty}"));
    assert_eq!(r["message"], "OK", "{r}");
    let (mut sock, _) = l.accept().unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let read_until = |sock: &mut std::net::TcpStream, what: &str| {
        let mut got = Vec::new();
        let mut buf = [0u8; 256];
        while !String::from_utf8_lossy(&got).contains(what) {
            let n = sock.read(&mut buf).unwrap();
            assert!(n > 0, "the monitor closed");
            got.extend_from_slice(&buf[..n]);
        }
    };
    read_until(&mut sock, "rx ");
    // The baud changes while open (configure again), and the stream goes on.
    assert_eq!(m.cmd("CONFIGURE baudrate 9600")["message"], "OK");
    read_until(&mut sock, "rx ");
    // Input goes to the UART's TX without breaking the stream.
    sock.write_all(b"hello\n").unwrap();
    read_until(&mut sock, "rx ");
    drop(m.stdin.take());
    let _ = m.child.wait();
}

#[test]
fn the_cli_monitor_streams_an_oep_console() {
    let Some((_fake, pty)) = fake_pty(&[
        "--target-id",
        "0x20310500",
        "--console",
        "uptime %d\r\n",
        "--every",
        "50",
    ]) else {
        return;
    };
    let out = uv::with_runtime(Command::new(env!("CARGO_BIN_EXE_ch32rv")))
        .args([
            "monitor",
            "--source",
            "dmseq",
            "--probe",
            &format!("port:{pty}"),
            "--duration",
            "1",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("uptime"));
}

#[test]
fn uart_on_an_oep_probe_says_so_and_rtt_looks_for_the_block() {
    // "uart" is not an OEP probe's source: capability-unsupported (24) with what to use, not
    // device-not-found from a WCH-Link lookup. rtt is run by ch32rv over the probe's riscv-dm;
    // the fake target's RAM holds no control block, so it says that.
    let Some((_fake, pty)) = fake_pty(&["--target-id", "0x20310500"]) else {
        return;
    };
    let run = |source: &str| {
        let out = uv::with_runtime(Command::new(env!("CARGO_BIN_EXE_ch32rv")))
            .args(["monitor", "--source", source, "--duration", "1"])
            .args(["--probe", &format!("port:{pty}"), "--json"])
            .output()
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        (out.status.code(), v)
    };
    let (code, v) = run("uart");
    assert_eq!(code, Some(24), "{v}");
    assert_eq!(v["error"]["kind"], "capability-unsupported", "{v}");
    let (_, v) = run("rtt");
    assert!(
        v["error"]["msg"]
            .as_str()
            .unwrap_or("")
            .contains("RTT control block"),
        "{v}"
    );
}

#[test]
fn describe_lists_every_chip_name_configure_takes() {
    // docs/freeze-decisions.ja.md §1: family, series and SKU, plus `auto`.
    let mut m = Monitor::start();
    m.cmd("HELLO 1 \"test\"");
    let d = m.cmd("DESCRIBE");
    let values: Vec<&str> = d["port_description"]["configuration_parameters"]["chip"]["value"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    for name in ["auto", "CH32V20x", "CH32V203", "CH32V203C8T6"] {
        assert!(values.contains(&name), "{name} not in {values:?}");
        assert_eq!(m.cmd(&format!("CONFIGURE chip {name}"))["message"], "OK");
    }
}

#[test]
fn the_monitor_attaches_again_after_the_probe_loses_the_line() {
    // P1 (oep-if-debug §2): a probe no longer wakes a live line on its own; it closes the
    // connection and its console stream (mark detail 4). The monitor sees the stream closed when
    // it goes quiet, attaches again and reopens the console, and the output goes on. The fake's
    // `lose` line plays the lost line.
    let Some((mut fake, pty)) = fake_pty(&[
        "--target-id",
        "0x20310500",
        "--console",
        "uptime %d\r\n",
        "--every",
        "50",
    ]) else {
        return;
    };
    let mut m = Monitor::start();
    m.cmd("HELLO 1 \"test\"");
    assert_eq!(m.cmd("CONFIGURE source dmseq")["message"], "OK");
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let at = l.local_addr().unwrap();
    let r = m.cmd(&format!("OPEN {at} {pty}"));
    assert_eq!(r["message"], "OK", "{r}");
    let (mut sock, _) = l.accept().unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let mut buf = [0u8; 512];
    let mut read_for = |sock: &mut std::net::TcpStream, d: Duration| {
        let mut got = Vec::new();
        let end = Instant::now() + d;
        while Instant::now() < end {
            if let Ok(n) = sock.read(&mut buf) {
                got.extend_from_slice(&buf[..n]);
            }
        }
        String::from_utf8_lossy(&got).into_owned()
    };
    assert!(read_for(&mut sock, Duration::from_secs(1)).contains("uptime"));
    {
        let i = fake.0.stdin.as_mut().unwrap();
        writeln!(i, "lose").unwrap();
        i.flush().unwrap();
    }
    // The stream closed; once quiet for a second the monitor finds it and attaches again.
    let _ = read_for(&mut sock, Duration::from_millis(1500));
    let after = read_for(&mut sock, Duration::from_secs(3));
    assert!(
        after.contains("uptime"),
        "no output after the lost line: {after:?}"
    );
    drop(m.stdin.take());
}

#[test]
fn plain_monitor_on_an_oep_probe_reads_the_targets_console() {
    // `ch32rv monitor` with no --source on an OEP probe: the target's console by the slot's
    // mechanism, else dmseq (on a WCH-Link the default stays uart).
    let Some((_fake, pty)) = fake_pty(&[
        "--target-id",
        "0x20310500",
        "--console",
        "uptime %d\r\n",
        "--every",
        "50",
    ]) else {
        return;
    };
    let out = uv::with_runtime(std::process::Command::new(env!("CARGO_BIN_EXE_ch32rv")))
        .args([
            "monitor",
            "--probe",
            &format!("port:{pty}"),
            "--duration",
            "2",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stdout.contains("uptime"), "{stdout:?} / {stderr}");
    assert!(stderr.contains("monitor: dm"), "{stderr}");
}

#[test]
fn the_arduino_monitor_opens_a_tcp_port() {
    // An IDE's port address `tcp:<host>:<port>` (a probe on Wi-Fi): OPEN resolves it as --probe
    // does and meets the probe's broker (keyed by the unit id the probe describes), and the
    // console streams. The bench saw OPEN wait for a broker under another key (2026-10-07).
    let Some(dir) = uv::client_dir() else {
        return;
    };
    let unit = format!("{:08x}cafe", std::process::id());
    let mut child = uv::uv_run(&dir)
        .args([
            "python",
            "-m",
            "oep_client.virtual_bench_serve",
            "--tcp",
            "0",
        ])
        .args(["--unit-id", &unit])
        .args(["--target-id", "0x20310500"])
        .args(["--console", "uptime %d\r\n", "--every", "50"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    // Kept open: the virtual bench writes on, and a closed pipe would end it.
    let mut out = BufReader::new(child.stdout.take().unwrap());
    out.read_line(&mut line).unwrap();
    let _bench = Kill(child);
    let port = line.trim().strip_prefix("PORT ").unwrap().to_owned();
    for address in [
        format!("tcp:127.0.0.1:{port}"),
        format!("tcp://127.0.0.1:{port}"),
    ] {
        let mut m = Monitor::start();
        m.cmd("HELLO 1 \"test\"");
        assert_eq!(m.cmd("CONFIGURE source dmseq")["message"], "OK");
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = l.local_addr().unwrap();
        let r = m.cmd(&format!("OPEN {at} {address}"));
        assert_eq!(r["message"], "OK", "{address}: {r}");
        let (mut sock, _) = l.accept().unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 256];
        while !String::from_utf8_lossy(&got).contains("uptime") {
            let n = sock.read(&mut buf).unwrap();
            assert!(n > 0, "the monitor closed");
            got.extend_from_slice(&buf[..n]);
        }
        drop(m.stdin.take());
        let _ = m.child.wait();
    }
    drop(out);
}
