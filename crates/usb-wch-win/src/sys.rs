//! en: The Windows implementation: cfgmgr32 enumeration, `CreateFileW` open, and bulk
//! transfers through `IOCTL_CH375_COMMAND`. All byte layouts and codes were verified
//! against live WCH-Link probes (docs/windows-wch-driver.ja.md §5.1, 2026-09-02).
//! ja: Windows 実装。cfgmgr32 での列挙、`CreateFileW` open、`IOCTL_CH375_COMMAND` での
//! bulk 転送。レイアウトと定数は実機で検証済み(§5.1)。

use std::ffi::c_void;
use std::ptr::{from_mut, from_ref, null, null_mut};

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_ID_Size, CM_Get_Device_IDW,
    CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_ListW,
    CM_Get_Device_Interface_PropertyW, CM_Get_Parent, CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW,
    CR_BUFFER_SMALL, CR_SUCCESS,
};
use windows_sys::Win32::Devices::Properties::{
    DEVPKEY_Device_InstanceId, DEVPROP_TYPE_STRING, DEVPROPTYPE,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::core::GUID;

use crate::proto::{HEADER_LEN, IOCTL_CH375_COMMAND, PACKET_LEN, function_read, function_write};
use crate::{Ch375Error, InterfaceGuid};

impl InterfaceGuid {
    fn as_sys(&self) -> GUID {
        GUID {
            data1: self.data1,
            data2: self.data2,
            data3: self.data3,
            data4: self.data4,
        }
    }
}

fn cm_err(op: &'static str, code: u32) -> Ch375Error {
    Ch375Error::Cm { op, code }
}

fn win32_err(op: &'static str) -> Ch375Error {
    Ch375Error::Win32 {
        op,
        code: unsafe { GetLastError() },
    }
}

fn to_utf16z(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_utf16_trimmed(units: &[u16]) -> String {
    let end = units.iter().position(|&c| c == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

/// en: Enumerate the present device interfaces of `guid`. Devices whose interface is
/// disabled (unplugged phantoms) are not returned.
/// ja: `guid` の device interface を列挙する(present のみ。抜去済みの phantom は返らない)。
pub fn list_interfaces(guid: &InterfaceGuid) -> Result<Vec<DeviceInterface>, Ch375Error> {
    let sys_guid = guid.as_sys();
    // en: size+fetch can race against hotplug; retry on CR_BUFFER_SMALL a few times.
    for _ in 0..4 {
        let mut len: u32 = 0;
        let cr = unsafe {
            CM_Get_Device_Interface_List_SizeW(
                &mut len,
                &sys_guid,
                null(),
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if cr != CR_SUCCESS {
            return Err(cm_err("CM_Get_Device_Interface_List_SizeW", cr));
        }
        let mut buf = vec![0u16; len as usize];
        let cr = unsafe {
            CM_Get_Device_Interface_ListW(
                &sys_guid,
                null(),
                buf.as_mut_ptr(),
                len,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if cr == CR_BUFFER_SMALL {
            continue;
        }
        if cr != CR_SUCCESS {
            return Err(cm_err("CM_Get_Device_Interface_ListW", cr));
        }
        // Double-nul-terminated list of nul-terminated strings.
        return Ok(buf
            .split(|&c| c == 0)
            .filter(|chunk| !chunk.is_empty())
            .map(|chunk| {
                let mut path_utf16 = chunk.to_vec();
                path_utf16.push(0);
                DeviceInterface {
                    path: String::from_utf16_lossy(chunk),
                    path_utf16,
                }
            })
            .collect());
    }
    Err(cm_err("CM_Get_Device_Interface_ListW", CR_BUFFER_SMALL))
}

/// en: One enumerated device interface (a `\\?\USB#VID_xxxx&...#{guid}` path) that can be
/// opened into a [`Ch375Device`].
/// ja: 列挙された device interface 1 つ。[`Ch375Device`] として open できる。
#[derive(Clone)]
pub struct DeviceInterface {
    path_utf16: Vec<u16>,
    path: String,
}

impl std::fmt::Debug for DeviceInterface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("DeviceInterface").field(&self.path).finish()
    }
}

impl DeviceInterface {
    /// The interface path, e.g. `\\?\USB#VID_1A86&PID_8010&MI_00#6&27b6deca&0&0000#{f8d5edca-...}`.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// en: The owning devnode's instance ID, e.g. `USB\VID_1A86&PID_8010&MI_00\6&27b6deca&0&0000`.
    /// ja: この interface を持つ devnode の instance ID。
    pub fn instance_id(&self) -> Result<String, Ch375Error> {
        let mut prop_type: DEVPROPTYPE = 0;
        let mut size: u32 = 0;
        let cr = unsafe {
            CM_Get_Device_Interface_PropertyW(
                self.path_utf16.as_ptr(),
                &DEVPKEY_Device_InstanceId,
                &mut prop_type,
                null_mut(),
                &mut size,
                0,
            )
        };
        if cr != CR_BUFFER_SMALL {
            return Err(cm_err("CM_Get_Device_Interface_PropertyW(size)", cr));
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        let cr = unsafe {
            CM_Get_Device_Interface_PropertyW(
                self.path_utf16.as_ptr(),
                &DEVPKEY_Device_InstanceId,
                &mut prop_type,
                buf.as_mut_ptr().cast::<u8>(),
                &mut size,
                0,
            )
        };
        if cr != CR_SUCCESS {
            return Err(cm_err("CM_Get_Device_Interface_PropertyW", cr));
        }
        if prop_type != DEVPROP_TYPE_STRING {
            return Err(cm_err("CM_Get_Device_Interface_PropertyW(type)", prop_type));
        }
        Ok(from_utf16_trimmed(&buf))
    }

    /// en: The parent devnode's instance ID. For a composite USB device this is
    /// `USB\VID_xxxx&PID_xxxx\<serial-or-generated-id>` — the way to correlate an MI_xx
    /// function (which carries no serial of its own) with enumeration by serial number.
    /// ja: 親 devnode の instance ID。composite device では
    /// `USB\VID_xxxx&PID_xxxx\<serial>` になり、serial を持たない MI_xx 機能と
    /// serial 基準の列挙(nusb 等)を突き合わせる手掛かりになる。
    pub fn parent_instance_id(&self) -> Result<String, Ch375Error> {
        let own = to_utf16z(&self.instance_id()?);
        let mut devinst: u32 = 0;
        let cr =
            unsafe { CM_Locate_DevNodeW(&mut devinst, own.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) };
        if cr != CR_SUCCESS {
            return Err(cm_err("CM_Locate_DevNodeW", cr));
        }
        let mut parent: u32 = 0;
        let cr = unsafe { CM_Get_Parent(&mut parent, devinst, 0) };
        if cr != CR_SUCCESS {
            return Err(cm_err("CM_Get_Parent", cr));
        }
        let mut chars: u32 = 0;
        let cr = unsafe { CM_Get_Device_ID_Size(&mut chars, parent, 0) };
        if cr != CR_SUCCESS {
            return Err(cm_err("CM_Get_Device_ID_Size", cr));
        }
        let mut buf = vec![0u16; chars as usize + 1];
        let cr = unsafe { CM_Get_Device_IDW(parent, buf.as_mut_ptr(), chars + 1, 0) };
        if cr != CR_SUCCESS {
            return Err(cm_err("CM_Get_Device_IDW", cr));
        }
        Ok(from_utf16_trimmed(&buf))
    }

    /// Open this interface for bulk transfers.
    pub fn open(&self) -> Result<Ch375Device, Ch375Error> {
        let handle = unsafe {
            CreateFileW(
                self.path_utf16.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(win32_err("CreateFileW"));
        }
        Ok(Ch375Device { handle })
    }
}

/// The driver's transfer block: an 8-byte header followed by up to 64 payload bytes.
#[repr(C)]
struct Win32Command {
    m_function: u32,
    m_length: u32,
    m_buffer: [u8; PACKET_LEN],
}

/// en: An open CH375 device handle with endpoint-addressed blocking bulk transfers.
/// The handle is closed on drop.
///
/// Transfers block until the device produces or consumes data: the driver exposes no
/// verified timeout control yet, so only read after a command that makes the device
/// respond (the usual request/reply pattern).
///
/// ja: open 済みの CH375 device。endpoint 番号指定のブロッキング bulk 転送を提供し、
/// drop で handle を閉じる。timeout 制御は未検証のため、応答が保証される
/// request/reply パターンでのみ read すること。
#[derive(Debug)]
pub struct Ch375Device {
    handle: HANDLE,
}

// en: The kernel handle is process-global; moving it across threads is sound.
// ja: kernel handle はプロセス全体で有効なので、スレッド間 move は健全。
unsafe impl Send for Ch375Device {}

impl Drop for Ch375Device {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

impl Ch375Device {
    fn ioctl(&mut self, cmd: &mut Win32Command, in_len: u32) -> Result<u32, Ch375Error> {
        let mut ret: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                IOCTL_CH375_COMMAND,
                from_ref(cmd).cast::<c_void>(),
                in_len,
                from_mut(cmd).cast::<c_void>(),
                HEADER_LEN + PACKET_LEN as u32,
                &mut ret,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(win32_err("DeviceIoControl"));
        }
        if ret < HEADER_LEN {
            return Err(Ch375Error::MalformedReply(ret));
        }
        Ok(ret)
    }

    /// en: Write `data` to the OUT pipe addressed by `ep` (e.g. 0x01, 0x02). Transfers
    /// longer than 64 bytes are split into 64-byte ioctls. Empty writes are a no-op.
    /// ja: `ep` の OUT pipe へ書く。64 byte 超は 64 byte 単位の ioctl に分割する。
    pub fn write_pipe(&mut self, ep: u8, data: &[u8]) -> Result<(), Ch375Error> {
        let function = function_write(ep)?;
        for chunk in data.chunks(PACKET_LEN) {
            let mut cmd = Win32Command {
                m_function: function,
                m_length: chunk.len() as u32,
                m_buffer: [0; PACKET_LEN],
            };
            cmd.m_buffer[..chunk.len()].copy_from_slice(chunk);
            let ret = self.ioctl(&mut cmd, HEADER_LEN + chunk.len() as u32)?;
            let accepted = (ret - HEADER_LEN) as usize;
            if accepted != chunk.len() {
                return Err(Ch375Error::ShortWrite {
                    accepted,
                    requested: chunk.len(),
                });
            }
        }
        Ok(())
    }

    /// en: Read from the IN pipe addressed by `ep` (e.g. 0x81, 0x82) until `buf` is full
    /// or the device ends the bulk transfer with a short packet (< 64 bytes). Returns the
    /// number of bytes read. Blocks until the device has data (see the type-level note).
    /// ja: `ep` の IN pipe から、`buf` が埋まるか short packet(64 byte 未満)で転送が
    /// 終わるまで読む。device がデータを持つまでブロックする点に注意。
    pub fn read_pipe(&mut self, ep: u8, buf: &mut [u8]) -> Result<usize, Ch375Error> {
        let function = function_read(ep)?;
        let mut total = 0usize;
        while total < buf.len() {
            let want = (buf.len() - total).min(PACKET_LEN);
            let mut cmd = Win32Command {
                m_function: function,
                m_length: want as u32,
                m_buffer: [0; PACKET_LEN],
            };
            self.ioctl(&mut cmd, HEADER_LEN)?;
            let got = (cmd.m_length as usize).min(want);
            buf[total..total + got].copy_from_slice(&cmd.m_buffer[..got]);
            total += got;
            if got < PACKET_LEN {
                break;
            }
        }
        Ok(total)
    }
}
