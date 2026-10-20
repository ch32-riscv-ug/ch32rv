//! Experiment (2026-10-20): read and clear a CH32H417's FLASH_STATR.BOOT_MODE (bit 14) in one
//! connection - the BOOT_MODEKEYR unlock lasts only until the next reset, so the keys and the
//! write must share a session. Usage:
//! cargo run -p ch32rv-wchlink --example h417_boot_mode -- <link serial> [clear|set]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use ch32rv_wchlink::WchLink;
use std::time::Duration;

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
    wr(l, PROGBUF0, 0x0002_a303);
    wr(l, PROGBUF1, 0x0010_0073);
    wr(l, DATA0, addr);
    cmd(l, 0x0027_1005)?;
    cmd(l, 0x0022_1006)?;
    Ok(rd(l, DATA0))
}
fn write32(l: &mut WchLink, addr: u32, v: u32) -> Result<(), String> {
    wr(l, PROGBUF0, 0x0072_a023);
    wr(l, PROGBUF1, 0x0010_0073);
    wr(l, DATA0, addr);
    cmd(l, 0x0023_1005)?;
    wr(l, DATA0, v);
    cmd(l, 0x0027_1007)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let serial = args.get(1).expect("link serial");
    let action = args.get(2).map(String::as_str);
    let dev = ch32rv_usb::enumerate()
        .unwrap()
        .into_iter()
        .find(|d| d.serial().is_some_and(|s| s.eq_ignore_ascii_case(serial)))
        .expect("no such link");
    let mut l = WchLink::open(&dev).unwrap();
    l.set_timeout(Duration::from_secs(3));
    let _ = l.detach_chip();
    l.set_speed(0xC6, Default::default()).unwrap();
    println!("attach: {:?}", l.attach_chip());
    wr(&mut l, DMCONTROL, 0x8000_0001);
    println!("dmstatus 0x{:08x}", rd(&mut l, DMSTATUS));
    let statr = read32(&mut l, 0x4002_200c).unwrap();
    println!("STATR before 0x{statr:08x}");
    if let Some(a) = action {
        for (addr, key) in [
            (0x4002_2004, 0x4567_0123),
            (0x4002_2004, 0xcdef_89ab),
            (0x4002_2028, 0x4567_0123),
            (0x4002_2028, 0xcdef_89ab),
        ] {
            write32(&mut l, addr, key).unwrap();
        }
        println!(
            "STATR after keys 0x{:08x}",
            read32(&mut l, 0x4002_200c).unwrap()
        );
        let want = if a == "set" {
            statr | (1 << 14)
        } else {
            statr & !(1 << 14)
        };
        write32(&mut l, 0x4002_200c, want).unwrap();
        println!(
            "STATR after write 0x{:08x}",
            read32(&mut l, 0x4002_200c).unwrap()
        );
    }
    wr(&mut l, DMCONTROL, 0x4000_0001); // resumereq
}
