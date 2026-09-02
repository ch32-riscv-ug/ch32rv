# Windows で WCH 標準ドライバ経由アクセス(WinUSB 非依存)— 作業引き継ぎ

- 状態: **検討中(2026-09-02)**。実機で固まったら英語 main + `.ja` twin に整える。
- 対象読者: Windows ネイティブ(Rust 導入済み)で VSCode を開いて続きを実装・検証する人。
- 目的: **ユーザーが既に入れている WCH 標準ドライバ(`WCHLink_A64`)のまま、Zadig/WinUSB 置換なしで** ch32rv が WCH-Link と通信できる経路を追加する。ArduinoCore-CH32 依頼 B-2 の Windows 対応の核。

## 1. なぜ必要か(WSL 側で実測した事実)

- ch32rv は USB を **nusb(Windows では WinUSB 専用)**で叩く。
- WCH-LinkUtility を入れた Windows では、WCH-Link の **vendor interface(MI_00)に WCH 純正ドライバ `WCHLink_A64`(class `WCH`, oem91.inf)** が当たる(MI_01 CDC は `usbser`=COM)。
- そのため nusb は `failed to open device: incompatible driver is installed for this interface` で **開けない**。libusb(WinUSB/libusbK/libusb-win32 のみ対応)も同様に不可。
- Zadig で WinUSB/libusbK に**置き換えれば**開けるが、それは標準ドライバの上書き = 避けたい方向(WCH-LinkUtility が使えなくなる)。
- → 標準ドライバのまま喋る道は **WCH の CH375 系ドライバの IOCTL を直接叩く**こと。**proprietary DLL は不要**(標準 Win32 + WCH の IOCTL 定数 + GUID のみ)。

現状の Windows での ch32rv 挙動(nusb 経路、リリース 0.2.0 exe を interop 実行で確認済み):
- `probe list` = 列挙は成功(serial/topology 出る)。
- `probe info` / `target info` = `incompatible driver ... for this interface` で失敗(上記のとおり)。

## 2. 先行事例(方式の根拠)

cw2/ch32v003fun, branch `experimental/minichlink-wchlinkdll-driver`, `minichlink/pgm-wch-linke.c`
(コミット「[WIP] Experimental communication via WCH custom driver (Windows only WCHLinkDll.dll)」)。
- <https://github.com/cw2/ch32v003fun/tree/experimental/minichlink-wchlinkdll-driver>
- **[WIP]/experimental** で完動実績は薄い(`UNDONE` コメントあり)。方式の参考として使う。

## 3. 方式の詳細(そのまま実装できる粒度)

### 3.1 デバイスを開く(SetupAPI + CreateFile)
- device interface GUID: **`{F8D5EDCA-B647-4E9C-9BD3-A5BD2328D55C}`**(WCH CH375 系の interface。**実機で `WCHLink_A64` がこの GUID を出すか要確認** — 出なければ Device Manager / `pnputil /enum-devices` で正しい GUID を特定する)。
- `SetupDiGetClassDevs(&GUID, NULL, NULL, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE)`
  → `SetupDiEnumDeviceInterfaces(..., index, ...)`(index=0,1,... で複数 probe)
  → `SetupDiGetDeviceInterfaceDetail`(DevicePath 取得)
  → `CreateFileW(DevicePath, GENERIC_READ|GENERIC_WRITE, FILE_SHARE_READ|FILE_SHARE_WRITE, NULL, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL)` → `HANDLE`

### 3.2 bulk 転送(DeviceIoControl)
- `IOCTL_CH375_COMMAND = 0x223CDC`
  (= `CTL_CODE(FILE_DEVICE_UNKNOWN=0x22, 0x0f37, METHOD_BUFFERED=0, FILE_ANY_ACCESS=0)`
   = `(0x22<<16)|(0<<14)|(0x0f37<<2)|0`)
- コマンド構造体(C):
  ```c
  #define mCH375_PACKET_LENGTH 64
  typedef struct _WIN32_COMMAND {
      ULONG mFunction;                 // 方向|パイプ
      ULONG mLength;                   // データ長
      UCHAR mBuffer[mCH375_PACKET_LENGTH]; // 64
  } WIN32_COMMAND;                     // 先頭 8 byte がヘッダ
  ```
- `mFunction = (pipe - 1) | 方向フラグ`(方向: **write=`0x20000`, read=`0x10000`**)。pipe は endpoint 番号:
  | 論理 | endpoint | mFunction |
  |---|---|---|
  | cmd out (0x01) | pipe 1 | `0x20000` |
  | cmd in  (0x81) | pipe 1 | `0x10000` |
  | data out(0x02) | pipe 2 | `0x20001` |
  | data in (0x82) | pipe 2 | `0x10001` |
- **Write**: `mLength=len; memcpy(mBuffer, data, len);` → `DeviceIoControl(h, IOCTL, pCmd, mLength+8, pCmd, 64, &ret, NULL)`
- **Read**:  `mLength=64;` → `DeviceIoControl(h, IOCTL, pCmd, 8, pCmd, 64+8, &ret, NULL)`; `ret>8 && mLength` なら受信 = `mBuffer[..mLength]`
- **64byte packet 制約**: WCH-Link の data EP は 1 転送最大 4096B。IOCTL は 64B/回なので **64B 単位に chunk 分割**が要る(cmd EP は元々 ~小さいので影響小、data EP=flash 書込で要対応)。要実測(1 IOCTL で 64 超を扱えるか確認)。

## 4. ch32rv への実装方針

- **上位(`ch32rv-wchlink` / `ch32rv-dmi` / `ch32rv-flash`)は無改変**。差し替えるのは USB bulk トランスポートだけ。
- **`ch32rv-usb::UsbInterface` を enum 化**して 2 経路を dispatch(最小改修):
  ```
  enum UsbInterface { Nusb(NusbInterface), Ch375(Ch375Device) }   // write/read/write_data/read_data を分岐
  ```
- **unsafe FFI の隔離**: CH375 コードは `unsafe` 必須。workspace lint は `unsafe_code = forbid`(inner `#[allow]` では外せない)。→ **Windows 専用の別 crate**(例 `crates/usb-wch-win`、`cfg(windows)` のみ、その crate の `[lints]` で `unsafe_code` を許可、`windows-sys` を使う)に閉じ込め、`ch32rv-usb` が cfg(windows) で依存する。architecture §2 の「別バックエンド crate は core 依存を増やさず隔離」枠に合致(旧「nusb のみ/libusb 不採用」規定は新条件で更新可、とユーザー合意済み)。
- **ランタイム選択(Windows)**: まず nusb(WinUSB)で open を試し、`incompatible driver` かつ CH375 GUID で当該 serial が見つかるなら CH375 経路にフォールバック。Linux/macOS は従来どおり nusb のみ。
- 依存: `windows-sys`(SetupAPI / ioapiset DeviceIoControl / fileapi CreateFileW / handleapi CloseHandle。features を追加)。既に依存ツリーに `windows-sys` あり。

## 5. 最小スパイク(まずここだけ、Windows ネイティブ)

目的: 「CH375 IOCTL で WCH-Link と 1 往復できる」を実機で確定する(統合前の feasibility)。

1. 単体の小プログラム(examples/ か別 bin)で:
   - GUID `{F8D5EDCA-...}` を `SetupDiGetClassDevs` で列挙 → 見つかった DevicePath を print(見つからなければ GUID 特定からやり直し)。
   - `CreateFileW` で open。
   - **GetProbeInfo を 1 往復**: cmd out(pipe1 write)で `81 0d 01 01`(SetSpeed/ProbeInfo 相当の既知シーケンス。実際の WCH-Link ハンドシェイクは `ch32rv-wchlink` の `probe_info()` 参照)→ cmd in(pipe1 read)で応答 `82 0d 04 ..` が返るか。
   - 返れば OK。応答バイトを hex で出す。
2. 返らない/開けない場合の切り分け: GUID 違い / IOCTL 値 / pipe 番号 / 64B 制約 / 事例の `-5`(この事例自体 WIP)。

## 6. 検証手順(usbipd の状態が肝)

- **probe を「WCH ドライバが所有」状態にする**: WSL に attach していると usbipd スタブ(VirtualBox USB)が握るので CH375 経路も NG。**`usbipd detach --busid <X>`** で Windows に返す(state=`Shared` = `WCHLink_A64` が当たる。これが CH375 経路のテスト対象)。
- 現状の probe(2026-09-02、`usbipd list`):
  | BUSID | serial | 種別 |
  |---|---|---|
  | 9-2 | 434A124C5596 | CH549 WCH-Link (fw2.12) |
  | 9-3/9-4/11-1/11-2 | 38EF8F06BDC2 ほか | WCH-LinkE (fw2.22) |
  (BUSID→serial は detach して Windows 側 `probe list` に "WCH-Link" ドライバで出た serial で確認。11-2=38EF8F06BDC2 は確認済み。)
- テスト後は **`usbipd attach --wsl --busid <X>`** で WSL に戻す(bench 復旧)。detach 直後は再列挙で一時 flapping するので、`usbipd list` が安定 `Shared` になってから作業する。
- **`--force`(`usbipd bind --force`)は使わない**(スタブを恒久上書きするため。標準状態でない)。
- ネイティブビルド: Windows の Rust で `cargo build -p ch32rv`(msvc)。WSL からのクロスは不要。

## 7. 注意点まとめ

- 事例は WIP。完動保証なし = スパイクで可否を先に固める。
- 64B packet の chunk 分割(特に flash data EP)。
- `unsafe`/`windows-sys` は Windows 専用 crate に隔離(workspace forbid を汚さない)。
- クリーンな Windows(WCH ドライバ未導入)では LinkE が MS OS descriptor で **WinUSB 自動 bind** される可能性が高く、その場合は従来 nusb 経路でそのまま動く。CH375 経路は「WCH ドライバが先に入っている機器」向けの共存策。両対応にしておくと親切。
- 上位プロトコル層は無改変を維持(トランスポート境界だけ差し替える)。

## 8. 参照

- 事例: cw2/ch32v003fun `experimental/minichlink-wchlinkdll-driver` `minichlink/pgm-wch-linke.c`
- ch32rv 既存トランスポート: `crates/usb/src/device.rs`(`UsbInterface::{write,read,write_data,read_data}`、EP 0x01/0x81・0x02/0x82)
- WCH-Link ハンドシェイク: `crates/wchlink/src/probe.rs`(`probe_info`/`attach` の実バイト列)
- architecture §2(別バックエンド crate 方針)、B-2(docs/ch32rv-requests.ja.md 側)
