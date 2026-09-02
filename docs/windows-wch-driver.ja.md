# Windows で WCH 標準ドライバ経由アクセス(WinUSB 非依存)— 作業引き継ぎ

- 状態: **検討中(2026-09-02)**。**§5 の最小スパイクは同日 Windows 実機で成功**(5/5 probe、§5.1)。
  実装方針が固まったら英語 main + `.ja` twin に整える。
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

## 2. 先行事例サーベイ(2026-09-02、実ソース確認)

**「有名ツールは WCH 標準ドライバ非対応」は誤りだった。wlink は対応済み。** まとめ:

| ツール | 言語 | WCH 標準ドライバ経路 | 方式 |
|---|---|---|---|
| **wlink**(ch32-rs/wlink) | **Rust** | **対応済み** | **`WCHLinkDLL.dll`(WCH の DLL)を libloading で読み込み、CH375* stdcall API。`USBDeviceBackend` trait で nusb 系と切替。`cfg(target_os="windows", target_arch="x86")`= 32bit 限定** |
| minichlink fork(cw2/ch32v003fun `experimental/...`) | C | 対応(WIP) | **DLL 不要**。WCH ドライバの **IOCTL を DeviceIoControl 直叩き**(§4) |
| minichlink 上流(cnlohr/ch32fun) | C | ✕ | libusb のみ(`CH375`/`DeviceIoControl` 参照 0、`libusb_open` のみ) |
| probe-rs | Rust | ✕ | nusb=WinUSB のみ(公式ドライバでは動かず Zadig 必須と明記) |
| wchisp | Rust | ✕ | WinUSB/libusb |

- wlink #37「Official Windows Driver support」(ユーザー要望「libusb/winusb のインストールは受け入れられない」)→ maintainer が **「The basic Windows driver is done.」で実装・close**。→ **ch32rv は Rust なので wlink が直接の参照実装**になる。
- 生態系の共通認識:**WCH 純正ドライバと WinUSB は排他**(片方を Zadig で入れると WCH-LinkUtility 側が使えなくなる)。だから「標準ドライバのまま使いたい」需要が実在し、wlink がそれに応えた。

### 2 つの実装アプローチ
- **A) WCH の DLL(`WCHLinkDLL.dll`)経由**(= wlink)。API は明快・WCH 提供で安定。**ただし DLL が 32bit stdcall = 呼ぶ側も 32bit(x86)でないとロード不可**(64bit プロセスは 32bit DLL を読めない)。ch32rv は x86_64 配布なので、この経路のために 32bit ビルドを別途出すか、**64bit の WCHLinkDLL が提供されているか要確認**。
- **B) IOCTL 直叩き(`DeviceIoControl`)**(= minichlink fork)。**DLL 不要・arch 非依存(64bit で動く)**。ただし IOCTL 定数は WCH ドライバのリバース(§4)。**ch32rv の 64bit 配布と相性が良いのはこちら。**

判断材料: **64bit のまま出したいなら B、WCH の supported API に乗るなら A(要 32bit or 64bit DLL 確認)**。まず A の DLL の bitness と 64bit 版有無を実機で確認するのが早い。

**→ 確認済み(2026-09-02、実機)**: `WCHLinkDLL.DLL` は
`C:\WINDOWS\System32\DriverStore\FileRepository\wchlinkwdm.inf_amd64_28c146f8d6c53b69\`(ドライバパッケージ同梱)と
`C:\WINDOWS\SysWOW64\`(= 32bit 配置)の 2 箇所にあり、**PE ヘッダはどちらも x86(32bit)。64bit 版は存在しない**
(amd64 パッケージでもカーネルドライバ `WCHLinkW64.SYS` だけが 64bit で、DLL は 32bit のまま)。→ **B(IOCTL 直叩き)を採用**。

### wlink の DLL API(A を採る場合の参照)
`WCHLinkDLL.dll`(全て `extern "stdcall"`):
- `CH375OpenDevice(iIndex: u32) -> handle(u32)` / `CH375CloseDevice(iIndex)`
- `CH375GetDeviceDescr(iIndex, *UsbDeviceDescriptor, *len) -> bool`(VID/PID で選別)
- `CH375ReadEndP(iIndex, ep: u32, *buf, *len) -> bool` / `CH375WriteEndP(iIndex, ep: u32, *buf, *len) -> bool`(ep に 0x81/0x01/0x82/0x02 を渡す)
- `CH375SetTimeoutEx(...)` / `CH375GetVersion()` / `CH375GetDrvVersion()`
- wlink 実装: `src/usb_device.rs` の `mod ch375_driver`(<https://github.com/ch32-rs/wlink/blob/main/src/usb_device.rs>)。`USBDeviceBackend { open_nth, read_endpoint(ep,buf), write_endpoint(ep,buf), ... }` trait で nusb backend と統一。

**注**: `CH375*` は WCH の CH375 汎用 USB ドライバ API。EP 番号ベースなので ch32rv の `UsbInterface`(EP 0x01/0x81/0x02/0x82)にほぼ 1:1 で対応する。

## 3. アプローチ B の詳細(IOCTL 直叩き、DLL 不要・64bit 可)

### 3.1 デバイスを開く(SetupAPI + CreateFile)
- device interface GUID: **`{F8D5EDCA-B647-4E9C-9BD3-A5BD2328D55C}`**(WCH CH375 系の interface)。
  **→ 実機確認済み(2026-09-02)**: `pnputil /enum-interfaces` で、接続中の全 WCH-Link MI_00 にこの GUID の
  interface が Enabled で存在(5 台分)。なお INF(`wchlinkwdm.inf`)はレジストリ経由で第 2 の GUID
  `{CDB3B5AD-293B-4663-AA36-1AAE46463776}`(`DeviceInterfaceGUIDs`)も登録するが、F8D5EDCA 側で動作する。
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
  (事例の出力バッファ長 `64` は構造体 72B と不整合に見えるが、**スパイクでは `64+8` を渡して動作確認済み**。write の `ret` は 12 = ヘッダ 8 + 送信 4 だった)
- **Read**:  `mLength=64;` → `DeviceIoControl(h, IOCTL, pCmd, 8, pCmd, 64+8, &ret, NULL)`; `ret>8 && mLength` なら受信 = `mBuffer[..mLength]`
- **64byte packet 制約**: WCH-Link の data EP は 1 転送最大 4096B。IOCTL は 64B/回なので **64B 単位に chunk 分割**が要る(cmd EP は元々 ~小さいので影響小、data EP=flash 書込で要対応)。要実測(1 IOCTL で 64 超を扱えるか確認)。

## 4. ch32rv への実装方針

- **参照実装 = wlink**(Rust、同じ EP ベース)。`USBDeviceBackend { open_nth, read_endpoint(ep,buf), write_endpoint(ep,buf), .. }` trait 構成をほぼ踏襲できる。**A/B は B(IOCTL)に確定**(DLL は 32bit のみ、§2。feasibility は §5.1 で実証済み)。
- **上位(`ch32rv-wchlink` / `ch32rv-dmi` / `ch32rv-flash`)は無改変**。差し替えるのは USB bulk トランスポートだけ。
- **`ch32rv-usb::UsbInterface` を enum 化**して 2 経路を dispatch(最小改修):
  ```
  enum UsbInterface { Nusb(NusbInterface), Ch375(Ch375Device) }   // write/read/write_data/read_data を分岐
  ```
- **unsafe FFI の隔離**: CH375 コードは `unsafe` 必須。workspace lint は `unsafe_code = forbid`(inner `#[allow]` では外せない)。→ **Windows 専用の別 crate**(例 `crates/usb-wch-win`、`cfg(windows)` のみ、その crate の `[lints]` で `unsafe_code` を許可、`windows-sys` を使う)に閉じ込め、`ch32rv-usb` が cfg(windows) で依存する。architecture §2 の「別バックエンド crate は core 依存を増やさず隔離」枠に合致(旧「nusb のみ/libusb 不採用」規定は新条件で更新可、とユーザー合意済み)。
- **ランタイム選択(Windows)**: まず nusb(WinUSB)で open を試し、`incompatible driver` かつ CH375 GUID で当該 serial が見つかるなら CH375 経路にフォールバック。Linux/macOS は従来どおり nusb のみ。
- 依存: `windows-sys`(SetupAPI / ioapiset DeviceIoControl / fileapi CreateFileW / handleapi CloseHandle。features を追加)。既に依存ツリーに `windows-sys` あり。

## 5. 最小スパイク(まずここだけ、Windows ネイティブ)

目的: 「WCH 標準ドライバ経由で WCH-Link と 1 往復できる」を実機で確定する(統合前の feasibility)。
**→ 完了(2026-09-02)。結果は §5.1。**

0. **A/B を決める**: `WCHLinkDLL.dll` の有無と bitness を確認(`where WCHLinkDLL.dll`、`dumpbin /headers <dll> | findstr machine`。WCH-LinkUtility 導入で入る)。**32bit のみなら B(IOCTL、64bit ch32rv でそのまま可)**、64bit DLL もあるなら A(wlink 実装をほぼ移植)。A なら wlink `src/usb_device.rs` の `ch375_driver` を Rust でそのまま参考にできる(ただし 32bit ビルドが要る)。以下は B(IOCTL)での最小確認手順。
   **→ 済: 32bit のみ(§2)= B 採用。**
1. 単体の小プログラム(examples/ か別 bin)で:
   - GUID `{F8D5EDCA-...}` を `SetupDiGetClassDevs` で列挙 → 見つかった DevicePath を print(見つからなければ GUID 特定からやり直し)。
   - `CreateFileW` で open。
   - **GetProbeInfo を 1 往復**: cmd out(pipe1 write)で `81 0d 01 01`(= GetProbeInfo。`ch32rv-wchlink` の `probe_info()` と同一バイト列。crates/wchlink/src/probe.rs 参照)→ cmd in(pipe1 read)で応答 `82 0d 04 ..` が返るか。
   - 返れば OK。応答バイトを hex で出す。
2. 返らない/開けない場合の切り分け: GUID 違い / IOCTL 値 / pipe 番号 / 64B 制約 / 事例の `-5`(この事例自体 WIP)。

### 5.1 スパイク結果(2026-09-02、Windows 11 実機)

**成功。接続中 5 probe すべてで、WCH 標準ドライバ(`WCHLinkW64.SYS`)経由の GetProbeInfo 1 往復が成立した。**
64bit(x86_64-pc-windows-msvc)プロセス・WinUSB 置換なし・DLL なし・管理者権限なし。

| probe | 応答(hex) | firmware |
|---|---|---|
| WCH-Link(CH549)×1 | `82 0d 04 02 0c 01 00` | v2.12(variant 0x01) |
| WCH-LinkE ×4 | `82 0d 04 02 16 12 00` | v2.22(variant 0x12) |

確定した事実(§3 の「要確認」への回答):
- GUID `{F8D5EDCA-...}` で列挙・`CreateFileW` open とも成功(全台)。列挙は SetupDi ではなく
  cfgmgr32 の `CM_Get_Device_Interface_ListW` でも同等に動く(コード量が少ない。実装時はどちらでも可)。
- `IOCTL_CH375_COMMAND = 0x223CDC`、`mFunction`(write=`0x20000`/read=`0x10000`、pipe-1 加算)、
  ヘッダ 8B の構造体レイアウトは §3.2 のとおりで正しい。
- write の DeviceIoControl は `ret = 8 + 送信長` を返す。read は `ret = 8 + mLength` で
  `mBuffer[..mLength]` が応答フレームそのもの。
- probe は usbipd `Shared` 状態(WSL 未 attach)なら WCH ドライバが所有しており、detach 操作は不要だった。
- 未確認のまま残る項目: **data EP(pipe2)の 64B 超転送**(flash 書込で必要。1 IOCTL で 64B 超を
  扱えるか、chunk 分割が要るか)。attach + flash 実書込を伴うため統合実装時に確認する。

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
- architecture §2(別バックエンド crate 方針)、B-2(`../../ArduinoCore-CH32/docs/ch32rv-requests.ja.md` — ArduinoCore-CH32 repo 側の依頼文書)
