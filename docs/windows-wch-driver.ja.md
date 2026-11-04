# Windows: WCH 純正ドライバのまま WCH-Link を使う(CH375 IOCTL 経路)

Windows で、利用者が既に入れている WCH 純正ドライバを WinUSB に置き換えずに(Zadig 不要)、ch32rv が WCH-Link と通信する経路。実装は `ch32rv-usb-wch-win`(`crates/usb-wch-win`)で、`ch32rv-usb`(`crates/usb`)が Windows でだけフォールバックとして使う。

## 1. なぜこの経路があるか

- ch32rv は USB を nusb で開く。nusb は Windows では WinUSB 専用。
- WCH-LinkUtility(または WCH のドライバパッケージ)を入れた Windows では、WCH-Link の vendor interface(MI_00)に WCH 純正ドライバ(`wchlinkwdm.inf`、カーネルドライバ `WCHLinkW64.SYS`、class `WCH`)が付く。CDC の MI_01 は `usbser`(COM)。
- その interface は nusb では開けない(`incompatible driver is installed for this interface`)。libusb も WinUSB / libusbK / libusb-win32 しか扱えないので同じ。
- Zadig で WinUSB に置き換えれば開けるが、今度は WCH-LinkUtility が使えなくなる。WCH 純正ドライバと WinUSB は排他。
- 純正ドライバのまま話す方法は、WCH の CH375 系ドライバの IOCTL を `DeviceIoControl` で直接出すこと。必要なのは標準の Win32 API と、WCH ドライバの IOCTL 番号・interface GUID だけ。

### 1.1 DLL ではなく IOCTL を使う理由

WCH は `WCHLinkDLL.dll`(`CH375OpenDevice` / `CH375ReadEndP` / `CH375WriteEndP` などの stdcall API)も配る。wlink はこの DLL を読み込む方式で、そのため 32bit(x86)ビルドでしか使えない。

`WCHLinkDLL.dll` はドライバパッケージ内(`DriverStore\FileRepository\wchlinkwdm.inf_amd64_*\`)にも `SysWOW64\` にもあるが、PE はどちらも x86 で、64bit 版は無い(amd64 パッケージでも 64bit なのはカーネルドライバ `WCHLinkW64.SYS` だけ)。64bit プロセスは 32bit DLL を読めない。

ch32rv は x86_64 で配るので、DLL を使わず IOCTL を直接出す(minichlink の実験ブランチ `cw2/ch32v003fun` の `experimental/minichlink-wchlinkdll-driver` と同じ方式)。DLL が要らず、bitness にも依存しない。

## 2. 構成

| 層 | 場所 | 役割 |
|---|---|---|
| `ch32rv-usb-wch-win` | `crates/usb-wch-win` | CH375 系ドライバの device interface の列挙、open、endpoint 番号指定の bulk 転送。WCH-Link の protocol は持たない |
| `ch32rv-usb` | `crates/usb/src/device.rs` | `UsbInterface` の backend の 1 つ(`Ch375`)。nusb で開けない device に限ってこちらを使う |
| 上位(`ch32rv-wchlink` / `ch32rv-dmi` / `ch32rv-flash` ほか) | | 変更なし。差し替わるのは USB の bulk トランスポートだけ |

## 3. 仕組み

### 3.1 列挙と open

- device interface GUID: `{F8D5EDCA-B647-4E9C-9BD3-A5BD2328D55C}`(`GUID_CH375`)。WCH の CH375 系ドライバが登録する interface で、接続中の WCH-Link の MI_00 ごとに 1 つある。
- `wchlinkwdm.inf` はレジストリ(`DeviceInterfaceGUIDs`)経由でもう 1 つの GUID `{CDB3B5AD-293B-4663-AA36-1AAE46463776}`(`GUID_WCHLINK_INF`)も登録する。ch32rv が使うのは `GUID_CH375` で、もう一方は定数として置いてあるだけ。
- 列挙は cfgmgr32 の `CM_Get_Device_Interface_List_SizeW` → `CM_Get_Device_Interface_ListW`(`CM_GET_DEVICE_INTERFACE_LIST_PRESENT`。抜いた device は返らない)。サイズ取得と取得の間に hotplug があると `CR_BUFFER_SMALL` になるので、数回やり直す。
- open は `CreateFileW(path, GENERIC_READ|GENERIC_WRITE, FILE_SHARE_READ|FILE_SHARE_WRITE, NULL, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL)`。handle は drop で `CloseHandle` する。管理者権限は要らない。
- serial との対応: interface の instance ID(`USB\VID_1A86&PID_8010&MI_00\6&27b6deca&0&0000` など)には serial が入らない。serial は composite の親が持つ。`parent_instance_id()` が `CM_Locate_DevNodeW` → `CM_Get_Parent` → `CM_Get_Device_IDW` で親の `USB\VID_xxxx&PID_xxxx\<SERIAL>` を返す。

### 3.2 bulk 転送(DeviceIoControl)

- IOCTL: `IOCTL_CH375_COMMAND = 0x223CDC`(`CTL_CODE(FILE_DEVICE_UNKNOWN=0x22, 0x0f37, METHOD_BUFFERED, FILE_ANY_ACCESS)`)。
- 入出力は同じ構造体(先頭 8 byte がヘッダ、payload は最大 64 byte):

  ```c
  typedef struct _WIN32_COMMAND {
      ULONG mFunction;   // 方向 | pipe
      ULONG mLength;     // データ長
      UCHAR mBuffer[64];
  } WIN32_COMMAND;
  ```

- `mFunction = 方向 | (endpoint 番号 - 1)`。方向は write = `0x20000`、read = `0x10000`。endpoint の方向 bit(0x80)は見ず、方向は write / read の呼び分けで決まる(`CH375WriteEndP` / `CH375ReadEndP` と同じ)。

  | 論理 | endpoint | mFunction |
  |---|---|---|
  | cmd out | 0x01 | `0x20000` |
  | cmd in | 0x81 | `0x10000` |
  | data out | 0x02 | `0x20001` |
  | data in | 0x82 | `0x10001` |

- write(`write_pipe`): `mLength` = 送る長さ、`mBuffer` にデータを入れ、入力長 `8 + mLength`、出力バッファ 72 byte で出す。戻り値 `ret` は `8 + 受け付けた長さ`。送った長さと違えば `ShortWrite` エラー。
- read(`read_pipe`): `mLength` = 欲しい長さ(64 以下)、入力長 8、出力バッファ 72 byte で出す。返った `mLength` の分が `mBuffer` に入る。
- 64 byte 単位: 1 回の IOCTL は payload 64 byte まで。`write_pipe` は 64 byte ごとに分けて出す。`read_pipe` は、バッファが埋まるか short packet(64 byte 未満)が来るまで繰り返す。WCH-Link の flash 書込は data endpoint に 128 / 256 byte(family の packet サイズ)を送るが、この分割で正しく届く。

## 4. ch32rv での使い方

### 4.1 `ch32rv-usb-wch-win` crate

- protocol に依存しない部品: 列挙(`list_interfaces(&guid)` → `DeviceInterface`)、`DeviceInterface::{path, instance_id, parent_instance_id, open}`、`Ch375Device::{write_pipe, read_pipe}`、GUID 定数(`GUID_CH375` / `GUID_WCHLINK_INF`)。GUID は `windows-sys` の型でなく独自の `InterfaceGuid`。
- WCH-Link のフレームなどは持たない。他のツールも WCH の device を純正ドライバのまま開くのに使える。ただし他の ch32rv の crate と同じく CLI の部品として publish しており、API と semver は約束しない(crate の README)。
- 名前は suite の prefix に揃えて `ch32rv-usb-wch-win`([naming.ja.md](naming.ja.md))。見つけやすさは keywords(`wch`, `ch375`, `wch-link`, `usb`, `windows`)と README で補う。
- version は workspace と共通。publish の順は `ch32rv-usb` より前([release-plan.ja.md](release-plan.ja.md))。
- `unsafe` はこの crate に閉じ込める。workspace の lint は `unsafe_code = forbid`(内側の `#[allow]` では外せない)なので、この crate だけ `[lints]` を workspace から継承せず、clippy の deny(`unwrap_used` / `expect_used` / `todo` / `unimplemented` / `panic`)を手で同じに保つ([architecture.ja.md](architecture.ja.md) §2)。
- 依存は `windows-sys`(cfgmgr32、`DeviceIoControl`、`CreateFileW`、`CloseHandle`)。Windows 以外では空の crate としてコンパイルされる(IOCTL の番号と計算だけは全 OS で unit test する)。使う側は `[target.'cfg(windows)'.dependencies]` で依存する。
- 実機での最小確認: `examples/wch_probe_info.rs` が、見つかった各 WCH-Link に GetProbeInfo(`81 0d 01 01`)を 1 往復送って応答を出す。

### 4.2 `ch32rv-usb` のフォールバック

- `UsbInterface` の backend は `Nusb` / `Ch375`(Windows のみ)/ `Replay`。`write` / `read` / `write_data` / `read_data` が backend で分かれる。`--capture` の記録は backend の外側で取るので、どちらの経路でも同じ fixture になる。
- `open_interface` はまず nusb で開く。失敗したら(Windows のみ)CH375 側を試す。「incompatible driver」の文言は安定した API ではないので、文言は見ず、nusb の open の失敗全般で試す。
- CH375 側の条件: interface 0(MI_00)であること、device が serial を持つこと。`GUID_CH375` の interface を列挙し、`parent_instance_id()` の末尾の serial が一致する(大文字小文字は区別しない)ものを開く。endpoint は claim せず、転送ごとに渡す。
- 見つからない・開けないときは、nusb の元のエラーをそのまま返す(ドライバ以外の原因も分かるように)。
- Linux / macOS は nusb だけ。
- device 全体に `usbccgp` / WinUSB / CH375 系以外のドライバが付いている(WSL に共有中の usbipd の stub など)device は、どちらの経路でも開けない。`arduino discovery` はこれを「WCH-Link <sn> (cannot open: driver <name>)」と出し、properties に `driver` を入れる(`UsbDeviceInfo::foreign_driver`)。

## 5. 制約と注意

- **timeout が無い**: CH375 経路の転送はブロックする。ドライバに検証済みの timeout 制御が無いため。呼び出し側はすべて request / reply の形で、応答が来ると決まっているときだけ読むので問題にならない。投機的な read はこの経路ではできない(`UsbInterface` の doc に書いてある)。
- **遅い**: 1 回の IOCTL は 64 byte まで。往復の回数がそのまま時間になるので、WinUSB の経路より遅い。
- **usbipd**: WCH-Link を WSL に attach していると、usbipd の stub が device を持つので CH375 経路も使えない。`usbipd detach --busid <X>` で Windows に返す(`usbipd list` の状態が `Shared` になれば WCH ドライバの持ち物)。detach の直後は再列挙でしばらく不安定なので、`Shared` に落ち着いてから使う。`usbipd bind --force` は stub を恒久的に上書きするので使わない。
- **WinUSB が付いた device**: WCH ドライバが入っていない機器などで interface に WinUSB が付いていれば、従来どおり nusb の経路で開く。CH375 経路は WCH ドライバが先に入っている機器との共存のためのもの。
- **上位は変えない**: 違いは USB の bulk トランスポートの境界だけに閉じる。

Windows 11 の WCH 純正ドライバ(Zadig なし)で、WCH-Link(CH549、fw 2.12)と WCH-LinkE(fw 2.22)を使い、`probe list` / `probe info` / `target info` と、CH32V003 / V103 / L103 / V203 / V307 の flash 往復(読み出し → chip erase → 書込 → readback verify)を確認している(CHANGELOG)。

## 6. 参照

- `crates/usb-wch-win/src/sys.rs`(列挙・open・転送)、`crates/usb-wch-win/src/proto.rs`(IOCTL 番号と `mFunction`)
- `crates/usb/src/device.rs`(`UsbDeviceInfo::open_interface`、`UsbInterface`)
- `crates/wchlink/src/probe.rs`(GetProbeInfo などの実バイト列)
- [architecture.ja.md](architecture.ja.md) §2(crate 構成)
- 先行例: [wlink](https://github.com/ch32-rs/wlink)(`src/usb_device.rs` の `ch375_driver`、`WCHLinkDLL.dll` 経由)、cw2/ch32v003fun の `experimental/minichlink-wchlinkdll-driver`(`minichlink/pgm-wch-linke.c`、IOCTL 直接)
