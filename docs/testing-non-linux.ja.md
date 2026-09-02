# 非 Linux 環境でのテスト手順(Windows / macOS)

Windows / macOS で ch32rv を実機テストする手順。開発機は Linux(WSL2)なので、これらは verified 昇格のための持ち回りテストに使う(Windows x64 は verified、macOS / arm は experimental)。

## 1. ch32rv バイナリの入手

どちらか:

- **リリース配布物**(ツールチェーン不要): [Releases](https://github.com/ch32-riscv-ug/ch32rv/releases) から対象アーカイブを取得。
  - Windows: `ch32rv-<ver>-x86_64-pc-windows-msvc.zip`
  - macOS: `ch32rv-<ver>-aarch64-apple-darwin.tar.gz`(Apple Silicon)/ `-x86_64-apple-darwin.tar.gz`(Intel)
  - 各 `.sha256` で検証。展開すると `ch32rv-<ver>-<target>/` に `ch32rv`(または `ch32rv.exe`)+ LICENSE/README/CHANGELOG。
- **ネイティブビルド**(その OS の Rust): `cargo build --release --bin ch32rv` → `target/release/ch32rv[.exe]`。

以降 `ch32rv` はこのバイナリを指す(Windows は `ch32rv.exe`)。まず `ch32rv --version` が動くこと。

## 2. probe の接続と確認

- **probe セレクタは `serial:<SN>` 形式**(素の SN は不可)。`ch32rv probe list` で SN を確認。
- **Windows**:
  - probe を Windows に挿す(WSL で usbipd に attach している場合は `usbipd detach --busid <B>` で Windows に戻す。`--force` は使わない)。
  - ドライバは WCH 純正でも WinUSB でも可(ch32rv が自動フォールバック、Zadig 不要)。状況は **`ch32rv doctor`** で確認。
- **macOS**: probe を挿すだけ(WCH-Link は標準 USB デバイス、専用ドライバ不要)。開けない場合は `ch32rv doctor` の指示に従う。

## 3. テスト用バイナリ

- **転送用(ツールチェーン不要)**: [`tests/fixtures/pattern-4k.bin`](../tests/fixtures/pattern-4k.bin)(4KB の決定的パターン)。flash 経路の検証に使う。実行可能ファームではない。
- **実行確認用(任意)**: 各 family の blink を arduino-cli で作る(LED/走行の確認用)。
  ```sh
  arduino-cli compile -b ch32-riscv-ug:ch32v:<CH32V003|CH32V203|CH32V307|CH32L103|CH32X035|...> \
    --output-dir out <sketch>
  # out/*.bin か *.elf を ch32rv flash に渡す
  ```
  ピン非依存の blink(counter++/delay、GPIO 無し)ならどの family でもコンパイルが通る。

## 4. テスト手順

`--chip` は省略可(自動検出)。複数 probe があるときは各コマンドに `--probe serial:<SN>` を付ける。

### Tier 1: read-only(トランスポート疎通 = 最重要スモーク)
新 OS ではまずここが通れば「ch32rv が probe と喋れる」が確定する。**書き込まない**ので完全安全。
```sh
ch32rv probe list
ch32rv probe info --probe serial:<SN>
ch32rv target info --probe serial:<SN>          # SKU / family / 容量 / UID
ch32rv read --range 0x08000000+256 --format hex # 先頭 256B を dump
ch32rv capabilities --probe serial:<SN>
ch32rv doctor
```

### Tier 2: flash 往復(非破壊 = backup→書込→verify→restore)
既存ファームを退避してから書き、検証後に戻す。
```sh
# 1) 退避(全 code 領域)
ch32rv read --region code -o backup.bin --probe serial:<SN>
# 2) テストパターンを書いて verify(既定 offset=flash 先頭、erase auto=chip)
ch32rv flash tests/fixtures/pattern-4k.bin --verify --probe serial:<SN>
# 3) 独立 readback で再確認(任意)
ch32rv verify tests/fixtures/pattern-4k.bin --probe serial:<SN>
# 4) 退避を書き戻して復旧
ch32rv flash backup.bin --verify --probe serial:<SN>
ch32rv reset --probe serial:<SN>
```
`--verify` が通れば erase/program/read 経路 OK。**Windows の WCH 純正ドライバ経路は 64B ioctl 単位で遅い**(読み ~2.3KiB/s)ので、大容量部品の全 backup は時間がかかる点に留意。

### Tier 3: 実行確認(任意)
実 blink を書いて走行を見る。
```sh
ch32rv flash out/<blink>.bin --verify --probe serial:<SN>     # ELF/HEX も可
ch32rv reset --probe serial:<SN>
# LED 点滅 or `ch32rv monitor --source dmdata` で runtime 出力を確認
```

### 補助
```sh
ch32rv gdb --probe serial:<SN>            # 別端末で gdb 接続(HW/flash BP)
ch32rv monitor --source dmdata --probe serial:<SN>
ch32rv --capture cap.ndjson probe info --probe serial:<SN>   # 問題時に添付する transaction 記録
```

## 5. 記録テンプレ

probe / target ごとに:

| 項目 | 結果 |
|---|---|
| OS / arch | |
| ch32rv バイナリ(release / build)+ version | |
| ドライバ(Windows: WCH純正 / WinUSB) | |
| probe(SN / model / fw) | |
| target(SKU / family) | |
| Tier1 read-only | OK / NG(詳細) |
| Tier2 flash 往復(verify) | OK / NG |
| Tier3 走行 | OK / NG / 未実施 |
| doctor | |
| 備考(所要時間・エラー・`--capture` 添付) | |

## 6. 既知の注意

- **Windows**: WCH 純正ドライバ経路は動くが遅い(64B ioctl)。WinUSB があればそちらが速い。`ch32rv doctor` でどちらか判別。
- **macOS**: 未検証(experimental)。このテストで verified 昇格を狙う。権限で開けないときは `doctor` の指示。
- 問題を報告するときは **`--capture <file>`** を付けて NDJSON transaction を添付すると、Linux 側で replay 解析しやすい。
