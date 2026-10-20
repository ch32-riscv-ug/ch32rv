//! Experiment (2026-10-20): with the CH32H417 held in reset through the LinkE's RstOut, does the
//! Debug Module run abstract commands and the program buffer (memory access)? If so, the flash can
//! be erased before the firmware ever runs.
//! Usage: cargo run -p ch32rv-wchlink --example h417_under_reset -- <link serial> [erase]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use ch32rv_wchlink::WchLink;
use std::time::{Duration, Instant};

const DATA0: u8 = 0x04;
const DMCONTROL: u8 = 0x10;
const DMSTATUS: u8 = 0x11;
const ABSTRACTCS: u8 = 0x16;
const COMMAND: u8 = 0x17;
const PROGBUF0: u8 = 0x20;
const PROGBUF1: u8 = 0x21;

fn rd(l: &mut WchLink, a: u8) -> Result<u32, String> {
    l.dmi_op(a, 0, 1).map(|r| r.data).map_err(|e| e.to_string())
}
fn wr(l: &mut WchLink, a: u8, v: u32) -> Result<(), String> {
    l.dmi_op(a, v, 2).map(|_| ()).map_err(|e| e.to_string())
}
fn cmd(l: &mut WchLink, c: u32) -> Result<(), String> {
    wr(l, ABSTRACTCS, 0x700)?;
    wr(l, COMMAND, c)?;
    for _ in 0..100 {
        let cs = rd(l, ABSTRACTCS)?;
        if cs & (1 << 12) == 0 {
            let err = (cs >> 8) & 7;
            return if err == 0 {
                Ok(())
            } else {
                Err(format!("cmderr {err} (abstractcs 0x{cs:08x})"))
            };
        }
    }
    Err("busy".into())
}
fn read32(l: &mut WchLink, addr: u32) -> Result<u32, String> {
    wr(l, PROGBUF0, 0x0002_a303)?;
    wr(l, PROGBUF1, 0x0010_0073)?;
    wr(l, DATA0, addr)?;
    cmd(l, 0x0027_1005)?;
    cmd(l, 0x0022_1006)?;
    rd(l, DATA0)
}
fn write32(l: &mut WchLink, addr: u32, v: u32) -> Result<(), String> {
    wr(l, PROGBUF0, 0x0072_a023)?;
    wr(l, PROGBUF1, 0x0010_0073)?;
    wr(l, DATA0, addr)?;
    cmd(l, 0x0023_1005)?;
    wr(l, DATA0, v)?;
    cmd(l, 0x0027_1007)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let serial = args.get(1).expect("link serial");
    let erase = args.get(2).is_some_and(|a| a == "erase");
    let dev = ch32rv_usb::enumerate()
        .unwrap()
        .into_iter()
        .find(|d| d.serial().is_some_and(|s| s.eq_ignore_ascii_case(serial)))
        .expect("no such link");
    let mut l = WchLink::open(&dev).unwrap();
    l.set_timeout(Duration::from_secs(3));
    l.set_speed(0xC6, Default::default()).unwrap();
    l.rst_out(true).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    println!("attach under reset: {:?}", l.attach_chip());
    let r = (|| -> Result<(), String> {
        wr(&mut l, DMCONTROL, 0x8000_0001)?;
        println!("dmstatus 0x{:08x}", rd(&mut l, DMSTATUS)?);
        println!("abstractcs 0x{:08x}", rd(&mut l, ABSTRACTCS)?);
        cmd(&mut l, 0x0022_07b1)?;
        println!("dpc 0x{:08x}", rd(&mut l, DATA0)?);
        for (n, a) in [
            ("STATR", 0x4002_200c),
            ("CTLR", 0x4002_2010),
            ("CFGR0", 0x4002_202c),
            ("flash[0]", 0x0800_0000),
        ] {
            println!("{n} 0x{:08x}", read32(&mut l, a)?);
        }
        if !erase {
            return Ok(());
        }
        for (a, k) in [
            (0x4002_2004, 0x4567_0123),
            (0x4002_2004, 0xcdef_89ab),
            (0x4002_2024, 0x4567_0123),
            (0x4002_2024, 0xcdef_89ab),
        ] {
            write32(&mut l, a, k)?;
        }
        println!("CTLR after keys 0x{:08x}", read32(&mut l, 0x4002_2010)?);
        for block in 0..2u32 {
            let addr = 0x0800_0000 + block * 0x1_0000;
            write32(&mut l, 0x4002_2010, 1 << 18)?;
            write32(&mut l, 0x4002_2014, addr)?;
            write32(&mut l, 0x4002_2010, (1 << 18) | (1 << 6))?;
            let t0 = Instant::now();
            loop {
                let st = read32(&mut l, 0x4002_200c)?;
                if st & 1 == 0 {
                    println!("BER 0x{addr:08x}: {:?}, STATR 0x{st:08x}", t0.elapsed());
                    break;
                }
                if t0.elapsed() > Duration::from_secs(5) {
                    return Err("BER still busy".into());
                }
            }
            write32(&mut l, 0x4002_2010, 0)?;
        }
        let mut bad = 0;
        for i in 0..128 {
            if read32(&mut l, 0x0800_0000 + i * 1024)? != 0xe339_e339 {
                bad += 1;
            }
        }
        println!("128 words sampled over 128 KiB: {bad} not erased");
        write32(&mut l, 0x4002_2010, (1 << 7) | (1 << 15))?;
        Ok(())
    })();
    println!("result: {r:?}");
    l.rst_out(false).unwrap();
}
