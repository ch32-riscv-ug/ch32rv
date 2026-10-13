use ch32rv_dmi::{DebugModule, DtmAccess, RegName};
use ch32rv_wchlink::{Speed, WchLink};
use std::{path::Path, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ch32rv_usb::capture::start(Path::new("target/hil-h417-core-cli-2026-10-10/same-session-control.ndjson"))?;
    let _lock = ch32rv_usb::DeviceLock::acquire("49808F06CE30", Duration::from_secs(10))?;
    let device = ch32rv_usb::enumerate()?.into_iter().find(|d| d.serial() == Some("49808F06CE30")).ok_or("probe absent")?;
    let mut link = WchLink::open(&device)?;
    link.detach_chip()?;
    link.set_speed_default(Speed::Low)?;
    let attach = link.attach_chip()?;
    if attach.chip_id != 0x4170053d { link.detach_chip()?; return Err("unexpected target".into()); }
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        link.select_hart(1)?;
        DebugModule::new(&mut link).halt()?;
        println!("before mhartid={:x}", DebugModule::new(&mut link).read_reg(RegName::Csr(0xf14))?);
        DebugModule::new(&mut link).resume()?;
        println!("running status={:08x}", link.dmi_read(0x11)?);
        DebugModule::new(&mut link).halt()?;
        println!("halted status={:08x}", link.dmi_read(0x11)?);
        println!("after mhartid={:x}", DebugModule::new(&mut link).read_reg(RegName::Csr(0xf14))?);
        println!("selection={:08x}", link.dmi_read(0x10)?);
        Ok(())
    })();
    let _ = link.detach_chip();
    result
}
