//! en: `port_speed` (oep-core §3.5): raise the serial link to a UART-bridge probe above its boot
//! speed for a long session (the broker), the way the reference client's `raise_speed` does: try
//! (answered at the speed now) -> switch the host side, 20 ms, confirm -> verify both ways with
//! max_frame-sized frames (link_source / link_sink), pipelined first, then one at a time when that
//! broke -> commit at the new speed when nothing broke; else revert and back to the boot speed.
//! Every trial is reported (what passed, KB/s each way, how many requests in flight, how long), for
//! the broker's log: which rates to try and whether to remember them is decided from what is seen.
//! ja: `port_speed`。UART bridge の probe との serial を、長い session(ブローカー)の間だけ起動時の
//! 速さより上げる。参照 client の `raise_speed` と同じ手順。試した結果はすべて返す(ブローカーの log 用)。

use std::time::{Duration, Instant};

use crate::link::{Call, LinkError};
use crate::registry::{core, outcomes, reject_reasons};
use crate::session::{OepError, Probe};

/// One rate tried.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpeedTrial {
    pub rate: u32,
    /// What the probe said it sets (its UART's nearest), when it took the try.
    pub actual: Option<u32>,
    pub committed: bool,
    /// Requests in flight the verify passed with (0: it did not pass).
    pub inflight: usize,
    pub broken_in: u32,
    pub broken_out: u32,
    /// Frames broken while both ways ran at once.
    pub broken_both: u32,
    pub in_kb_s: f64,
    pub out_kb_s: f64,
    /// Both ways at once, in and out together.
    pub both_kb_s: f64,
    pub elapsed: Duration,
    /// Why it was not used.
    pub why: Option<String>,
}

/// What [`raise_speed`] did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpeedReport {
    /// The probe takes port_speed (describe 0x4E) on a UART bridge this link is.
    pub supported: bool,
    /// The speed the link runs at afterwards.
    pub rate: u32,
    pub trials: Vec<SpeedTrial>,
    pub elapsed: Duration,
    /// The UART bridge's transport index port_speed was sent for (for [`revert`]).
    pub port: Option<u8>,
    /// Why nothing was tried.
    pub why: Option<String>,
}

impl SpeedReport {
    /// One line for a log.
    pub fn summary(&self) -> String {
        if !self.supported || self.trials.is_empty() {
            return format!(
                "port_speed: not tried ({})",
                self.why.as_deref().unwrap_or("nothing to try")
            );
        }
        let trials: Vec<String> = self
            .trials
            .iter()
            .map(|t| {
                let head = format!(
                    "{}{}",
                    t.rate,
                    t.actual
                        .filter(|&a| a != t.rate)
                        .map(|a| format!("(={a})"))
                        .unwrap_or_default()
                );
                if t.committed {
                    format!(
                        "{head} ok in {:.1} / out {:.1} / both {:.1} KB/s inflight {} broken {}/{}/{} {} ms",
                        t.in_kb_s,
                        t.out_kb_s,
                        t.both_kb_s,
                        t.inflight,
                        t.broken_in,
                        t.broken_out,
                        t.broken_both,
                        t.elapsed.as_millis()
                    )
                } else {
                    format!(
                        "{head} no ({}; broken {}/{}/{}) {} ms",
                        t.why.as_deref().unwrap_or("?"),
                        t.broken_in,
                        t.broken_out,
                        t.broken_both,
                        t.elapsed.as_millis()
                    )
                }
            })
            .collect();
        format!(
            "port_speed: at {} after {} ms: {}",
            self.rate,
            self.elapsed.as_millis(),
            trials.join("; ")
        )
    }
}

/// The broker's candidates, in order (the user's choice for now, 2026-10-01: decided again once
/// the logs show what passes on which bridge).
pub const DEFAULT_RATES: &[u32] = &[921_600, 750_000, 500_000];

const STEP_TRY: u8 = 0x00;
const STEP_COMMIT: u8 = 0x01;
const STEP_REVERT: u8 = 0x02;
/// How long the probe waits for the commit after a try: the verify (1 s) and some.
const VERIFY_MS: u16 = 2500;
const VERIFY_TIME: Duration = Duration::from_secs(1);
/// Once committed, the probe goes back after this long with no good frame: the most the spec
/// allows (oep-core §3.5), against a host that died; the broker's 1 s keepalive keeps it up.
const IDLE_MS: u32 = crate::registry::timing::PORT_SPEED_IDLE_MAX_MS;
const VERIFY_BYTES: usize = 32 * 1024;
/// What the verify's frames leave of max_frame: link_source / link_sink carry max_frame − 16
/// bytes (oep-core §3.5, room for the headers and the frame's own bytes).
const VERIFY_ROOM: usize = 16;

/// The UART bridge's transport index, when the probe declares port_speed; else why not.
fn speed_port(p: &mut Probe) -> Result<u8, String> {
    let tlvs = p.describe(core::FN).map_err(|e| e.to_string())?;
    if !tlvs
        .iter()
        .any(|t| t.tag == core::tlvs::describe::PORT_SPEED && t.value.first() == Some(&1))
    {
        return Err("the probe does not declare port_speed".into());
    }
    tlvs.iter()
        .filter(|t| t.tag == core::tlvs::describe::TRANSPORT && t.value.len() >= 2)
        .find(|t| t.value[1] == core::enums::transport_kind::UART_BRIDGE)
        .map(|t| t.value[0])
        .ok_or_else(|| "the probe has no UART bridge".into())
}

fn port_speed_body(port: u8, rate: u32, step: u8, verify_ms: u16, idle_ms: u32) -> Vec<u8> {
    let mut b = vec![port];
    b.extend_from_slice(&rate.to_le_bytes());
    b.push(step);
    b.extend_from_slice(&verify_ms.to_le_bytes());
    b.extend_from_slice(&idle_ms.to_le_bytes());
    b
}

/// en: Try `rates` in order on this serial link (its session open) and commit the first that
/// passes; the link is left at that rate (or the boot speed). Errors only when the probe answers
/// at neither the tried rate nor the boot speed.
/// ja: `rates` を順に試し、最初に通ったものに決める。link はその速さ(か起動時の速さ)のまま。
/// `budget` bounds the whole: no new rate is tried after it (each takes about 1.3 s).
pub fn raise_speed(
    p: &mut Probe,
    rates: &[u32],
    budget: Duration,
) -> Result<SpeedReport, OepError> {
    let t0 = Instant::now();
    let mut report = SpeedReport {
        rate: p.link().baud().unwrap_or(0),
        ..SpeedReport::default()
    };
    if p.link().base_baud().is_none() {
        report.why = Some("the link is not a serial port".into());
        return Ok(report);
    }
    if p.session_id().is_none() {
        report.why = Some("no session (the rate lasts as long as one)".into());
        return Ok(report);
    }
    let port = match speed_port(p) {
        Ok(port) => port,
        Err(why) => {
            report.why = Some(why);
            return Ok(report);
        }
    };
    report.supported = true;
    report.port = Some(port);
    let full = usize::from(p.limits().max_inflight.max(1));
    for &rate in rates {
        if t0.elapsed() >= budget {
            report.why = Some(format!("out of time ({} ms)", budget.as_millis()));
            break;
        }
        // The verify's requests go without the session (lock-free) and do not renew the lease.
        p.keepalive()?;
        let started = Instant::now();
        let mut trial = SpeedTrial {
            rate,
            ..SpeedTrial::default()
        };
        let tried = p.call(
            core::FN,
            core::op::PORT_SPEED,
            port_speed_body(port, rate, STEP_TRY, VERIFY_MS, 0),
        );
        match tried {
            Ok(r) if r.succeeded() => {
                trial.actual = r
                    .payload
                    .get(..4)
                    .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
            }
            Ok(r) => {
                let unknown = r.resolution
                    == crate::codec::Resolution::Rejected(reject_reasons::UNKNOWN_OPERATION);
                if unknown {
                    report.supported = false;
                    report.why = Some("the probe does not take port_speed".into());
                    break;
                }
                let unsupported =
                    r.resolution == crate::codec::Resolution::Rejected(reject_reasons::UNSUPPORTED);
                trial.why = Some(if unsupported {
                    "the probe's UART cannot make it".into()
                } else {
                    format!("refused ({:?})", r.resolution)
                });
                trial.elapsed = started.elapsed();
                report.trials.push(trial);
                if unsupported {
                    continue;
                }
                break; // wrong port, locked, ...: nothing else will do better
            }
            Err(_) => {
                // It may have switched: wait it out at the boot speed.
                trial.why = Some("no answer to the try".into());
                trial.elapsed = started.elapsed();
                report.trials.push(trial);
                if !p.link().back_to_base(wait_back()) {
                    return Err(OepError::Link(LinkError::Timeout(wait_back())));
                }
                continue;
            }
        }
        p.link().set_baud(rate)?;
        // The switch-over may cost the first frame: a few confirms find the new rate.
        let heard = (0..3).any(|_| p.link().confirm_raw(Duration::from_millis(200)));
        let mut ok = heard;
        if heard {
            // As the link will run (pipelined), then one at a time if that broke.
            let tries: &[usize] = if full == 1 { &[1] } else { &[full, 1] };
            for &n in tries {
                trial.broken_in = 0;
                trial.broken_out = 0;
                trial.broken_both = 0;
                ok = verify(p, rate, n, &mut trial);
                if ok {
                    trial.inflight = n;
                    break;
                }
                let _ = (0..3).any(|_| p.link().confirm_raw(Duration::from_millis(200)));
            }
        }
        if ok {
            let committed = p
                .call(
                    core::FN,
                    core::op::PORT_SPEED,
                    port_speed_body(port, rate, STEP_COMMIT, 0, IDLE_MS),
                )
                .ok()
                .is_some_and(|r| r.succeeded());
            if committed {
                trial.committed = true;
                p.link().inflight_cap = if trial.inflight < full {
                    trial.inflight
                } else {
                    0
                };
                trial.elapsed = started.elapsed();
                report.rate = rate;
                report.trials.push(trial);
                report.elapsed = t0.elapsed();
                return Ok(report);
            }
            trial.why = Some("the commit failed".into());
        } else {
            trial.why = Some(if heard {
                "frames broke".into()
            } else {
                "no confirm at the new rate".into()
            });
            // Lost at that rate is fine: the probe goes back by itself.
            let session = p.session_id();
            let _ = p.link().exchange_once(
                vec![Call {
                    func: core::FN,
                    op: core::op::PORT_SPEED,
                    session,
                    payload: port_speed_body(port, rate, STEP_REVERT, 0, 0),
                }],
                1,
                Duration::from_millis(300),
            );
        }
        trial.elapsed = started.elapsed();
        report.trials.push(trial);
        if !p.link().back_to_base(wait_back()) {
            return Err(OepError::Link(LinkError::Timeout(wait_back())));
        }
        report.rate = p.link().baud().unwrap_or(0);
    }
    report.elapsed = t0.elapsed();
    Ok(report)
}

/// The probe's verify_ms and some: a probe still trying goes back after that.
fn wait_back() -> Duration {
    Duration::from_millis(u64::from(VERIFY_MS) + 1500)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    In,
    Out,
    Both,
}

/// en: Both ways with max_frame-sized frames, `inflight` at a time: link_source (in), link_sink
/// (out), then the two interleaved (both); stops at the first frame that breaks.
/// ja: 両方向に max_frame の大きさで流す(in、次に out)。最初に壊れた所で止める。
fn verify(p: &mut Probe, rate: u32, inflight: usize, trial: &mut SpeedTrial) -> bool {
    let limits = p.limits();
    let max_frame = usize::from(limits.max_frame);
    let n_in = max_frame.saturating_sub(VERIFY_ROOM);
    let n_out = max_frame.saturating_sub(VERIFY_ROOM);
    let wire = (max_frame + 8) as f64 * 10.0 / f64::from(rate.max(1));
    let timeout = Duration::from_secs_f64((4.0 * wire * inflight as f64 + 0.1).max(0.3));
    let pattern: Vec<u8> = (0..n_in).map(|k| k as u8).collect();
    // In, out, then both at once: a line may carry each way alone and break when both run
    // (oep-core §3.5; a CH340 at 921600 under a fixture UART's stream, 2026-10-01).
    for phase in [Phase::In, Phase::Out, Phase::Both] {
        if phase != Phase::In && p.keepalive().is_err() {
            return false;
        }
        let limit = if phase == Phase::Both {
            VERIFY_TIME
        } else {
            VERIFY_TIME / 2
        };
        let started = Instant::now();
        let mut moved = 0usize;
        let mut broken = 0u32;
        while moved < VERIFY_BYTES / 2 && started.elapsed() < limit {
            let kinds: Vec<bool> = (0..inflight * 2)
                .map(|k| match phase {
                    Phase::In => true,
                    Phase::Out => false,
                    Phase::Both => k % 2 == 0,
                })
                .collect();
            let calls: Vec<Call> = kinds
                .iter()
                .map(|&inward| {
                    if inward {
                        Call {
                            func: core::FN,
                            op: core::op::LINK_SOURCE,
                            session: None,
                            payload: (n_in as u32).to_le_bytes().to_vec(),
                        }
                    } else {
                        Call {
                            func: core::FN,
                            op: core::op::LINK_SINK,
                            session: None,
                            payload: (0..n_out).map(|k| (k * 7) as u8).collect(),
                        }
                    }
                })
                .collect();
            let sent = calls.len();
            let replies = p.link().exchange_once(calls, inflight, timeout);
            let good = replies
                .iter()
                .zip(&kinds)
                .take_while(|(r, inward)| {
                    r.resolution == crate::codec::Resolution::Completed(outcomes::SUCCESS)
                        && if **inward {
                            r.payload == pattern
                        } else {
                            r.payload.get(..4) == Some(&(n_out as u32).to_le_bytes()[..])
                        }
                })
                .count();
            moved += kinds[..good]
                .iter()
                .map(|&inward| if inward { n_in } else { n_out })
                .sum::<usize>();
            if good < sent {
                broken += (sent - good) as u32;
                break;
            }
        }
        let kb_s = moved as f64 / started.elapsed().as_secs_f64().max(1e-6) / 1000.0;
        match phase {
            Phase::In => {
                trial.in_kb_s = kb_s;
                trial.broken_in = broken;
            }
            Phase::Out => {
                trial.out_kb_s = kb_s;
                trial.broken_out = broken;
            }
            Phase::Both => {
                trial.both_kb_s = kb_s;
                trial.broken_both = broken;
            }
        }
        if broken > 0 {
            return false;
        }
    }
    true
}

/// en: Back to the boot speed together with the probe (oep-core §3.5: the host, which counts the
/// broken frames, lowers first): port_speed revert at the speed now (answered there, lost is
/// fine: the probe goes back on its own too), then the host side, confirmed. True when the link
/// answers at the boot speed. ja: probe と揃って起動時の速さに戻る(戻すを今の速さで送り、host も
/// 戻して confirm)。
pub fn revert(p: &mut Probe, port: u8) -> bool {
    let Some(base) = p.link().base_baud() else {
        return false;
    };
    if p.link().baud() == Some(base) {
        return true;
    }
    let rate = p.link().baud().unwrap_or(base);
    let session = p.session_id();
    let _ = p.link().exchange_once(
        vec![Call {
            func: core::FN,
            op: core::op::PORT_SPEED,
            session,
            payload: port_speed_body(port, rate, STEP_REVERT, 0, 0),
        }],
        1,
        Duration::from_millis(300),
    );
    p.link().back_to_base(wait_back())
}
