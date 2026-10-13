# factory ISP USB protocol ノート(ch32rv 実装メモ)

- **本書は ch32rv 実装の一次記録**。自分の実機で確かめた事実だけを書き、[`wch-protocols`](../../../wch-protocols/protocols/pc-to-device-isp.ja.md) へ反映して先行実装と突き合わせてもらう。
- 作成日: 2026-10-13
- 実機: WeAct CH32X035 CoreBoard(CH32X035F8U6、debug chip id `0x035e0611`)、RVSWD を OEP probe(ESP32-P4、`30eda0e34a0e`)に、USB を PC に接続。bootloader の BTVER は 2.60。
- 状態: 下表の各項目は **verified**(実機で往復、書いた内容を RVSWD の読み出しで照合)。replay fixture は `tests/fixtures/replay/isp-info-x035.ndjson` / `isp-flash-x035.ndjson`。
- 先行実装(minichlink `pgm-wch-isp.c`、wchisp)は仕様書として読んだだけで、コードは取り込んでいない。

## 1. USB 識別

| 項目 | 値 |
|---|---|
| VID:PID | **`1a86:55e0`**(X035 の bootloader。WCH-Link の IAP mode は `4348:55e0` で、VID が違う) |
| bcdDevice | `26.00` |
| interface | class `0xff`、bulk の組 1 つ(`0x02` OUT / `0x82` IN、64 B) |
| string descriptor | 無し(product / serial とも)。複数台は USB の位置で選ぶ |

## 2. bootloader への入り方(CH32X035)

| 方法 | 結果 |
|---|---|
| debug 線で `FLASH_KEYR` と `FLASH_BOOT_MODEKEYR`(`0x40022028`)にそれぞれ `0x45670123`, `0xCDEF89AB` → `FLASH_STATR` に bit14(`BOOT_MODE`)を足して書く → system reset | **入る**。reset 後 0.5 s 以内に `1a86:55e0` が列挙される。`STATR` は `0xF000`、PC は `0x680` 前後(BOOT 領域) |
| 同じ手順を、hart が flash の中身を実行している状態で、別々の attach に分けて行う | 中身が正規の app なら入る。**flash に実行できないデータ(`pattern-4k.bin`)が入っていると入れない**: reset のたびにそのデータが命令として走り、鍵を触って FLASH をロックする(`CTLR` の LOCK が外れなくなる) |
| reset vector で hart を捕まえて(`ResetMode::HaltAtReset`)から上の手順を 1 回の attach で行う | flash の中身によらず入る。`isp enter --via probe` / `--entry probe` の実装 |

`FLASH_STATR` の上位 4 bit の観測値: app 実行中 `0xB000`、鍵を外した後 `0x3000`、`BOOT_MODE` を書いた後 `0x5000`、bootloader 実行中 `0xF000`。

EVT の `SystemReset_StartMode(Start_Mode_BOOT)`(`ch32x035_flash.c`)は同じレジスタ操作で、app からの切り替え(1200bps touch など)にもこの手順が使える見込み(app からは未検証)。

## 3. 枠

- request: `cmd(1) len(2 LE) payload`。response: `cmd(1) 00 len(2 LE) payload`。

## 4. コマンド(実測の byte)

| cmd | request | response | 備考 |
|---|---|---|---|
| `0xA1` Identify | `a1 12 00` + `00 00` + `"MCU ISP & WCH.CN"` | `a1 00 02 00 5e 23` | chip `0x5e`(debug chip id の 2 byte 目と一致)、type `0x23` = CH32X03x |
| `0xA7` ReadConfig | `a7 02 00 1f 00` | `a7 00 1a 00` + `1f 00` + config 12 B + BTVER 4 B + UID 8 B | 先頭 2 B は送った mask(`1f 00`)の返し |
| `0xA3` IspKey | `a3 1e 00` + 30 B の 0 | `a3 00 02 00 c6 00` | `c6` = key 8 B の和。key は UID の byte 和を 8 つ並べ、最後だけ chip id を足す(seed 全 0 の場合) |
| `0xA4` Erase | `a4 04 00` + sector 数(4 B LE) | `a4 00 02 00 00 00` | 8 sector(8 KiB)で約 25 ms。全 62 sector も受理 |
| `0xA5` Program | `a5 3d 00` + offset(4 B LE、flash 先頭から)+ `00` + 56 B(key で XOR) | `a5 00 02 00 00 00` | image は 256 B 境界まで `0xff` で埋める。**最後に data 0 B の Program**(offset = 末尾)を送り、これも `00` で受理される |
| `0xA6` Verify | Program と同形 | 一致 `00`、不一致は非 0 | 消去後の flash、別内容の image とも非 0 を返すことを確認(値は記録していない) |
| `0xA2` IspEnd | `a2 01 00 01` | `a2 00 02 00 00 00` | **応答を読まないと reset しない**(下記) |

### config 12 B と option bytes の対応

RVSWD で読んだ option bytes(`0x1FFFF800`)は `a5 5a 1f e0 ff 00 ff 00 | ff 00 ff 00 ff 00 ff 00`、ReadConfig の 12 B は `a5 5a 1f e0 ff 00 ff 00 ff ff ff ff`。

| config の byte | 内容 |
|---|---|
| 0–1 | RDPR, nRDPR |
| 2–3 | USER, nUSER |
| 4–5 | DATA0, nDATA0 |
| 6–7 | DATA1, nDATA1 |
| 8–11 | WRPR0–3(**補数 byte は含まない**) |

UID(ReadConfig の最後の 8 B)は ESIG の `0x1FFFF7E8` から読んだ 8 B と一致。

## 5. 終わり方と時間

- `IspEnd(01)` を送って応答を読むと、約 200 ms で bootloader は `BOOT_MODE` を解除して app へ reset する(`STATR` が `0xB000` に戻る)。
- **応答を読まずに終えると、bootloader は応答の送信待ちのまま残る**。その後の session では Identify には答えるが、ReadConfig の OUT が stall する。最後は下の timeout で app へ抜けた。
- 最後の通信から **約 60 s** 何も来ないと、bootloader は `BOOT_MODE` を解除して app へ抜ける。
- bootloader 実行中に debug 線で hart を halt したままにすると、USB の OUT がすべて timeout する(USB は CPU が処理しているため)。

## 6. 未確認

- Boot キー(PC17 を電源投入時に High)での入り方と、キーを離す時機。
- app のコードからの切り替え(EVT の `SystemReset_StartMode` + `NVIC_SystemReset`)。
- WriteConfig(`0xA8`)、data flash、UART 経路。
- WCH-Link を使った debug 線からの入り方(ch32rv は今は OEP の probe だけ)。
