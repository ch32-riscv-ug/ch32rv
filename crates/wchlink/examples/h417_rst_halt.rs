//! Experiment (2026-10-20): catch a CH32H417 whose firmware takes PB8 / PB9 (SWCLK / SWDIO) a few
//! ms after boot - hold NRST through the LinkE's RstOut with haltreq (and setresethaltreq) set,
//! release it, halt at once - then erase the first 64 KiB (the code the boot copies to RAM) with
//! one BER block erase, following the H417 RM's CTLR table.
//! Usage: cargo run -p ch32rv-wchlink --example h417_rst_halt -- <link serial> [erase]
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

fn rd(l: &mut WchLink, a: u8) -> u32 {
    l.dmi_op(a, 0, 1).expect("dmi read").data
}
fn wr(l: &mut WchLink, a: u8, v: u32) {
    l.dmi_op(a, v, 2).expect("dmi write");
}
fn cmd(l: &mut WchLink, c: u32) -> Result<(), String> {
    wr(l, ABSTRACTCS, 0x700);
    wr(l, COMMAND, c);
    for _ in 0..100 {
        let cs = rd(l, ABSTRACTCS);
        if cs & (1 << 12) == 0 {
            let err = (cs >> 8) & 7;
            return if err == 0 {
                Ok(())
            } else {
                Err(format!("cmderr {err}"))
            };
        }
    }
    Err("busy".into())
}
fn read32(l: &mut WchLink, addr: u32) -> Result<u32, String> {
    wr(l, PROGBUF0, 0x0002_a303); // lw x6, 0(x5)
    wr(l, PROGBUF1, 0x0010_0073); // ebreak
    wr(l, DATA0, addr);
    cmd(l, 0x0027_1005)?; // x5 <- data0, then progbuf
    cmd(l, 0x0022_1006)?; // data0 <- x6
    Ok(rd(l, DATA0))
}
fn write32(l: &mut WchLink, addr: u32, v: u32) -> Result<(), String> {
    wr(l, PROGBUF0, 0x0072_a023); // sw x7, 0(x5)
    wr(l, PROGBUF1, 0x0010_0073); // ebreak
    wr(l, DATA0, addr);
    cmd(l, 0x0023_1005)?; // x5 <- data0
    wr(l, DATA0, v);
    cmd(l, 0x0027_1007) // x7 <- data0, then progbuf
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

    // Hold the chip in reset first, then attach (the DM may or may not answer under reset).
    l.rst_out(true).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let attached = l.attach_chip();
    println!("attach under reset: {attached:?}");
    // haltreq | setresethaltreq | dmactive
    wr(&mut l, DMCONTROL, 0x8000_0009);
    println!("dmstatus under reset: 0x{:08x}", rd(&mut l, DMSTATUS));
    l.rst_out(false).unwrap();
    let t_rel = Instant::now();
    // The wire needs AttachChip again after the reset; try it at once, again and again.
    for k in 0..12 {
        if k > 0 {
            std::thread::sleep(Duration::from_millis(200));
        }
        match l.attach_chip() {
            Ok(a) => {
                println!("attach #{k} ok after {:?}: {a:?}", t_rel.elapsed());
                break;
            }
            Err(e) => println!("attach #{k} failed after {:?}: {e}", t_rel.elapsed()),
        }
    }
    let t0 = Instant::now();
    let mut halted = false;
    let mut seen = Vec::new();
    for i in 0..2000 {
        wr(&mut l, DMCONTROL, 0x8000_0001);
        let st = rd(&mut l, DMSTATUS);
        if seen.len() < 12 {
            seen.push((t0.elapsed(), st));
        }
        // version 2 or 3 and allhalted: a real answer, not a lost wire.
        if (st & 0xf == 2 || st & 0xf == 3) && st & (1 << 9) != 0 && i > 0 {
            println!(
                "halted after {i} polls, {:?}, dmstatus 0x{st:08x}",
                t0.elapsed()
            );
            halted = true;
            break;
        }
    }
    for (t, st) in &seen {
        println!("  {t:?} dmstatus 0x{st:08x}");
    }
    if !halted {
        println!("not halted; dmstatus 0x{:08x}", rd(&mut l, DMSTATUS));
        return;
    }
    wr(&mut l, DMCONTROL, 0x0000_0001); // drop haltreq, stay halted
    println!("abstractcs 0x{:08x}", rd(&mut l, ABSTRACTCS));
    let _ = cmd(&mut l, 0x0022_07b1); // data0 <- dpc
    println!("dpc 0x{:08x}", rd(&mut l, DATA0));
    for (n, a) in [
        ("STATR", 0x4002_200c),
        ("CTLR", 0x4002_2010),
        ("CFGR0", 0x4002_202c),
    ] {
        println!("{n} {:?}", read32(&mut l, a).map(|v| format!("0x{v:08x}")));
    }
    if !erase {
        return;
    }
    // Unlock (KEYR, MODEKEYR), BER on 0x08000000 (64 KiB with DBMODE = 1), wait BSY.
    for (a, k) in [
        (0x4002_2004, 0x4567_0123),
        (0x4002_2004, 0xcdef_89ab),
        (0x4002_2024, 0x4567_0123),
        (0x4002_2024, 0xcdef_89ab),
    ] {
        write32(&mut l, a, k).unwrap();
    }
    let ctlr = read32(&mut l, 0x4002_2010).unwrap();
    println!("CTLR after keys 0x{ctlr:08x}");
    for block in 0..2u32 {
        let addr = 0x0800_0000 + block * 0x1_0000;
        write32(&mut l, 0x4002_2010, 1 << 18).unwrap(); // BER
        write32(&mut l, 0x4002_2014, addr).unwrap(); // ADDR
        write32(&mut l, 0x4002_2010, (1 << 18) | (1 << 6)).unwrap(); // BER | STRT
        let t0 = Instant::now();
        loop {
            let st = read32(&mut l, 0x4002_200c).unwrap();
            if st & 1 == 0 {
                println!(
                    "BER 0x{addr:08x} done in {:?}, STATR 0x{st:08x}",
                    t0.elapsed()
                );
                break;
            }
            if t0.elapsed() > Duration::from_secs(5) {
                println!("BER still busy");
                break;
            }
        }
        write32(&mut l, 0x4002_2010, 0).unwrap();
    }
    let mut bad = 0;
    for i in 0..128 {
        let v = read32(&mut l, 0x0800_0000 + i * 1024).unwrap_or(0);
        if v != 0xe339_e339 {
            bad += 1;
        }
    }
    println!("sampled 128 words across 128 KiB: {bad} not erased");
    write32(&mut l, 0x4002_2010, (1 << 7) | (1 << 15)).unwrap(); // LOCK | FLOCK
}
