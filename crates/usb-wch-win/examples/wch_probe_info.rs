//! en: Hardware smoke test — one WCH-Link GetProbeInfo round trip per connected probe,
//! through the stock WCH driver. Run on Windows with a WCH-Link attached (and owned by
//! the WCH driver, i.e. not attached to WSL via usbipd).
//! ja: 実機スモークテスト。接続中の各 WCH-Link に対し、WCH 標準ドライバ経由で
//! GetProbeInfo を 1 往復する。

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use ch32rv_usb_wch_win::{GUID_CH375, list_interfaces};

    let interfaces = list_interfaces(&GUID_CH375)?;
    println!("{} CH375 device interface(s)", interfaces.len());

    for iface in &interfaces {
        println!("\n{}", iface.path());
        println!("  instance : {}", iface.instance_id()?);
        println!("  parent   : {}", iface.parent_instance_id()?);

        let mut dev = iface.open()?;
        dev.write_pipe(0x01, &[0x81, 0x0d, 0x01, 0x01])?;
        let mut reply = [0u8; 64];
        let n = dev.read_pipe(0x81, &mut reply)?;
        let hex: Vec<String> = reply[..n].iter().map(|b| format!("{b:02x}")).collect();
        println!("  reply    : {}", hex.join(" "));
        if n >= 5 && reply[0] == 0x82 && reply[1] == 0x0d {
            println!("  probe    : firmware v{}.{}", reply[3], reply[4]);
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("this example is Windows-only");
}
