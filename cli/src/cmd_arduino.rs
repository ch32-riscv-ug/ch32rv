//! en: `arduino discovery` / `arduino monitor` (docs/cli.ja.md §4.11): the Arduino Pluggable
//! Discovery and Monitor protocols (line-based stdio JSON). Discovery exposes each WCH probe as a
//! `wchlink://<serial>` port; Monitor wraps a `ch32rv monitor` source (dmdata/sdi/uart) as the
//! IDE's Serial Monitor. Machine-facing: never mixes human text onto stdout.
//! ja: `arduino discovery`/`monitor`。Arduino の Pluggable Discovery/Monitor プロトコル(行単位の
//! stdio JSON)。discovery は各 WCH probe を `wchlink://<serial>` port として公開、monitor は
//! `ch32rv monitor` の source を IDE の Serial Monitor として wrap する。

use std::io::{BufRead, Write};
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::args::Cli;

/// `arduino discovery`: the Pluggable Discovery protocol over stdio.
pub fn discovery(_cli: &Cli) -> ExitCode {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let word = line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        match word.as_str() {
            "" => {}
            "HELLO" => emit(
                &mut out,
                &json!({"eventType":"hello","protocolVersion":1,"message":"OK"}),
            ),
            "START" => emit(&mut out, &json!({"eventType":"start","message":"OK"})),
            "STOP" => emit(&mut out, &json!({"eventType":"stop","message":"OK"})),
            "LIST" => emit(&mut out, &json!({"eventType":"list","ports": list_ports()})),
            "START_SYNC" => {
                // We do not watch USB hotplug yet: acknowledge, emit the current ports once as
                // `add` events, then rely on the IDE's periodic re-LIST for changes.
                emit(&mut out, &json!({"eventType":"start_sync","message":"OK"}));
                for p in list_ports() {
                    emit(&mut out, &json!({"eventType":"add","port": p}));
                }
            }
            "QUIT" => {
                emit(&mut out, &json!({"eventType":"quit","message":"OK"}));
                return ExitCode::SUCCESS;
            }
            other => emit(
                &mut out,
                &json!({"eventType": other.to_ascii_lowercase(), "error": true, "message": format!("unknown command {other}")}),
            ),
        }
    }
    ExitCode::SUCCESS
}

/// Enumerate WCH probes as Pluggable-Discovery ports (from USB descriptors only - no AttachChip,
/// so it never disturbs an in-flight upload/monitor on the same probe).
fn list_ports() -> Vec<Value> {
    let entries = crate::cmd_probe::wch_devices().unwrap_or_default();
    entries
        .iter()
        .map(|e| {
            let serial = e.dev.serial().unwrap_or("unknown");
            json!({
                "address": format!("wchlink://{serial}"),
                "label": format!("WCH-Link {serial}"),
                "protocol": "wchlink",
                "protocolLabel": "WCH-Link (RISC-V debug)",
                "hardwareId": serial,
                "properties": {
                    "serial": serial,
                    "vid": format!("0x{:04x}", e.dev.vid()),
                    "pid": format!("0x{:04x}", e.dev.pid()),
                    "mode": crate::cmd_probe::mode_str(e.mode),
                },
            })
        })
        .collect()
}

fn emit(out: &mut impl Write, v: &Value) {
    // One compact JSON object per line; flush so the IDE sees it immediately.
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

// ---- arduino monitor (Pluggable Monitor protocol) ----

use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// `arduino monitor`: the Pluggable Monitor protocol over stdio. OPEN connects (TCP client) to the
/// IDE-provided address and pipes the target's runtime output to it. `source` (dmdata/uart/sdi/rtt)
/// selects the backend - dmdata (probe-agnostic DMI mailbox) is the default and the wired one.
pub fn monitor(_cli: &Cli) -> ExitCode {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let mut source = "dmdata".to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let mut stream: Option<JoinHandle<()>> = None;

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let parts: Vec<&str> = line.split_whitespace().collect();
        let word = parts.first().copied().unwrap_or("").to_ascii_uppercase();
        match word.as_str() {
            "" => {}
            "HELLO" => emit(
                &mut out,
                &json!({"eventType":"hello","protocolVersion":1,"message":"OK"}),
            ),
            "DESCRIBE" => emit(
                &mut out,
                &json!({
                    "eventType":"describe","message":"OK",
                    "port_descriptor": {
                        "protocol":"wchlink",
                        "configuration_parameters": {
                            "source": {
                                "label":"Runtime output source","type":"enum",
                                "values":["dmdata","uart","sdi","rtt"],"selected":"dmdata"
                            }
                        }
                    }
                }),
            ),
            "CONFIGURE" => {
                if parts.len() >= 3 && parts[1] == "source" {
                    source = parts[2].to_string();
                }
                emit(&mut out, &json!({"eventType":"configure","message":"OK"}));
            }
            "OPEN" => {
                // OPEN <client-host:port> <port-address>
                let (Some(&client), Some(&port)) = (parts.get(1), parts.get(2)) else {
                    emit(
                        &mut out,
                        &json!({"eventType":"open","error":true,"message":"OPEN needs <host:port> <port>"}),
                    );
                    continue;
                };
                if source != "dmdata" {
                    emit(
                        &mut out,
                        &json!({"eventType":"open","error":true,
                        "message":format!("source {source:?} is not wired yet; CONFIGURE source dmdata")}),
                    );
                    continue;
                }
                let serial = port.strip_prefix("wchlink://").unwrap_or(port).to_string();
                match TcpStream::connect(client) {
                    Ok(sock) => {
                        stop.store(false, Ordering::SeqCst);
                        let stop2 = stop.clone();
                        stream = Some(std::thread::spawn(move || {
                            pipe_dmdata(&serial, sock, stop2)
                        }));
                        emit(&mut out, &json!({"eventType":"open","message":"OK"}));
                    }
                    Err(e) => emit(
                        &mut out,
                        &json!({"eventType":"open","error":true,
                        "message":format!("connect {client}: {e}")}),
                    ),
                }
            }
            "CLOSE" => {
                stop.store(true, Ordering::SeqCst);
                if let Some(h) = stream.take() {
                    let _ = h.join();
                }
                emit(&mut out, &json!({"eventType":"close","message":"OK"}));
            }
            "QUIT" => {
                stop.store(true, Ordering::SeqCst);
                if let Some(h) = stream.take() {
                    let _ = h.join();
                }
                emit(&mut out, &json!({"eventType":"quit","message":"OK"}));
                return ExitCode::SUCCESS;
            }
            other => emit(
                &mut out,
                &json!({
                "eventType": other.to_ascii_lowercase(),"error":true,
                "message":format!("unknown command {other}")}),
            ),
        }
    }
    ExitCode::SUCCESS
}

/// Attach to the probe with `serial`, resume the core, and pipe its SerialDMDATA output to `sock`
/// until `stop` is set or the socket errors.
fn pipe_dmdata(serial: &str, mut sock: TcpStream, stop: Arc<AtomicBool>) {
    let Ok(entries) = crate::cmd_probe::wch_devices() else {
        return;
    };
    let Some(entry) = entries.into_iter().find(|e| e.dev.serial() == Some(serial)) else {
        return;
    };
    let mut warnings = Vec::new();
    let mut session = match crate::session::Session::attach(
        &entry,
        ch32rv_wchlink::Speed::High,
        Duration::from_millis(1000),
        Duration::from_secs(10),
        None,
        &mut warnings,
    ) {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut dm = session.dm();
    let _ = dm.resume(); // the mailbox only moves while the core runs
    while !stop.load(Ordering::SeqCst) {
        match dm.dmdata_poll(&[]) {
            Ok(Some(bytes)) if !bytes.is_empty() => {
                if sock.write_all(&bytes).is_err() {
                    break;
                }
                let _ = sock.flush();
            }
            Ok(_) => std::thread::sleep(Duration::from_millis(2)),
            Err(_) => break,
        }
    }
}
