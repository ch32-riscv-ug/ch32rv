//! en: Pure constants and math for the CH375 ioctl framing — kept OS-independent so the
//! unit tests run on every platform.
//! ja: CH375 ioctl フレーミングの純粋な定数と計算。unit test を全 OS で回すため OS 非依存。

use crate::Ch375Error;

/// `CTL_CODE(FILE_DEVICE_UNKNOWN=0x22, 0x0f37, METHOD_BUFFERED, FILE_ANY_ACCESS)`.
pub(crate) const IOCTL_CH375_COMMAND: u32 = 0x0022_3CDC;

/// The WIN32_COMMAND header: `sizeof(mFunction) + sizeof(mLength)`.
pub(crate) const HEADER_LEN: u32 = 8;

/// en: `mBuffer` capacity — one ioctl moves at most this many payload bytes.
/// ja: `mBuffer` 容量。1 ioctl で運べる payload はこのバイト数まで。
pub(crate) const PACKET_LEN: usize = 64;

const DIR_WRITE: u32 = 0x2_0000;
const DIR_READ: u32 = 0x1_0000;

/// `mFunction` for a host-to-device transfer on the pipe addressed by `ep`.
pub(crate) fn function_write(ep: u8) -> Result<u32, Ch375Error> {
    Ok(DIR_WRITE | pipe_index(ep)?)
}

/// `mFunction` for a device-to-host transfer on the pipe addressed by `ep`.
pub(crate) fn function_read(ep: u8) -> Result<u32, Ch375Error> {
    Ok(DIR_READ | pipe_index(ep)?)
}

/// en: Map a USB endpoint address to the driver's zero-based pipe index. The direction
/// bit (0x80) is ignored, so 0x02 and 0x82 both address pipe 2 — direction comes from
/// the write/read call, mirroring `CH375WriteEndP`/`CH375ReadEndP` semantics.
/// ja: endpoint アドレスを pipe index(0 始まり)へ変換する。方向 bit(0x80)は無視し、
/// 方向は write/read の呼び分けで決まる(`CH375WriteEndP`/`CH375ReadEndP` と同じ)。
fn pipe_index(ep: u8) -> Result<u32, Ch375Error> {
    let number = ep & 0x7f;
    if number == 0 || number > 15 {
        return Err(Ch375Error::InvalidEndpoint(ep));
    }
    Ok(u32::from(number) - 1)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn function_codes_match_verified_capture() {
        // Verified live on 2026-09-02 (docs/windows-wch-driver.ja.md §5.1).
        assert_eq!(function_write(0x01).unwrap(), 0x2_0000);
        assert_eq!(function_read(0x81).unwrap(), 0x1_0000);
        assert_eq!(function_write(0x02).unwrap(), 0x2_0001);
        assert_eq!(function_read(0x82).unwrap(), 0x1_0001);
    }

    #[test]
    fn direction_bit_is_ignored() {
        assert_eq!(function_write(0x81).unwrap(), function_write(0x01).unwrap());
        assert_eq!(function_read(0x02).unwrap(), function_read(0x82).unwrap());
    }

    #[test]
    fn endpoint_zero_and_out_of_range_are_rejected() {
        assert!(matches!(
            function_write(0x00),
            Err(Ch375Error::InvalidEndpoint(0x00))
        ));
        assert!(matches!(
            function_read(0x80),
            Err(Ch375Error::InvalidEndpoint(0x80))
        ));
    }
}
