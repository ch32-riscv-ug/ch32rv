# ch32rv-usb-wch-win

Access USB devices bound to WCH's stock Windows vendor driver (the CH375 driver family:
`WCHLinkW64.SYS` for WCH-Link probes and compatible CH37x function drivers) from 64-bit
Rust — **no WinUSB replacement (Zadig), no 32-bit-only `WCHLinkDLL.dll`**.

Part of the [ch32rv](https://github.com/ch32-riscv-ug/ch32rv) suite, but deliberately
protocol-agnostic: it knows nothing about WCH-Link framing, only device-interface
enumeration, open, and endpoint-addressed bulk transfers over `DeviceIoControl`. Any Rust
tool that wants to talk to a WCH device without touching the user's installed driver can
use it as a component.

## Why

On Windows, installing WCH-LinkUtility (or the WCH driver package) binds the WCH-Link
vendor interface to WCH's own driver. WinUSB-based stacks (`nusb`, `libusb`) then cannot
open the device, and replacing the driver via Zadig breaks WCH-LinkUtility in return.
This crate talks to the stock driver directly, so both worlds coexist.

## Example

```rust,no_run
use ch32rv_usb_wch_win::{list_interfaces, GUID_CH375};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for iface in list_interfaces(&GUID_CH375)? {
        // e.g. "USB\VID_1A86&PID_8010\<serial>"
        println!("{}", iface.parent_instance_id()?);
        let mut dev = iface.open()?;
        // WCH-Link GetProbeInfo: one request/reply round trip on EP 0x01/0x81.
        dev.write_pipe(0x01, &[0x81, 0x0d, 0x01, 0x01])?;
        let mut reply = [0u8; 64];
        let n = dev.read_pipe(0x81, &mut reply)?;
        println!("  reply: {:02x?}", &reply[..n]);
    }
    Ok(())
}
```

## Verified

- WCH-Link (CH549, fw 2.12) and WCH-LinkE (fw 2.22), Windows 11 x64, driver
  `wchlinkwdm.inf` / `WCHLinkW64.SYS` (2026-09-02).
- Interface GUID `{F8D5EDCA-B647-4E9C-9BD3-A5BD2328D55C}` (`GUID_CH375`).

## Caveats

- **Windows only.** On other platforms the crate compiles to nothing; depend on it via
  `[target.'cfg(windows)'.dependencies]`.
- Transfers block until the device responds; the driver exposes no verified timeout
  control yet. Stick to request/reply patterns.
- One ioctl moves at most 64 bytes; longer transfers are chunked internally. Behaviour
  for multi-packet reads beyond 64 bytes is implemented per the CH375 semantics but not
  yet exercised against large flash transfers.

## License

MIT.
