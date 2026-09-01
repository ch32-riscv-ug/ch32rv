# WCH-Link USB protocol ノート(ch32rv 一次成果物)

- 作成日: 2026-09-01
- 状態: **骨組み**。先行実装からの転記段階であり、実機 capture での裏取りは未着手
- ルール(原設計案 §3): 先行実装(wlink / minichlink / probe-rs / RINS / WCH OpenOCD)は**仕様書として読む**。ここに書く内容は自分の実機 capture で裏を取ってから `verified` にする。**裏の取れていない項目は実装しない**

各表の `状態` 列: `verified`(自前 capture で確認済み)/ `attested`(複数の先行実装が一致)/ `single-source`(単一実装のみ)/ `conflict`(実装間で矛盾。要 capture)/ `todo`(存在の証拠のみ)。

## 1. USB 識別

| mode | VID:PID | interface 構成 | 状態 |
|---|---|---|---|
| RISC-V mode | `1a86:8010` | vendor bulk(MI_00)+ CDC serial | attested(全実装一致) |
| ARM/DAP mode | `1a86:8012` | CMSIS-DAP + CDC | attested |
| IAP mode | `4348:55e0` | WCH factory ISP と同一の bulk 構成 | attested(minichlink / wlink-iap) |

- product string は `"WCH-Link"` または `"WCH_Link"`(probe-rs は両方を受ける)。
- IAP mode は搭載 MCU(LinkE = CH32V305)の factory ISP そのものなので、ISP scan と衝突する。判別は BTVER / chip 種別で行う。

## 2. Endpoint と転送

| 用途 | EP | 状態 | 根拠 |
|---|---|---|---|
| command OUT / IN | `0x01` / `0x81` | **conflict** | probe-rs `usb_interface.rs:11-12`。wlink protocol.md も同様 |
| data(raw)OUT / IN | `0x02` / `0x82` | **conflict** | probe-rs はコメントアウトのまま未使用。minichlink `pgm-wch-linke.c` は OUT `0x02` / IN `0x81` を使用 |

→ **OQ-1**: command と data の EP 使い分け(コマンドはどちらの EP でも受くのか、flash データ転送だけ `0x02` なのか)を capture で確定する。timeout は probe-rs が 100ms 固定、minichlink はより長い。

## 3. フレーム形式

```text
host → probe:  0x81 | cmd | len | payload...
probe → host:  0x82 | cmd | len | payload...   (成功)
```

- 状態: attested(wlink protocol.md / minichlink / probe-rs / RINS 一致)
- エラー応答の形式(先頭 byte、error code 体系): **todo**。要 capture

## 4. コマンド一覧

### 4.1 転記済み(probe-rs `commands.rs` / minichlink / OpenOCD 文字列より)

| cmd | sub | 意味 | 状態 | 根拠 |
|---|---|---|---|---|
| `0x0d` | `0x01` | GetProbeInfo(型番・firmware 版) | attested | probe-rs, wlink |
| `0x0d` | `0x02` | AttachChip(family byte + chip ID 応答) | attested | probe-rs, wlink |
| `0x0d` | `0xff` | DetachChip | attested | probe-rs |
| `0x0d 0x01` | `0x09`/`0x0a` | 3.3V 出力 on/off(`81 0d 01 09` / `0a`) | attested | minichlink `pgm-wch-linke.c:604-613`, wlink |
| `0x0d 0x01` | `0x0b`/`0x0c` | 5V 出力 on/off | attested | minichlink `pgm-wch-linke.c:615-624` |
| `0x0d 0x01` | `0x0f 0x09` | 公式 unbrick | single-source | minichlink(**コメントアウト**。「X シリーズで不安定」と注記あり。採用判断は capture 後) |
| `0x01` | `0x01` | CheckFlashProtection | attested | probe-rs, wlink |
| `0x01` | `0x02` | UnprotectFlash | attested | probe-rs, wlink |
| `0x0b` | - | Reset(target) | attested | probe-rs, wlink |
| `0x0c` | - | SetSpeed(family + 速度段階) | attested | probe-rs。段階は low=400kHz / medium=4MHz / high=6MHz の 3 つのみ |
| `0x08` | - | DmiOp(nop / read / write) | attested | probe-rs, wlink, RINS |

### 4.2 存在の証拠のみ(WCH OpenOCD binary の文字列、wlink 実装)— すべて todo

flash 書込経路: `wlink_ramcodewrite`(flash stub の RAM 転送)、`wlink_fastprogram`、`wlink_ready_write`、`wlink_endprogram`、`wlink_endprocess`。
消去・保護: `wlink_erase`、`wlink_code_erase`、`wlink_flash_protect`。
その他: `wlink_sdi`(SDI print 有効化)、`wlink_disabledebug`、`wlink_getromram`(CODE/RAM split 取得)、`wlink_rstout`(NRST 制御)、`wlink_chip_reset`、`wlink_set_address`、`wlink_speed_div`、`wlink_armversion`、mode 切替、IAP entry(wlink-iap 実装)。

→ それぞれ wlink source / wlink protocol.md / RINS から byte 列を転記 → capture で verified 化、が M0-M1 の作業。

## 5. AttachChip 応答と chip 識別

- 応答に family byte(下表)と 32bit chip ID が含まれる。probe-rs はこれを mask(概ね `0xffffff0f`)で target YAML と照合する。[7:4] は silicon revision で don't-care。
- **OQ-2**: この chip ID とメモリ番地(`0x1FFFF704` 等)の device_id が同一値か → [data-request 0001](../data-requests/0001-device-id.ja.md) で実測依頼。

family byte(probe-rs `wlink/mod.rs:90-128` より転記。状態: attested):

| byte | family | core |
|---|---|---|
| `0x01` | CH32V103 | V3A |
| `0x02` | CH57x | V3A |
| `0x03` | CH56x | V3A |
| `0x04` | CH32F10x | Cortex-M3 |
| `0x05` | CH32V20x | V4B/V4C |
| `0x06` | CH32V30x | V4C/V4F |
| `0x07` | CH58x | V4A |
| `0x09` | CH32V003 | V2A |
| `0x0A` | CH8571 | (undocumented) |
| `0x0B` | CH59x | V4C |
| `0x0C` | CH643 | V4C |
| `0x0D` | CH32X035 | V4C |
| `0x0E` | CH32L103 | V4C |
| `0x49` | CH641 | V2A |
| `0x4E` | CH32V00X | V2C |
| `0x86` | CH32V317 | V4F |
| `0x8B` | CH570/572 | V3C |
| `0xC6` | CH32H4(H415/416/417) | V4F |

- **OQ-3**: gap series(V205/V407/V467/X305/X315/M030/M103)の family byte は上表に無い。既存 family byte に相乗りか新値か → 実機 attach で確定する(M2)。

## 6. firmware 版

| 項目 | 内容 | 状態 |
|---|---|---|
| 取得 | GetProbeInfo 応答の v_major / v_minor(raw byte) | attested |
| 表記の三重性 | raw `02 0c` = 正規化 `2.12` = WCH 表示 `v32`(`major*10+minor`) | attested |
| 既知不良版 | **2.11(v31): `download --reset` 後に target が走らない**(ArduinoCore-CH32 で実測)。2.12 で解消 | verified(実測 log あり) |
| SDI print 要件 | firmware 2.10 以降(wlink README) | single-source |
| probe-rs の版チェックのバグ | `v_major != 2 && v_minor < 7` のため major=2 で素通り。**同じ比較ミスをしないこと**(正規化値で比較 + 単体テスト) | 教訓 |
| hash→版対応 | `ch32-device-data/evidence/link_firmware.csv`(10 行)を照合に使う | データ |

## 7. 実装間で確認された quirk(要 capture 検証)

| quirk | 内容 | 出所 |
|---|---|---|
| DMI NOP | addr=0, val=0 の nop が直前の read 結果を返す前提のハックがある | probe-rs `mod.rs:512-522` |
| resume 後 sleep | DMI write `0x10=0x40000001`(resume)後に 10ms sleep が必要 | probe-rs `mod.rs:526-529` |
| attach 直後のレース | 挿抜直後は CDC が vendor interface より先に enumerate され、その窓で開くと失敗する。1 秒間隔 3 回の retry で回避 | ArduinoCore-CH32 実測 |
| 大 image で固まる | 16.7KB の書込中に bulk timeout → probe が無応答化。USB 再接続でのみ復旧(`USBDEVFS_RESET` 不可) | ArduinoCore-CH32 実測 |
| flash 直後の UART bridge | LinkE の CDC 配送が止まることがあり、port の再 open で直る | ArduinoCore-CH32 実測 |

## 8. capture 計画(M0-M1)

1. `--capture` 相当の record 機構を最初に作る(usbmon / Wireshark でも代替可)。
2. wlink / probe-rs / WCH-LinkUtility(Windows)それぞれで同一操作(list→attach→flash→reset)を行い、firmware 2.11 / 2.12 / 2.15 で記録する。
3. 記録を fixture 化し、本書の `attested` 項目を `verified` へ昇格。`conflict`(OQ-1)を解消する。

## 9. 参照

- [wlink protocol.md](https://github.com/ch32-rs/wlink/blob/main/protocol.md)
- [RINS: WCH-Link](https://perigoso.github.io/rins/wch-link/index.html)
- minichlink `pgm-wch-linke.c`、probe-rs `probe/wlink/`(転記元)
- `../../../ArduinoCore-CH32/docs/upload-and-fixture.ja.md`(実測 log)
