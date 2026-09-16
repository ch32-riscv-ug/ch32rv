//! Bench diagnostic: how large a single ReadMemory the probe honours, and whether anything is
//! left on the data endpoint afterwards. Usage: read_limit <serial> <len>[,<len>...]
//! (not a test - needs a probe with an attached target; reads only, never writes)
//! Findings: docs/protocol/wch-link.ja.md §4.2.2 (per-probe stream rate and the 16 KiB read window).
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::cast_precision_loss
)]
use std::time::{Duration, Instant};

use ch32rv_dmi::DebugModule;
use ch32rv_wchlink::{Speed, WchLink};

fn main() {
    let mut args = std::env::args().skip(1);
    let serial = args.next().expect("serial");
    let lens: Vec<u32> = args
        .next()
        .expect("len list")
        .split(',')
        .map(|s| s.parse().expect("u32"))
        .collect();
    let dev = ch32rv_usb::enumerate()
        .expect("enumerate")
        .into_iter()
        .find(|d| d.serial() == Some(serial.as_str()))
        .expect("probe not found");
    let mut link = WchLink::open(&dev).expect("open");
    link.set_timeout(Duration::from_millis(3000));
    let info = link.probe_info().expect("probe_info");
    link.set_speed_default(Speed::High).expect("speed");
    let att = link.attach_chip().expect("attach");
    println!(
        "probe {:?} fw {}.{}  target family 0x{:02x} chip 0x{:08x}",
        info.variant, info.fw_major, info.fw_minor, att.family_byte, att.chip_id
    );
    DebugModule::new(&mut link).halt().expect("halt");
    for len in lens {
        let t0 = Instant::now();
        let r = link.read_mem(0x0800_0000, len);
        let dt = t0.elapsed().as_secs_f64() * 1e3;
        match &r {
            Ok(d) => println!(
                "read_mem({len:7}) -> OK {} bytes in {dt:8.1} ms  head={:02x?}",
                d.len(),
                &d[..8.min(d.len())]
            ),
            Err(e) => println!("read_mem({len:7}) -> ERR after {dt:8.1} ms: {e}"),
        }
        // Anything left behind? Bounded speculative read, a few times with growing waits.
        for wait in [50u64, 500, 2000] {
            let mut b = [0u8; 4096];
            let t1 = Instant::now();
            match link.debug_read_data(&mut b, Duration::from_millis(wait)) {
                Ok(n) => {
                    println!(
                        "     leftover after {:6.1} ms: {n} bytes head={:02x?}",
                        t1.elapsed().as_secs_f64() * 1e3,
                        &b[..8.min(n)]
                    );
                }
                Err(_) => {
                    println!("     leftover: none within {wait} ms");
                    break;
                }
            }
        }
    }
    let _ = link.detach_chip();
}
