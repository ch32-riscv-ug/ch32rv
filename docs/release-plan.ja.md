# リリース計画・手順

- 状態: **運用中(2026-09-02)**。初回 **0.2.0 出荷済み**。安定したら英語 main + `.ja` twin に整える。
- 方針: 「WCH-LinkE 経路で 6台ベンチ実機検証済みのもの」を軸に、crates.io へ library crate、全対象 OS のバイナリ、Arduino 対応込みで配布。重い/未検証/非LinkE/電源系は後続。
- **経緯**: 0.2.0 が初回リリース(0.1.x はテスト専用)。以降 **A-2 lock / A-3 capture / Windows の WCH 純正ドライバ対応(`ch32rv-usb-wch-win`、依頼 B-2、Zadig 不要)** を実装済み → 次リリースに含める。
- **0.x はβ位置づけ**: 依存プロジェクト(ArduinoCore-CH32 等)に先行利用してもらい、要望・不足機能を取り込んでから **1.0 で正式リリース**。CHANGELOG は初回だけスナップショット、以降は差分。

## 1. 配布する crate(crates.io、依存順に publish)

version は workspace 一括、リリースごとに`release.sh`がbump、license MIT。**10 crate、publish順は依存順**(先に出したものがindexに載ってから次。`ch32rv-usb-wch-win`は`ch32rv-usb`のcfg(windows)依存なのでusbより前):

1. `ch32rv-contract`(exit code / JSON envelope / policy 語彙)
2. `ch32rv-usb-wch-win`(Windows の WCH 純正ドライバ経路。0.2 で追加、`ch32rv-usb` より先)
3. `ch32rv-usb`(USB 列挙 / selector / lock / capture、backend 型非漏洩)
4. `ch32rv-dmi`(RISC-V Debug Module + 直接 FLASH controller)★再利用の目玉
5. `ch32rv-target`(生成 device DB。`generated/*.csv` を include_str! で同梱)
6. `ch32rv-wchlink`(WCH-Link protocol)★目玉
7. `ch32rv-flash`(erase/program/verify orchestration)
8. `ch32rv-boot`(rv003usb/UIAPduino HID bootloader client)
9. `ch32rv-debug`(run control / gdb server)
10. `ch32rv`(CLI バイナリ。`cargo install ch32rv` 可)

各 library に keywords / categories / README を付与済み。`ch32rv-contract` は `cargo publish --dry-run` 成功済み(他は contract が index に載れば通る)。

**publish しない**(空スタブ、`publish = false`): `ch32rv-monitor` / `ch32rv-isp`。monitor の実体は現状cli側。

### publish のやり方(3段階)

Rust/crates.io は、あなたの他プロジェクトの分類にこう対応する:

- **「公開リポジトリから許可 GitHub を登録するだけ」= Trusted Publishing(OIDC)**。GitHub 側にトークンを埋めず、Actions が実行時に crates.io から 30 分の短命トークンを OIDC で受け取る(`rust-lang/crates-io-auth-action`)。設定は crates.io 側で crate ごとに「この owner/repo の release.yml からの publish を許可」を登録するだけ。
- **「認証トークンが必要なので CLI から明示リリース」= 初回だけ必要**。**新規 crate の初回 publish はトークンが要る**(crates.io に pending/事前予約が無いため、Trusted Publisher を後付けするにも一度 crate を存在させる必要がある)。JS でブラウザ+パスキーに当たるのがこれ。

手順:

1. **(新規 crate ごとに一度きり・ユーザー、CLI)** crates.ioでAPIトークンを発行 → `cargo login <token>` → **`scripts/first-publish.sh`**（依存順にpublish、既存crateは自動skip、最後に手順2の登録先を表示）。**現況: 既存9 crateは登録済み。0.9.0で公開対象へ昇格する`ch32rv-boot`だけ初回publishとTrusted Publisher登録が必要。** 現在のmainはまだ0.8.0で、`ch32rv-boot`のcapture/replay実装が未公開の`ch32rv-usb` APIを使うため、そのままでは0.8.0のpackage verifyが成立しない。スクリプトはbootに限り、capture依存を足す前の0.8.0互換commit (`fad0881`)を一時worktreeへ展開して名前を確保する。mainや作業treeは変更しない。

2. **(初回一度きり・ユーザー、Web UI)** 各 crate の Settings → Trusted Publishing で GitHub を登録:
   owner=`ch32-riscv-ug` / repo=`ch32rv` / workflow=`release.yml`（environmentは任意）。既存9 crateは登録済みで、0.9.0前に`ch32rv-boot`を追加登録する。

3. **(以降・毎回)** Actions の「Release」ワークフローを **画面から起動**(workflow_dispatch)。中で version bump → 検証 → commit/tag → OIDC で crates.io publish、までトークン埋め込み無しで走る。詳細は §2。

## 2. リリースワークフロー(`.github/workflows/release.yml`)

あなたの「画面から明示起動 → 内部で version を bump → build → release」に合わせた **単一の workflow_dispatch ワークフロー**を用意済み。タグ起点ではなく UI 起点。`permissions: id-token: write`(OIDC)+ `contents: write`(bump commit / tag / Release 作成)。ジョブ構成:

| job | 内容 |
|---|---|
| `prepare` | **埋め込み device DB の drift 検査(`ch32-device-data` を checkout → `cargo xtask db-check`)** → **version bump(`./scripts/release.sh <level>` フック)** → fmt/clippy/test/deny → commit + tag + push → GitHub Release 作成。bump 後の version は `cargo metadata` から読む(スクリプト出力形式に非依存)。db-check は隣接 data repo が要るので CI では回さない(生成物は commit 済みで hermetic、ローカルのドリフト検査に留める)。 |
| `crates-io` | tag を checkout → `rust-lang/crates-io-auth-action`(OIDC 短命トークン)→ 依存順に `cargo publish`。`inputs.publish_crates=false` で無効化可。 |
| `binaries` | matrix(下表)で `cargo build --release --locked` → tar.gz(Unix)/ zip(Windows)+ `.sha256` → 同じ Release に `gh release upload`。 |

バイナリ matrix(すべて **ネイティブ**ビルド。cross 不使用):

| runner | target |
|---|---|
| ubuntu-latest | x86_64-unknown-linux-gnu |
| ubuntu-24.04-arm | aarch64-unknown-linux-gnu |
| macos-13 | x86_64-apple-darwin |
| macos-14 | aarch64-apple-darwin |
| windows-latest | x86_64-pc-windows-msvc |

要対応(ユーザー):

- **スクリプトは 2 本**(役割が別):
  - `scripts/release.sh <patch|minor|major|X.Y.Z>` = **毎回**の bump フック。workspace `version` と Cargo.toml 内部 pin(全メンバーは `version.workspace=true` 継承なのでルートのみ)+ Cargo.lock を bump、CHANGELOG の `Unreleased` を新 version に切る。実装済み。他プロジェクトの慣習に合わせて調整可。
  - `scripts/first-publish.sh` = **初回一度きり**の crates.io ブートストラップ(名前のとおり初回専用)。§1 手順 1 の crate 確保を実行。以後は使わない。
- **初回の crate 確保 + Trusted Publisher 登録**(§1 の手順 1・2)を済ませないと `crates-io` job は通らない。
- `ubuntu-24.04-arm`(GitHub の arm64 ランナー)が使えない環境なら、その行を外すか cross に差し替える。
- main が **branch protection** だと Actions からの bump commit push がブロックされうる。bot に例外を許すか、専用リリースブランチ運用にする。
- **cargo-dist は不採用**(タグ起点で UI-bump フローに噛み合わないため手書きにした)。将来インストーラ(shell/powershell one-liner)や自動更新が欲しくなったら dist へ移行を再検討。
- **`prepare` の Verify が `cargo deny check` で落ちたら yank を疑う**: 依存 crate が crates.io で yank されると `error[yanked]: detected yanked crate (try cargo update -p <crate>)` で止まる(0.8.0 の初回試行で `serialport 4.10.0` がこれ)。直前までローカルで通っていても CI は最新 index を見るので起こる。対処は提案どおり `cargo update -p <crate>`(patch 版へ)→ ゲート再実行 → lock を commit → workflow 再起動。CI 側は `Commit, tag, push` に達していないので remote に半端な状態は残らない。
- **注意**: 開発機は Linux(WSL2)+ usbipd 越しの Windows ネイティブ。**Linux x64 = verified**。**Windows x64 = verified**(2026-09-02、WCH 純正ドライバ経路 `ch32rv-usb-wch-win` で全5 probe の flash 往復まで実機確認。Zadig 不要。依頼 B-2 完了)。**macOS / arm = experimental**(未実機)。Release ノートにこの verified 状況を明記する。

## 3. 出荷済みの機能(0.7.0 時点、全て実機検証済み)

| 系統 | コマンド |
|---|---|
| probe | list(`--watch`)/ info / firmware info・check・update・exit-iap / mode get・set / power 3v3・5v・cycle |
| target | info(SKU/family/配線/容量)/ option get・set・reset・write-raw / protect(option base は DB 由来) |
| flash | flash(erase auto/sector/chip/none・restore-unwritten・preverify・verify・reset・confirm-run・sdi・monitor・repeat)/ verify / read(range・region)/ write / erase(all/range/region)/ reset / recover(power-off・nrst・unprotect・unbrick) |
| debug | dbg halt/resume/step/regs/reg/dmi / gdb server(HW+RAM+flash BP) |
| monitor / run | monitor uart / sdi / dmdata / rtt(uart・dmdata・rtt は双方向)/ run(HIL: flash→reset→出力→semihosting exit code) |
| DB/診断 | db list・info / capabilities(live+static)/ doctor / version / complete |
| arduino | discovery / monitor(Pluggable。dmdata・rtt を双方向に wrap。upload は flash) |
| 横断 | `--json`(契約 2)/ `--capture`・`--replay` / per-probe lock(`--lock-timeout`)/ Windows は WCH 純正ドライバで動作(Zadig 不要) |

## 4. 版ごとの主な追加(詳細は CHANGELOG)

- 0.3.0: Windows 純正ドライバ(`ch32rv-usb-wch-win`)、`--capture`、per-probe lock
- 0.4.0: 一貫性スイープ(契約 1→2)、`recover unbrick`、`monitor rtt`、`run`、`probe power`、`probe mode set`、バルク read 高速化 / 0.4.1: CH549 の stale read 修正
- 0.5.0: `read --region`、`probe list --watch`、`--replay`
- 0.6.0: `--non-interactive` の破壊操作拒否、`probe firmware update` / `exit-iap`
- 0.7.0: FLASH controller profile・option base を生成 DB 由来に、recover が他 option byte を保存、V103 の option verify 修正
- 次版: **V00x(V002/V004/V005/V006/V007/M007)対応** — stub が無い family 向けに FLASH controller 直叩きの書込経路を追加、device DB が CH32V006K8U6 を識別

## 5. 後続(未実装/重い/未検証)

- `isp`(factory ISP 4348:55e0)、`boot`(UIAPduino/DFU/UF2/HID、後日実機)、`dap`(DAP server)
- gap 7 series(V205/V407/V467/X305/X315/M030/M103)device 対応 ← データ側未発売でブロック
- option layout(register CSV)、multi-bit option、`option set` の構造化別名(`nrst=`/`split=` 等)、V4F FPU レジスタ、vFlash(load)
- macOS の実機 verified 昇格、arduino discovery の USB hotplug 追随
- - `monitor --source sdi` の in-process forward 起動不良(enable は成功するのに CDC へ流れない。wlink との usbmon 差分要)
- RTT channel 選択(`--channel`)← **需要待ち**。warning `rtt-channels`(方向あたり 2 本以上)に当たる利用者が出たら
- `monitor` の外部出口(TCP `--listen` / pty)← **需要待ち**。dmdata/rtt を標準シリアルツール(screen/minicom/PlatformIO/serial GUI)へ繋ぐ IF 候補。IDE は pluggable monitor、端末・CI は stdio で足りるため保留。要るなら TCP(全 OS、std のみ、OpenOCD `rtt server` 同型、channel→port)→ pty(Linux/macOS 限定、pty crate 依存、symlink 管理、読み手不在時は target から汲み続けて host で捨てる)の順。unix の代替: `socat pty,raw,echo=0,link=/tmp/ch32rv0 exec:'ch32rv monitor --source rtt'`

## 6. リリース前チェック

ブートストラップ(新規 crate ごと一度きり):

- [x] 0.2.0 で 8 crate を名前確保 + Trusted Publisher 登録済み
- [x] `ch32rv-usb-wch-win`を初回トークンpublish + TP登録（既存9 crateは完了）
- [ ] `ch32rv-boot`を初回トークンpublish + TP登録（保存済みtokenは2026-09-18に403。`cargo login`更新が必要）
- [x] `scripts/release.sh` 動作確認済み

毎回:

- [x] `cargo fmt --check` / `cargo clippy --all-targets --all-features`(warning 0)/ `cargo test` / `cargo deny check` — 2026-09-16 済
- [x] `cargo xtask db-check`(埋め込み device DB が `ch32-device-data` と一致。ずれていれば `db-gen` して commit。**release workflow でも版 bump 前に検査される**) — 2026-09-16 済(`ch32-device-data@e3e723a`、K8U6 収載)
- [ ] `cargo xtask db-check`(生成物が pinned data と一致)
- [x] 7台ベンチ(V003 / V00x / V103 / V203 / V307 / X035 / L103)で代表フロー(flash→verify、gdb、monitor、target info、capabilities)を再確認 — 2026-09-16 済: 全 7 台で dump→flash→dump がバイト一致(V203 3s / V307 12s / L103 4s / X035 3s / V103 15s / V003 2s / V006 243s)、CH549 と V307 で `flash` 連続実行も成功/失敗の交互なし。V006 は Windows(WCH 純正ドライバ)経由でも read/flash/verify を確認
- [x] **Windows(WCH 純正ドライバ経路)で probe list / target info / flash 往復を再確認**(依頼 B-2 の回帰) — 2026-09-16 済: V006 `497F8F06CE2F` を `usbipd detach` で Windows へ戻し、`probe list/info`・`target info`(K8U6)・`read` 4 KiB・`flash` 4 KiB(V00x controller 経路)・`verify` 62 KiB を Windows ネイティブビルドで確認。**detach 対象は `usbipd list` の DEVICE 欄が `WCH-Link SERIAL` の個体のみ**(docs/testing.ja.md)
- [ ] CHANGELOG の `Unreleased` を新 version に切る(= release.sh がやる)
- [ ] README(repo)に crates.io バッジ / インストール手順 / verified OS 明記
- [ ] Actions「Release」をUI起動 → crates.io publish（10 crate）と全OSバイナリ添付を確認

## 7. リリース実行順

### 7.1 初回リリース(0.2.0)= 実施済み(2026-09-02)
初回は crates.io 制約(新規 crate の初回はトークン必須・TP は crate 存在後にしか登録できない)で特殊だった。8 crate をトークンで初回 publish → TP 登録 → Actions を `version=0.2.0` / `publish_crates=false` でバイナリ+Release、という順で完了。**記録として残す**。

### 7.2 次リリース(Windows 対応込み)の実行順
0.9.0では`ch32rv-boot`を公開対象へ昇格するため、この1 crateだけ初回bootstrapが要る。それ以外は通常フロー。

1. **未コミット分をコミット & push**(Windows crate / 自動化修正 / docs が `main` に載ること。ワークフローは `main` の release.yml を使う)。
2. **新規 crate をbootstrap**: `cargo login <token>` → `scripts/first-publish.sh`（既存9 crateはskip、`ch32rv-boot`だけ0.8.0互換sourceでpublish）。
3. **そのcrateのTrusted Publisher登録**（§1手順2、`ch32rv-boot`の1個）。
4. **リリース起動**: Actions「Release」を **`level=minor` / `publish_crates=true`** で起動 → version bump → 10 crateをトークンレスpublish → 全OSバイナリ添付。
5. これ以降は新規 crate を足さない限り **手順 4 だけ**(bootstrap 不要)。

> メモ: 新規 crate を追加した回だけ手順 2・3 が要る(crates.io は新規 crate の初回 publish にトークンが要り、TP は後付けだから)。既存 crate の版上げは常にトークンレス。
