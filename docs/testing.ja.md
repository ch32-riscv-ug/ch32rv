# 実機テスト手順(全 OS)

ch32rv を実機でテストする手順。**まず Linux で通し、次に Windows / macOS で追試**する(Linux x64 と Windows x64 は verified、macOS / arm は experimental)。転送用バイナリは `tests/fixtures/` に用意済みで、Arduino ツールチェーンが無くても flash まで試せる。

## 1. ch32rv バイナリの入手

どちらか:

- **リリース配布物**(ツールチェーン不要): [Releases](https://github.com/ch32-riscv-ug/ch32rv/releases) から対象アーカイブを取得。
  - Linux: `ch32rv-<ver>-x86_64-unknown-linux-gnu.tar.gz`(arm64 版もあり)
  - Windows: `ch32rv-<ver>-x86_64-pc-windows-msvc.zip`
  - macOS: `ch32rv-<ver>-aarch64-apple-darwin.tar.gz`(Apple Silicon)/ `-x86_64-apple-darwin.tar.gz`
  - `.sha256` で検証。展開すると `ch32rv-<ver>-<target>/` に本体 + LICENSE/README/CHANGELOG。
- **ネイティブビルド**: `cargo build --release --bin ch32rv` → `target/release/ch32rv[.exe]`。

まず `ch32rv --version` が動くこと。

## 2. probe の接続と確認

- **probe セレクタは `serial:<SN>` 形式**(素の SN は不可)。`ch32rv probe list` で SN を確認。複数あれば各コマンドに `--probe serial:<SN>`。
- **Linux**: udev ルールを入れる(非 root アクセス)。配布 tar 内の `60-ch32rv.rules` を `sudo cp`、またはバイナリから出力:
  ```sh
  ch32rv doctor --emit-udev | sudo tee /etc/udev/rules.d/60-ch32rv.rules
  sudo udevadm control --reload-rules && sudo udevadm trigger
  ```
- **Windows**: probe を Windows に挿す(WSL で usbipd に attach 中なら `usbipd detach --busid <B>` で Windows へ戻す。`--force` は使わない)。ドライバは WCH 純正でも WinUSB でも可(ch32rv が自動フォールバック、Zadig 不要)。状況は `ch32rv doctor`。
- **macOS**: 挿すだけ(専用ドライバ不要)。開けなければ `ch32rv doctor` の指示に従う。

いずれも `ch32rv doctor` が USB 列挙・権限・firmware・probe mode を診断する。

## 3. テスト用バイナリ(`tests/fixtures/`、用意済み)

| ファイル | 用途 |
|---|---|
| `pattern-4k.bin` | 4KB の決定的パターン(`byte[i]=i&0xFF`)。flash 経路(erase/program/verify)の検証。実行可能ではない |
| `runtest-<family>.bin` | family 別の走行自己テスト(GPIO 無しの counter loop)。**flash + `--confirm-run pc`** で走行確認。**Linux 実機検証済み**: `ch32v003` / `ch32v103` / `ch32v203` / `ch32v307` / `ch32l103` |

- 他 family(X035 等)は基板接続時に `arduino-cli compile -b ch32-riscv-ug:ch32v:<board> tests/fixtures/runtest` でビルド・追加できる(sketch は [`tests/fixtures/runtest/`](../tests/fixtures/runtest/)、再生成は [`tests/fixtures/README.md`](../tests/fixtures/README.md))。

## 4. テスト手順(3 tier)

`--chip` は省略可(自動検出)。

### Tier 1: read-only(トランスポート疎通=最重要スモーク、書かない)
```sh
ch32rv probe list
ch32rv probe info    --probe serial:<SN>
ch32rv target info   --probe serial:<SN>            # SKU / family / 容量 / UID
ch32rv read --range 0x08000000+256 --format hex     # 先頭 256B dump
ch32rv capabilities  --probe serial:<SN>
ch32rv doctor
```

### Tier 2: flash 往復(非破壊 = backup→書込→verify→restore)
```sh
ch32rv read --region code -o backup.bin --probe serial:<SN>     # 退避(全 code 領域)
ch32rv flash tests/fixtures/pattern-4k.bin --probe serial:<SN>  # 既定 verify=readback で検証込み
ch32rv verify tests/fixtures/pattern-4k.bin --probe serial:<SN> # 独立 readback で再確認(任意)
ch32rv flash backup.bin --probe serial:<SN>                     # 書き戻して復旧
ch32rv reset --probe serial:<SN>
```
`flash` は既定で `--verify readback`(書込後に読み戻し照合)。Windows の WCH 純正ドライバ経路は 64B ioctl 単位で遅いので、大容量部品の全 backup は時間がかかる。

### Tier 3: 走行確認(fixture で toolchain 不要)
```sh
ch32rv flash tests/fixtures/runtest-<family>.bin --confirm-run pc --probe serial:<SN>
```
`--confirm-run pc` = reset 後に PC をサンプルし flash 内で実行中かを確認(失敗は exit 50)。**実 LED を見たい/自作ファームを試すなら** ELF/HEX/bin をそのまま `flash` に渡す。

### Tier 3b: semihosting 走行(`run` HIL、exit コード伝搬)
```sh
ch32rv run tests/fixtures/semihosting.bin --probe serial:<SN> --exit-on semihosting
# stdout: hello from semihosting  /  プロセス終了コード: 42
```
`run` は 書込→reset 実行→runtime 出力→終了 を 1 コマンドで行う。`--exit-on semihosting` は
target の `SYS_WRITE0` 出力を中継し、`SYS_EXIT`/`SYS_EXIT_EXTENDED` の値をプロセス終了コードに
伝搬する(fixture は 42)。`--exit-on timeout=<s>` は s 秒だけ dmdata 出力を流して exit 0。
`--no-flash` で書込を省略。**CH32V307 実機検証済み**(family 非依存の base-ISA コード)。

### 補助
```sh
ch32rv gdb --probe serial:<SN>                               # 別端末で gdb 接続(HW/flash BP)
ch32rv monitor --source dmdata --probe serial:<SN>           # runtime 出力
ch32rv --capture cap.ndjson probe info --probe serial:<SN>   # 問題時に添付する transaction 記録
```

## 5. 記録テンプレ

| 項目 | 結果 |
|---|---|
| OS / arch | |
| ch32rv(release / build)+ version | |
| ドライバ(Windows: WCH純正 / WinUSB) | |
| probe(SN / model / fw) | |
| target(SKU / family) | |
| Tier1 read-only | OK / NG |
| Tier2 flash 往復(verify) | OK / NG |
| Tier3 走行(confirm-run pc) | OK / NG / 未実施 |
| Tier3b run(semihosting exit) | OK / NG / 未実施 |
| 備考(所要時間・エラー・`--capture` 添付) | |

## 6. 既知の注意

- **Windows**: WCH 純正ドライバ経路は動くが遅い(64B ioctl)。WinUSB があればそちら。`ch32rv doctor` で判別。
- **macOS**: 未検証(experimental)。このテストで verified 昇格を狙う。
- 問題報告時は **`--capture <file>`** の NDJSON を添付すると Linux 側で replay 解析しやすい。
