# 一貫性 総点検(破壊的変更フリーズ前)

crates.io 公開ライブラリ・CLI・JSON contract の仕様を横断点検し、統一が取れていない/仕様が単純でない箇所を洗い出したもの。**破壊的変更が許容される最後の窓**での意思決定材料。所見は 4 観点(exit code/ErrorKind、JSON envelope、CLI 引数、ライブラリ公開 API)の横断調査に基づく。file:line は点検時のもの。

## まず「壊すべきでない」もの(健全な土台)

- **exit code テーブルは整合**: 番号重複なし・帯内に欠番なし(`crates/contract/src/exit.rs`)。`ErrorKind`(細分)→`ExitCode`(粗)の写像も一貫。
- **エラー型規約は一貫**: 全 crate が `〜Error` + `thiserror` + `#[non_exhaustive]`。公開の非テスト経路に `unwrap/expect/panic` なし。
- **型の基本形は統一**: target 番地は一様に `u32`、host 長は `usize`。getter に `get_` 接頭辞なし。envelope の最上位キーと大半の `result` キーは snake_case。

問題は「土台」でなく「土台の使い方のブレ」と「同一概念の別名・別形」に集中している。

---

## テーマ別 所見(破壊度でランク)

### T1. サイズ単位・数値表現の三重化 【P0・cross-crate・破壊】
同じ物理量が 3 通りの単位/幅で公開されている。
- `target::SkuRecord.flash_bytes: u64` / `sram_bytes: u64`(bytes)
- `wchlink::ChipInfo.flash_kb: u16`(KiB)
- `contract::TargetReport.flash_kb: Option<u32>`(KiB)
- `target::FlashGeometry` は all-bytes `u32`
- JSON でも `target.info`=`flash_kb`(KiB)、`db.list/info`=`flash_bytes`/`sram_bytes`(bytes)。

**統一案**: 内部は **bytes + 単一幅(u32)** に統一(`flash_bytes`/`sram_bytes`)。KiB は表示境界で導出。JSON も `*_bytes` に寄せる。

### T2. `FlashParams` の二重定義 【P0・cross-crate・破壊】
`wchlink::FlashParams`(`probe.rs:34`)と `flash::FlashParams`(`flash/src/lib.rs:22`)がフィールド重複で別定義。`flash::params_for_family` で解決した値を `WchLink::write_flash` に渡すには**手作業で組み直し**が要る(cmd_flash/cmd_run で実際にやっている)。

**統一案**: 正準 `FlashParams` を 1 つ(`flash` 所有、または `contract` へ)。`wchlink` はそれを使う。

### T3. メモリ read/write API の綴り違い＋引数順逆転 【P0・cross-crate・破壊】
- read: `dmi::DebugModule::read_mem(addr,len)` vs `wchlink::read_memory(address,len)` — `mem`/`memory` の綴り違い。
- write: `dmi::write_mem(addr,data)`/`write_mem32`/`write_mem16` は **addr 先** だが `wchlink::write_flash(data,address,params,progress)` は **data 先**。引数順が逆。
- `read_mem32`/`write_mem32`/`write_mem16`(単語/半語)は別 op なので別名は妥当。

**統一案**: byte 範囲は `read_mem`/`write_mem(addr, data)` を house form として両 crate 統一。`address`→`addr` に統一(T7 と同時)。

### T4. contract キー名・符号化のブレ 【P1・JSON 破壊・下流 Arduino に影響】
同一概念が別キー/別型/別符号化。**`result` は free-form なので clap/schema 検証が効かず、ここが最大の温床**。
- 番地: `addr`(write, dbg dmi read) vs `start`(read, blank-check, erase)。
- chip 名: recover power-off/nrst=`chip` vs recover unprotect/unbrick・他=`family`。
- erase scope: flash=`erase` vs erase cmd=`scope`。
- verify 結果: flash=`verify:bool` vs verify cmd=`match:bool` vs write=`verified:bool`。
- firmware 版: 型 `norm` vs `probe.firmware.info`=`version` vs `check`/`capabilities`=`firmware`。
- firmware mode: 型/`info`=`mode` vs `probe.mode.get`=`firmware_mode`(かつ `mode` は probe mode と二重語義)。
- 単位: T1 参照。
- on/off の符号化が **3 通り**: 文字列 `"on"/"off"`(option get read_protection)/bool(write_protected, sdi, mode.set changed)/散文 `"3v3 on"`(probe power)。**同一オブジェクト内で文字列と bool が混在**。
- 散文を機械値に: `recover.note`/`action`、`option.note` の英文。
- `probe.list` が成功 envelope の `result.probes[i]` に文字列 `error` を注入 → 最上位 `error:ErrorBody`(構造体)と語義衝突。

**統一案**: キーを 1 つに正規化(`addr`/`family`/`scope`/`verified`)、firmware 版は `norm`、firmware mode は常に `firmware_mode`。二値は必ず **bool**、power は `{rail:"3v3", on:true}`。散文は構造化フィールド + 別 `message`。probe 単位のエラーは別キー名か構造化。→ **contract を 2 へ bump**。

### T5. `--json` で envelope を出さない終端がある 【P1・JSON 破壊(欠落)】
`cli.json` 分岐が無く人間向けテキスト+`ExitCode::SUCCESS` で終わる:
- `dbg reg write`(`cmd_dbg.rs:640`)/`dbg dmi write`(`:714`)— read 側は出す。
- `target protect on/off` の「already ON/OFF」短絡(`cmd_target.rs:718,731`)— `probe mode set` の「already」は出す。
- `monitor uart/sdi/dmdata/rtt` のストリーム終了(`cmd_monitor.rs:178,374,575`)。
- fail() を経由せず独自 stderr 行: verify `MISMATCH`(`cmd_flash.rs:1146`)、blank-check `NOT BLANK`(stdout、`cmd_dbg.rs:380`)、reset not-running(`:1064`)等。

**統一案**: 全終端の成功/失敗を `cli.json` で envelope 化。「already」短絡は `{changed:false}` の成功 envelope。失敗描画は 1 つの renderer に集約。

### T6. `cmd` 文字列の粒度が実装 vs `canonical_name` で不一致 【P1・JSON 破壊】
実装ハンドラの `CMD` 定数と `main.rs::canonical_name`(未実装経路のみ使用)が食い違う:
- `probe power`=`probe.power` vs canonical=`probe.power.3v3/.5v/.cycle`。
- `dbg reg`=`dbg.reg` vs `dbg.reg.read/.write`。`dbg dmi` 同様。

つまり安定識別子 `cmd` の値が「実装済みか」で変わる。**統一案**: 1 関数から両者を駆動し、ハンドラも細粒度ドット名を出す。

### T7. exit code/ErrorKind の使い分けと死蔵 【P1・一部破壊/一部 doc】
- **死蔵**: `TargetNoResponse`(20)/`TargetNotInDb`(20)/`TargetProtected`(21)/`ProbeWedged`(41) は**どこからも生成されない**。docs §3.6/§3.7 は 20(応答なし/DB 無しを JSON 区別)・41(固まり検出)を約束するが未実装。protected target も実際は 22/40 で出る。→ **配線するか、コード 20/21/41 と kind を削除して docs 修正**。
- **13(device-busy)の二義**: 本来 lock timeout 専用だが、USB open/probe-info の Display に `"busy"` が含まれると**部分文字列一致**で 13 に化ける。→ 13 は `LockError` 専用に。
- **open エラー分類が 3 実装**(`session_error`/`cmd_dbg::open_session`/`cmd_probe::info`)で**部分文字列マッチ**、しかも結果が食い違う(`ProbeInfo` に "busy" 含む → 共有経路 13 / dbg 経路 11)。→ typed `WchLinkError`/`UsbError` を見る単一 `classify_open_error` に。
- **40(transport-timeout)が万能 catch-all**(~38 箇所): program/erase/readback/option 書込/soft_reset/power/GPR 読み等、大半は timeout でない。真の timeout は run の 1 箇所のみ。→ 汎用 `TransferFailed` を分離、または 40 を「転送/DMI 失敗一般」に再定義。
- **誤分類**: 入力 read 失敗=`Usage`(2)/出力 write 失敗=`Internal`(70)=bug コード/USB 列挙失敗=`Internal`(70)/`doctor` 失敗=`DeviceOpenFailed`(11)。→ 環境エラーは環境コードへ。
- **`Unimplemented` と `Internal` が両方 70**(「未実装」と「バグ」を exit code で区別不可)。→ 併合を受け入れ Unimplemented を廃止、または別コード。
- **option set/write-raw/reset は readback 不一致を成功+warning(exit 0)扱い**、flash/verify/write は `VerifyMismatch`(30)。script が exit 0 で通ってしまう。→ 30 で失敗に。
- **run の「programmed but not running」は 22**、flash/reset は `NotRunningAfterWrite`(50)。→ run も 50 に。
- **`ExitCode` と `ErrorKind` の語幹ドリフト**(`DeviceOpen`↔`DeviceOpenFailed`、`NotRunning`↔`NotRunningAfterWrite`、`Unsupported`↔`CapabilityUnsupported`)— 数値/kebab は凍結済みなので識別子だけ揃えるのは無害。

### T8. CLI 引数の語彙分裂 【P2・flag 破壊・ユーザー面】
- **番地が 4 形式**: flash/verify=`--offset`、write=`--at`、dfu=`--address`、read/erase=`--range`。→ base は `--at <addr>`、範囲は `--range`(共有 `parse::range`)に統一。
- **region が enum(flash/verify)と自由文字列(read/erase/write)の混在**。→ 単一 `Region` enum + `[+off[+len]]`。しかも flash の enum は `code|system` しか受けないのに 5 変種を宣言(過大広告)。
- **policy enum のクローン**: `WriteErase`==`IspErase`(バイト一致)、`VerifyMode{readback,crc,none}` vs `IspVerify{on,none}`、`ResetPolicy` vs `IspReset`。→ 各 1 つに統一し、部分対応は parse 時に値集合を制限。
- **`--timeout` の語義過負荷**: transport(既定 3s)/monitor 実行長/run semihosting cap(既定 **60s**)。同じ未指定フラグで 20 倍違う。→ transport 専用にし、実行長は別 `--duration`。
- **`--exit-on` だけ手書き自由文字列**(他ポリシーは ValueEnum)。→ enum + 数値 duration。
- **monitor 転送の名前**: `monitor --source`/`run --source` vs `flash --monitor`。→ 名前統一。
- **`--format hex` の二義**: flash=Intel HEX 入力、read=hex ダンプ。Intel HEX は read では `ihex`。→ read のダンプを `hexdump` に改名、Intel HEX は一語に。
- **`--port` 過負荷**: serial 選択子(monitor/isp/boot) vs dap の TCP `u16`。→ dap は `--listen`/`--tcp-port`(gdb に合わせる)。
- **入力/出力ファイルが positional と flag の混在**: 入力は大半 positional だが `firmware update --image`。出力は `read -o` vs `isp eeprom read <out>`。→ 入力は positional、出力は `-o` に統一。
- **boolean 極性の混在**: `--no-flash`/`--no-reconnect`(負) vs 多数の正形。かつ `--no-reconnect` は spec §4.5 の `--reconnect`(既定 on)と食い違い。→ spec と一致させ極性規約を 1 つに。
- **operation picker が subcommand と `--method` flag の混在**、かつ `boot enter --method` は自由文字列。→ `recover` を subcommand 化するか統一し、`boot enter --method` は即 enum 化。
- **device 選択子の名前**: probe=`--probe`、isp=`--device`、boot=`--usb-id`。→ 少なくとも isp/boot を揃える。
- 低優先: `off` vs `none`(toggle vs policy)、`--baud` 既定差、`erase --range` help が `+len` を書いていない、`--chip`/`--core` の略語混在。

### T9. ライブラリ規約(揃える or 明文化) 【P2・一部破壊】
- **progress/cancel が規約通り適用されていない**: `contract/progress.rs` は「全長時間 op が `&dyn ProgressSink`+`&CancelToken` を取る」と明記だが、`write_flash` は自前クロージャ `impl FnMut(u64)`、`read_mem`/`write_mem`/`read_memory` はどちらも取らない。`DmiError::Cancelled` は生成不能。→ 署名を規約に合わせる、または `Cancelled` を削除。
- **enum→文字列が 3 形**: `Variant::name()->String`(確保) vs `FwMode::as_str()->&'static str` vs 自由関数 `family_name()`。→ `as_str(&self)->&'static str` に統一。
- **constructor 動詞**: `open`(HW)/`acquire`(lock)/`new`(純)/`builtin`/`parse` — 概ね妥当だが `WchLink::open` vs `DebugModule::new`/`Ch32Target::new`(いずれも下位資源を包む)は要判断。
- **pub フィールド vs accessor**: 大半は plain data で pub フィールド(妥当)。ただし不変条件を持ちうる `FlashParams`/`FlashCtrlProfile` は要検討。**規約として明文化**(後で accessor 化は破壊)。
- **`usb::capture` の `record`/`Chan`/`Dir` が pub**(内部 hook)。→ `pub(crate)` に。
- **`usb` crate が 4 エラー型**(`UsbError`/`LockError`/`ResolveError`/`SelectorParseError`)、`flash` は crate 級エラー無しで `ImageError` のみ。→ 「1 crate 1 エラー」か「module 別エラーを意図」と明文化。
- **timeout モデル**: `UsbInterface::read/write(data,timeout)`(毎回) vs `WchLink::set_timeout`(状態)。→ 規約化。
- `chip_info()->ChipInfoStatus`(status ラッパ) vs `probe_info()->ProbeInfo`(素)— 命名が対称性を誤示唆。
- `debug` crate が自前エラー無しで `DmiError` を再利用(将来自前失敗が要れば破壊追加)。

---

## 「今やる」推奨(フリーズ前・破壊度×巻き戻し困難度)

**P0(cross-crate、公開後の巻き戻しが最も高くつく)**
1. `FlashParams` 一本化(T2)
2. サイズ単位を bytes+u32 に統一(T1)
3. メモリ read/write の綴り+引数順統一(T3)

**P1(contract を 2 へ bump。下流 Arduino tooling に効く)**
4. `result` キー正規化・二値 bool 化・散文の構造化(T4)
5. 全終端で `--json` envelope、`cmd` を単一ソース化(T5, T6)
6. exit code/ErrorKind 整理: 死蔵 20/21/41 の去就、13 を lock 専用、40 の分離、誤分類是正、option-set 不一致を 30 に、run not-running を 50 に(T7)

**P2(CLI flag・ライブラリ規約。ユーザー面 & 明文化)**
7. 番地/region/policy enum/`--timeout`/`--source`/`--format` の統一(T8)
8. progress-cancel 適用、`as_str` 統一、capture を pub(crate)、pub-field 規約明文化(T9)

**非破壊で今すぐ実施可(安全)**: `cmd_dbg::open_session` を `session_error` 呼び出しに置換(重複解消)、hint 文字列を `ErrorKind` 別 const に集約、`usb::capture::record` を pub(crate) 化、`ExitCode`/`ErrorKind` の語幹統一。

## 実施状況(この総点検を受けた対応)

**実施済み(破壊的、CONTRACT_VERSION 1→2、全 gate green + 実機検証)**:
- P0: T1 サイズ bytes+u32 統一 / T2 `FlashParams` 一本化(wchlink 所有 + `CODE_FLASH_START` 定数)/ T3 `read_mem` 統一・`write_flash(addr,data,…)` 引数順。
- P1(exit code): `transfer-failed`(40)新設で操作失敗と真の timeout を分離、`13` を lock/型付き-busy 専用(部分文字列廃止)、無応答 attach=`target-no-response`(20)、option-set 不一致=`verify-mismatch`(30)、run 異常 halt=`not-running-after-write`(50)、file/enumeration を bug コードから分離、`ExitCode`/`ErrorKind` 語幹統一、`cmd_dbg` の重複分類を `session_error` に集約。
- P1(JSON): `flash_bytes`/`addr`/`scope`/`verified`/`firmware`/`firmware_mode`/`family` へ統一、on/off を bool・power を `{rail,on}`、`probe_error`、envelope 重複削除、欠落 envelope(dbg write / protect no-op)を追加、`cmd` 粒度を単一化。
- P2(CLI): `--at`(flash/verify)、global `--duration`(monitor/run の実行長)で `--timeout` を transport 専用化、`run --exit-on` を enum 化、`read --format hex-dump`。
- 非破壊: `capture::{record,Chan,Dir}` を pub(crate)。

**据え置き(理由あり)**:
- 死蔵 exit 21/41: 番号は凍結契約なので削除せず「予約(未発行)」と明記(protected 検出・wedged 検出は実機検証困難のため未配線)。
- Region enum 統一(read/erase/write): `region[+off[+len]]` サフィックスがあり ValueEnum 化不可。String のまま(将来 `RegionSpec` FromStr 型で対応可)。
- policy enum クローン(`WriteErase`/`IspErase` 等): コマンド別 subset は clap 検証を保つため妥当と判断。
- `flash --monitor` vs `--source`: 「flash 後に monitor」の意図が読めるので現状維持。
- isp/boot/dap のフラグ整形: 未実装(stub)のため実装時に確定(まだ live 契約でない)。
- ライブラリ規約(progress/cancel の全面適用、`Variant::name`=data 保持 enum で String 妥当、constructor 動詞): 別途 doc で規約明文化予定。

## 波及先(仕様の追従が要る)
`docs/cli.ja.md`(§3 exit code / §4 各コマンド)、`docs/contract/result.schema.json`(+ per-command result schema)、`CONTRACT_VERSION`(1→2)、`CHANGELOG`、ArduinoCore-CH32 の JSON 消費側。
