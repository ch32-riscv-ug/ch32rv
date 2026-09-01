# WCH-Link USB protocol ノート(ch32rv 一次成果物)

- 作成日: 2026-09-01
- 状態: **骨組み**。先行実装からの転記段階であり、実機 capture での裏取りは未着手
- ルール(原設計案 §3): 先行実装(wlink / minichlink / probe-rs / RINS / WCH OpenOCD)は**仕様書として読む**。ここに書く内容は自分の実機 capture で裏を取ってから `verified` にする。**裏の取れていない項目は実装しない**

各表の `状態` 列: `verified`(自前 capture で確認済み)/ `attested`(複数の先行実装が一致)/ `single-source`(単一実装のみ)/ `conflict`(実装間で矛盾。要 capture)/ `todo`(存在の証拠のみ)。

## 1. USB 識別

| mode | VID:PID | interface 構成 | 状態 |
|---|---|---|---|
| RISC-V mode | `1a86:8010` | vendor bulk(MI_00)+ CDC serial | verified(実機 2 台) |
| RISC-V mode(第 2 PID) | `1a86:8011` | 同上 | attested(ch32-device-data `read_link_version.py` が対応。手元に実機なし) |
| ARM/DAP mode | `1a86:8012` | CMSIS-DAP + CDC。**version 照会(`81 0d 01 01`)には EP `0x02`/`0x83` で同一フレームが通る** | attested(同スクリプト実測) |
| IAP mode | `4348:55e0` | WCH factory ISP と同一の bulk 構成 | attested(minichlink / wlink-iap) |

- product string は `"WCH-Link"` または `"WCH_Link"`(probe-rs は両方を受ける)。
- IAP mode は搭載 MCU(LinkE = CH32V305)の factory ISP そのものなので、ISP scan と衝突する。判別は BTVER / chip 種別で行う。

## 2. Endpoint と転送

| 用途 | EP | 状態 | 根拠 |
|---|---|---|---|
| command OUT / IN | `0x01` / `0x81` | **verified**(2026-09-01) | ch32rv 実装で LinkE(FW 2.22)・Link CH549(FW 2.12)の実機 2 台に対し GetProbeInfo が成功。probe-rs `usb_interface.rs:11-12`、wlink protocol.md と一致 |
| data(raw)OUT / IN | `0x02` / `0x82` | **conflict** | probe-rs はコメントアウトのまま未使用。minichlink `pgm-wch-linke.c` は OUT `0x02` / IN `0x81` を使用 |

→ **OQ-1**(縮小): command 経路は `0x01`/`0x81` で確定。残る疑問は data EP `0x02`/`0x82` の使い分け(flash データ転送で使うのか、minichlink が OUT `0x02` を使う理由は何か)。flash 実装時の capture で確定する。timeout は probe-rs が 100ms 固定、ch32rv は 500ms で成功。

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
| `0x0d` | `0x01` | GetProbeInfo(型番・firmware 版)。応答 payload = `[fw_major, fw_minor, variant, fw_mode]`(4 byte)。variant は 1=CH549 / 2,0x12=LinkE / 3=LinkS / 4=DAPLink / 5,0x85=LinkW。fw_mode は 0=RISC-V / 1=ARM(RV/ARM 別 firmware は CH549 のみ) | **verified**(2026-09-01、LinkE=variant 0x12 系・raw `02 16`=2.22、CH549=variant 1・raw `02 0c`=2.12、fw_mode=0 を実機確認) | ch32rv 実装 + probe-rs, wlink, read_link_version.py |
| `0x0d` | `0x02` | AttachChip。応答 payload = `[family, chip_id_be32]`(5 byte)。target 無しの場合は 4 byte 応答または reason `0x55` のエラー応答 | **verified**(2026-09-01: V203C8T6 → family `0x05`・chip_id `0x20310500`、V103R8T6 → family `0x01`・chip_id `0x2500410f`) | ch32rv 実装 + probe-rs, board-identify |
| `0x0d` | `0x03` | RedetectChip。target を **reset せずに** probe に把握し直させる(havereset sticky bit で確認済み)。壊れ読み値(§7)の復旧に使う | attested(board-identify 実測) | board-identify `wch_link.py` |
| `0x0d` | `0xff` | DetachChip(OptEnd)。掴んだ core の解放とセッション前の状態クリアの両方に使う | **verified**(2026-09-01) | ch32rv 実装 + probe-rs, board-identify |
| `0x11` | `0x05` | ChipInfo。**応答はフレームヘッダ無しの生 20 byte**: `[0:2]? / flash_kb(be16, [2:4]) / UUID([4:12]) / protection flags([12:16], 解釈未確立) / chip_id([16:20])`。UUID 全 0/全 ff は未応答 | **verified**(2026-09-01: V203C8T6 → flash 64KiB・UUID `b661abcd1e91bc63`・protection_raw `e339e339`。UUID は board-identify の独立読取と一致) | ch32rv 実装 + board-identify, wlink |
| `0x0d 0x01` | `0x09`/`0x0a` | 3.3V 出力 on/off(`81 0d 01 09` / `0a`) | attested | minichlink `pgm-wch-linke.c:604-613`, wlink |
| `0x0d 0x01` | `0x0b`/`0x0c` | 5V 出力 on/off | attested | minichlink `pgm-wch-linke.c:615-624` |
| `0x0d 0x01` | `0x0f 0x09` | 公式 unbrick | single-source | minichlink(**コメントアウト**。「X シリーズで不安定」と注記あり。採用判断は capture 後) |
| `0x01` | `0x01` | CheckFlashProtection | attested | probe-rs, wlink |
| `0x01` | `0x02` | UnprotectFlash | attested | probe-rs, wlink |
| `0x0b` | - | Reset(target) | attested | probe-rs, wlink |
| `0x0c` | - | SetSpeed(payload `[family, speed]`)。attach 前は family 不明のため `0x01` を送る。speed は high=`0x01` / medium=`0x02` / low=`0x03`(逆順注意) | **verified**(2026-09-01) | ch32rv 実装 + probe-rs |
| `0x08` | - | DmiOp。payload 6 byte `[addr, data_be32, op]`(op=0 nop/1 read/2 write)。応答 6 byte `[addr, data_be32, status]`(status=0 success/2 failed/3 busy)。busy は再試行 | **verified**(2026-09-01: DM 経由で V203/V103 の全 GPR・PC・flash/RAM を読み、wlink dump とバイト一致) | ch32rv 実装 + probe-rs, wlink, RINS |

### 4.1.1 Debug Module 操作(DMI 上の高レベル層。ch32rv-dmi crate、状態: verified 2026-09-01)

wlink `dmi.rs` から転記し実機で確認。DMI レジスタ番地: DMDATA0=`0x04` / DMCONTROL=`0x10` / DMSTATUS=`0x11` / DMABSTRACTCS=`0x16` / DMCOMMAND=`0x17` / DMPROGBUF0=`0x20` / DMPROGBUF1=`0x21`。

| 操作 | 手順 | 状態 |
|---|---|---|
| halt | DMCONTROL に `0x80000001` を書き DMSTATUS の all/any-halted を待つ→`0x00000001` で haltreq クリア | verified |
| resume | DMCONTROL=`0x40000001`(resumereq) | verified |
| read_reg(GPR/CSR/PC) | DMDATA0=0 → DMCOMMAND=`0x00220000\|regno`(GPR=`0x1000+n`, PC=dpc `0x7b1`)→ abstractcs busy 待ち → DMDATA0 読み | verified |
| write_reg(GPR/CSR/PC) | DMDATA0=value → DMCOMMAND=`0x00230000\|regno`→ busy 待ち | verified |
| step(1命令) | dcsr(CSR `0x7b0`)の step(bit2)を立てて write_reg → resume → 再 halt を待つ → step クリア | verified(V203 で PC 前進を確認) |
| ebreak を halt にする | dcsr(`0x7b0`)の ebreakm(bit15)/ebreaks(bit13)/ebreaku(bit12)を立てる。これで各特権 mode の `ebreak` が例外 trap でなく Debug Mode 突入(halt)になる。**SW breakpoint に必須**(未設定だと `continue` で止まらず暴走) | verified(2026-09-01、V203、gdb `continue` が breakpoint で停止) |
| HW trigger 数 | tselect(`0x7a0`)に index を write→read-back で存在確認し、mcontrol を tdata1(`0x7a1`)へ write→read-back で**定着するか**を検査(type field bits[31:28]=2)。**有無は misa/core 世代と無関係で動的検出が必須**(下記) | verified(2026-09-01、5 core 実測) |
| read_mem32 | PROGBUF0=`0x0002a303`(lw x6,0(x5))・PROGBUF1=`0x00100073`(ebreak)→ DMDATA0=addr → DMCOMMAND=`0x00271005`(x5←data0 + postexec)→ DMCOMMAND=`0x00221006`(data0←x6)→ DMDATA0 読み | verified |
| abstractcs | busy=bit12、cmderr=bits[10:8](書き戻しでクリア) | verified |
| DMSTATUS running | allrunning=bit11, anyrunning=bit10, allhalted=bit9, anyhalted=bit8 | verified |

**HW trigger 実測マトリクス(2026-09-01、5 core)**:

| core | family | misa | marchid | trigger slot | 実発火 |
|---|---|---|---|---|---|
| CH32V307 | 0x06 | `0x40901125` | `…d881` | **4** | ✓(pc=0x520 で停止) |
| CH32X035 | 0x0d | `0x40901105` | `…d883` | **4** | ✓(pc=0x416 で停止) |
| CH32V203 | 0x05 | `0x40901105` | `…d882` | **0** | — |
| CH32V003 | 0x09 | `0x40800014` | `…d841` | **0** | — |
| CH32V103 | 0x01 | `0x40101105` | `0`      | **0** | — |

重要: **V203 と X035 は misa 完全一致(`0x40901105`)なのに trigger 有無が逆**(V203=0、X035=4)。marchid の下位だけ違う。→ trigger の有無は misa/命令セット/core letter からは判別できず、**tselect/tdata1 の write-readback による動的検出が唯一の確実な方法**。type field は 2(mcontrol)。空き slot に mcontrol(type2, dmode1, action1=enter-debug, m/s/u, execute)を書き tdata2=addr で execute breakpoint。

**GDB breakpoint の実測(2026-09-01)**: SW breakpoint は対象番地を `ebreak`(4B `0x00100073`)/ `c.ebreak`(2B `0x9002`)で上書きし read-back で着弾を確認する。**着弾しても上記の dcsr.ebreak* を立てていないと `ebreak` が trap し halt しない** — これが SW breakpoint が動かない主因だった。SW(Z0)要求の `break` は「RAM patch → 空き HW trigger〔摩耗なし〕→ flash SW breakpoint〔§4.2.1 の直接 FLASH controller で page を書き換え〕」の順にフォールバックする。

- **HW trigger 経由**: V307/X035 は通常 `break` が flash で発火(実機確認)。
- **flash SW breakpoint 経由(trigger 無し core)**: V203 で通常 `break` が flash 上コードで発火し、複数 `continue`・detach 後の flash pristine 復元まで実機確認。**要点: code は低位 alias(0x0000_0000)で走るが FLASH controller には物理 flash 番地(0x0800_0000+off)を渡す**(alias 番地で erase/program すると効かず `continue` が暴走した)。read は alias/物理どちらでも鏡なので可。**摩耗**: set/clear ごとに page erase+program、step-over は remove+再 insert で 2 回書く。V003(64byte buffered)/V103 は profile 未検証。

trigger も flash profile も無い場合のみ未対応を返す(GDB は "Cannot insert breakpoint"。`hwbreak+` 偽装はしない)。**RV32E(V003、misa.E=bit4)は GPR が x0-x15 のみ**で、x16-x31 を abstract command で読むと cmderr → x0-x15 だけ扱う。

### 4.2 flash 書き込み経路(verified 2026-09-01。wlink から転記し実機確認)

**データ転送は command EP(0x01/0x81)ではなく data EP `0x02`/`0x82` を使う**(OQ-1 解決)。frame 化されず、生バイトを data_packet_size 単位(最終 packet は 0xff pad)で送る。

| cmd | payload | 意味 | 状態 |
|---|---|---|---|
| `0x06` | `0x01` / `0x02` | CheckReadProtect(1=保護/2=非保護)/ Unprotect。保護時のみ解除(option page が消えるため) | verified |
| `0x02` | `0x01` | EraseFlash(chip 全体)→ 後に AttachChip | verified |
| `0x01` | `addr_be32 len_be32` | SetWriteMemoryRegion | verified |
| `0x02` | `0x05` | WriteFlashOP → 直後に data EP へ flash stub を送る | verified |
| `0x02` | `0x07` | 確認(応答 payload[0] が `0x07`) | verified |
| `0x02` | `0x02` | WriteFlash → data EP へ write_pack_size(4096)ごとに chunk 送信、各 chunk 後に data EP から 4 byte ack を読む(`41 01 01 04`、byte3=`0x04` で成功) | verified |
| `0x02` | `0x08` | End | verified |
| `0x0b` | `0x01` | soft reset して実行 | verified |

family 別パラメータ(wlink 由来、実機確認): V003/CH641(family 0x09/0x49、**1線 SWIO**)= stub CH32V003・data packet 64・write pack 1024、V103(0x01)= stub CH32V103・data packet 128・write pack 4096、V20x/V30x(0x05/0x06)= stub CH32V307・data packet 256・write pack 4096。code flash 先頭は共通 `0x08000000`。

実機検証: CH32V203C8T6(LinkE)・CH32V103R8T6(CH549)・**CH32V003F4P6(LinkE、1線 SWIO)** へ Arduino ビルドの blink BIN を flash → readback が BIN とバイト一致 → confirm-run で running 確認。

**1線 SWIO と 2線 RVSWD の差は USB protocol 層には現れない**: attach / DMI / flash のコマンドは同一で、物理配線の差(1線 QingKe V2A の V003 と 2線 の V103/V203)は LinkE firmware が吸収する。ただし 1線 target は LinkE/LinkW のみ対応(旧 CH549 Link は不可)。V003 の attach 応答は family `0x09`・chip_id `0x00300500`(= CH32V003F4P6)を実機確認。

#### 4.2.1 直接 FLASH controller 経路(DMI 経由。page 単位。verified 2026-09-01)

**stub write 経路は部分書き込み不可**: `write_flash`(SetWriteMemoryRegion + stub + chunk)で chip erase 無しに mid-flash の 1 page(256B @0x08000400)を書くと probe が `81 55 01 02`(Protocol reason 0x55)で拒否する。stub 経路は full-region programming(chip erase 後、region=全 image)専用。

そこで **memory-mapped FLASH controller(0x4002_2000)を DMI(progbuf の `read_mem32`/`write_mem32`)で直接叩く** page 単位経路を実装(`DebugModule::flash_page_erase`/`flash_program_page`)。QingKe manual / wlink 参照ブロックの手順:

| reg | 番地 | 用途 |
|---|---|---|
| FLASH_KEYR | `0x40022004` | KEY1=`0x45670123`, KEY2=`0xCDEF89AB` で LOCK 解除 |
| FLASH_STATR | `0x4002200C` | bit0 BUSY / bit1 WRBUSY / bit4 WPRERR |
| FLASH_CTLR | `0x40022010` | bit6 STRT / bit7 LOCK / bit15 FLOCK / bit16 FTPG / bit17 FTER / bit18 BUFLOAD / bit19 BUFRST / bit21 PGSTART |
| FLASH_ADDR | `0x40022014` | 消去/プログラム page アドレス |
| FLASH_MODEKEYR | `0x40022024` | KEY1,KEY2 で FLOCK(fast mode)解除 |

- **unlock**: CTLR&(LOCK\|FLOCK)==0 ならスキップ。else KEYR に KEY1,KEY2 → MODEKEYR に KEY1,KEY2。
- **page erase(全 family 共通)**: unlock → CTLR=FTER → FLASH_ADDR=addr → CTLR=FTER\|STRT → STATR BUSY クリア待ち → CTLR=0 → STATR 書き戻し(EOP クリア)→ lock。WPRERR で write-protect エラー。
- **page program は 2 方式ある**(消去済み前提。unlock 後):
  - **PgStart 方式(V20x/V30x)**: CTLR=FTPG → 4B ずつ write_mem32(各 word 後 WRBUSY 待ち)→ CTLR=FTPG\|PGSTART(bit21)→ STATR BUSY 待ち → CTLR=0 → lock。
  - **Buffered 方式(V003/CH641・X035/CH643・L103)**: CTLR=FTPG → CTLR=FTPG\|BUFRST(buffer reset)→ BUSY 待ち → 各 word: write_mem32(word) → CTLR=FTPG\|BUFLOAD → BUSY 待ち → 全 word 後: FLASH_ADDR=addr → CTLR=FTPG\|STRT(bit6)→ BUSY 待ち → CTLR=0 → lock。minichlink が X035 を V003 と同じ buffered 系に分類しているのが根拠。
- **hart は halt 済みが前提**(progbuf を使うため)。stub 不要=probe 側の 0x55 拒否を回避。

実機検証(2026-09-01):

| family | page | 方式 | 検証 |
|---|---|---|---|
| CH32V20x/V30x(0x05/0x06) | 256 | PgStart | ✓ V203/V307(erase+program+restore) |
| CH32V003/CH641(0x09/0x49) | 64 | Buffered | ✓ V003 |
| CH32X035/CH643(0x0d/0x0c) | 256 | Buffered | ✓ X035 |
| CH32L103(0x0e) | 256 | Buffered | attested(X035 と同 profile、未接続) |
| CH32V103(0x01) | erase 128 / prog 標準 | V103(標準 halfword+commit) | ✓ erase --range・program・gdb flash bp 実機確認(attach quirk は soft-reset で対処) |

**当初 X035 を PgStart 方式で実装したが program が全く効かなかった(erase→FF のまま)**。X035 は buffered 方式が必要と実測で判明し修正(erase は FTER+STRT で全 family 共通なので erase --range は当初から動いていた)。

**CH32V103 の flash(WCH EVT `ch32v10x_flash.c` から確定)**: 他 family と 3 点で違う。(1) **fast erase=128B page(PAGE_ER bit17)/ program は fast buffer でなく標準 16bit halfword(CR_PG bit0)** が確実(fast BufLoad は 128bit=4word 単位で DMI 経由だと corrupt した)→ `sh x7,0(x5)`=`0x00729023` の `write_mem16` を追加。(2) **各 erase/program 後に未文書の commit 副作用が必須: `*(0x40022034) = *((addr & ~3) ^ 0x1000)`**(無いと erase/program が無反応。実測)。(3) 高速化: PG も commit も page で 1 回にして per-word の EVT 手順と等価を確認(gdb Z0 応答を remote timeout 内に収めるため)。実機検証: erase→FF→program a0a1..→erase の往復 OK、`erase --range` surgical(128B、隣接無傷)。**gdb flash bp は V103 固有の attach quirk 対処で動く(root-cause 済み)**: 症状は step→patch→resume で core が trap せず 0x110(default handler の `c.j .`)へ飛ぶこと。mcause/mepc/mtval を読んで判明 — **mcause=4(load-address-misaligned)、mepc=faulting `lw a5,4(s1)`、s1(x9)=chip_id(0x2500410f)**。つまり **WCH-Link の AttachChip が生きた GPR s1/x9 を chip id で上書きし(dscratch にも保存されない=復元不可)**、resume 後に program が s1 を使う瞬間 fault する。attach 直後に既に x9=chip_id。halt→resume だけでも crash(V003/V203/X035 は無事=V103 固有)。**修正: attach 後に soft-reset して program にレジスタを再構築させる**(soft-reset 後 x9 が正常な RAM ポインタに戻り halt/resume が通る)。gdb server は `attach_corrupts_regs` family(V103)で attach 後 soft-reset + 50ms 待ちしてから halt。実機で flash bp が複数 `continue` で発火を確認。単一 step・resumereq クリア・prefetch flush はいずれも無関係だった。

**消去済みセルの debug read 値は family で違う**: V20x/V30x は **`0xe339e339`**(実セルは 0xff だが LinkE の placeholder。power-off erase 後・ChipInfo protection_raw と同値)、**X035/V003 は素直に `0xff`**。→ **erase の成否判定は read 値でなく controller STATR(BUSY クリア + WPRERR 無し)で行う**。この経路は flash SW breakpoint(trigger 無し core)と option byte 書き込みの土台。

### 4.3 特殊消去(SWD ピン共用 target の復旧。verified 2026-09-01)

「Clear All Code Flash」(WCH-LinkUtility の Target タブ相当)。SWDIO/SWCLK を GPIO 等に使うと通常 attach ができなくなるが、これは target を電源/RST で再起動し、app が pin を再構成する前の boot 窓で消去する。**attach しない**(できない target が対象)。

| cmd | payload | 意味 | 状態 |
|---|---|---|---|
| `0x0c` | `family speed` | SetSpeed(先に必要) | verified |
| `0x0d` | `0x0f family` | EraseCodeFlash By Power off。probe が target を電源再投入(**LinkE/LinkW のみ**、target を probe 給電していること) | verified(コマンド受理を実機確認) |
| `0x0d` | `0x08 family` | EraseCodeFlash By RST pin。NRST を使う(RST 配線が要る) | attested(未実機) |

**実測の重要事実**: power-off erase 実行後、flash を debug 経由で読むと `39 e3 39 e3`(= `0xe339e339` の繰り返し)が返る。**これは wlink dump でも全く同じ**(独立ツールと一致)ので ch32rv のバグではなく、power-off erase 後の chip の debug-read 挙動そのもの(ちょうど ChipInfo の protection_raw と同じ値で、保護 fill の可能性)。RAM 読みと DMI 自体は正常。この状態でも **通常 flash を実行すれば即座に復旧**する(erase+program+verify OK を実機確認)。実運用の復旧経路「power-off erase → 通常 flash」は成立する。

### 4.4 monitor(実行時 I/O)

| 経路 | コマンド/機構 | 状態 |
|---|---|---|
| dmdata(SerialDMDATA) | host が DMI で DM data0(0x04)/data1(0x05)を polling。target→host frame: data0 低byte=`0x80\|(count+4)`、上位3B+data1=payload。ACK は data0 に host 入力(bit7 クリア)を書く。**core は running のまま**(attach 後に resume が必要) | **verified**(2026-09-01、CH32V203 で連続受信) |
| sdi enable/disable | **enable=`81 0d 02 ee 00`、disable=`ee 01`**(フラグは直感と逆)。応答 payload[0]=`0x00` 成功/`0xff` 非対応。LinkE 専用 | **verified**(2026-09-01、CH32V203 で `sdi 1,2,3...` 連続受信) |
| uart bridge | probe の CDC port を読むだけ | 実装済み(物理配線が無く未実機確認) |

**SDI enable 手順(usbmon で確定、2026-09-01)**: wlink `sdi-print enable` の実キャプチャと一致させて解決した。手順は GetProbeInfo → SetSpeed(family=`0x01` placeholder)→ AttachChip(family 判明、halt しない)→ **SetSpeed(実 family、例 `0x05`)** → **SDI enable = `81 0d 02 ee 00`**。詰まっていた原因は 2 つ: (1) **enable のフラグが逆**だった(`ee 01` を送っていた=実は disable)、(2) AttachChip 後に実 family で SetSpeed を再送していなかった。enable 後は vendor interface を解放してよい(wlink も終了する)。CDC 読みは **serialport crate が open 時に DTR を assert すると LinkE が forward を 1 行で止める**ため、Linux では生ブロッキングファイル読み(cat 相当)にしている(uart bridge は DTR 無害なので serialport で baud を効かせる)。

### 4.5 存在の証拠のみ(未実装)

`wlink_disabledebug`、`wlink_getromram`(CODE/RAM split)、`wlink_rstout`、`wlink_chip_reset`、`wlink_armversion`、mode 切替、IAP entry(wlink-iap)。→ wlink source / RINS から転記 → capture で verified 化。

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
| 取得 | GetProbeInfo 応答の v_major / v_minor(raw byte) | verified(実機: LinkE raw `0216`→2.22/v42、CH549 raw `020c`→2.12/v32) |
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
| **LinkE の壊れ読み値** | 一部ツールの後、probe が target の壊れた読み値を保持する: family byte は正しいまま chip ID と UUID が同一 32bit word の繰り返しになる。再 attach でも target 電源断でも直らない(**probe 側の状態**)。復旧は RedetectChip(`0x0d 0x03`)+ detach + 再 attach。ChipInfo 応答全体が同一 word 繰り返しかで検出できる | board-identify 実測(ch32rv も同じ検出・復旧を実装) |
| attach の掴み | AttachChip は target core を掴む。セッション終了時は必ず DetachChip で解放する(失敗経路含む) | board-identify 実測 |

## 8. capture 計画(M0-M1)

1. `--capture` 相当の record 機構を最初に作る(usbmon / Wireshark でも代替可)。
2. wlink / probe-rs / WCH-LinkUtility(Windows)それぞれで同一操作(list→attach→flash→reset)を行い、firmware 2.11 / 2.12 / 2.15 で記録する。
3. 記録を fixture 化し、本書の `attested` 項目を `verified` へ昇格。`conflict`(OQ-1)を解消する。

## 9. 参照

- [wlink protocol.md](https://github.com/ch32-rs/wlink/blob/main/protocol.md)
- [RINS: WCH-Link](https://perigoso.github.io/rins/wch-link/index.html)
- minichlink `pgm-wch-linke.c`、probe-rs `probe/wlink/`(転記元)
- `../../../ArduinoCore-CH32/docs/upload-and-fixture.ja.md`(実測 log)
