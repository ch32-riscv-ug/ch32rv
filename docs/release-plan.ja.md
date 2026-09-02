# リリース計画・手順

- 状態: **運用中(2026-09-02)**。初回 **0.2.0 出荷済み**。安定したら英語 main + `.ja` twin に整える。
- 方針: 「WCH-LinkE 経路で 6台ベンチ実機検証済みのもの」を軸に、crates.io へ library crate、全対象 OS のバイナリ、Arduino 対応込みで配布。重い/未検証/非LinkE/電源系は後続。
- **経緯**: 0.2.0 が初回リリース(0.1.x はテスト専用)。以降 **A-2 lock / A-3 capture / Windows の WCH 純正ドライバ対応(`ch32rv-usb-wch-win`、依頼 B-2、Zadig 不要)** を実装済み → 次リリースに含める。
- **0.x はβ位置づけ**: 依存プロジェクト(ArduinoCore-CH32 等)に先行利用してもらい、要望・不足機能を取り込んでから **1.0 で正式リリース**。CHANGELOG は初回だけスナップショット、以降は差分。

## 1. 配布する crate(crates.io、依存順に publish)

version は workspace 一括(現在 `0.2.0`、リリースごとに `release.sh` が bump)、license MIT。**9 crate、publish 順は依存順**(先に出したものが index に載ってから次。`ch32rv-usb-wch-win` は `ch32rv-usb` の cfg(windows) 依存なので usb より前):

1. `ch32rv-contract`(exit code / JSON envelope / policy 語彙)
2. `ch32rv-usb-wch-win`(Windows の WCH 純正ドライバ経路。0.2 で追加、`ch32rv-usb` より先)
3. `ch32rv-usb`(USB 列挙 / selector / lock / capture、backend 型非漏洩)
4. `ch32rv-dmi`(RISC-V Debug Module + 直接 FLASH controller)★再利用の目玉
5. `ch32rv-target`(生成 device DB。`generated/*.csv` を include_str! で同梱)
6. `ch32rv-wchlink`(WCH-Link protocol)★目玉
7. `ch32rv-flash`(erase/program/verify orchestration)
8. `ch32rv-debug`(run control / gdb server)
9. `ch32rv`(CLI バイナリ。`cargo install ch32rv` 可)

各 library に keywords / categories / README を付与済み。`ch32rv-contract` は `cargo publish --dry-run` 成功済み(他は contract が index に載れば通る)。

**publish しない**(空スタブ、`publish = false`): `ch32rv-monitor` / `ch32rv-isp` / `ch32rv-boot`。monitor の実体は現状 cli 側。v0.2 で crate 化する際に publish 検討。

### publish のやり方(3段階)

Rust/crates.io は、あなたの他プロジェクトの分類にこう対応する:

- **「公開リポジトリから許可 GitHub を登録するだけ」= Trusted Publishing(OIDC)**。GitHub 側にトークンを埋めず、Actions が実行時に crates.io から 30 分の短命トークンを OIDC で受け取る(`rust-lang/crates-io-auth-action`)。設定は crates.io 側で crate ごとに「この owner/repo の release.yml からの publish を許可」を登録するだけ。
- **「認証トークンが必要なので CLI から明示リリース」= 初回だけ必要**。**新規 crate の初回 publish はトークンが要る**(crates.io に pending/事前予約が無いため、Trusted Publisher を後付けするにも一度 crate を存在させる必要がある)。JS でブラウザ+パスキーに当たるのがこれ。

手順:

1. **(新規 crate ごとに一度きり・ユーザー、CLI)** crates.io で API トークンを発行 → `cargo login <token>` → **`scripts/first-publish.sh`**(依存順に publish、**既に存在する crate は自動 skip**、最後に手順 2 の登録先を表示)。**現況: 0.2.0 で 8 crate は済。0.2 で追加した `ch32rv-usb-wch-win` が未 publish なので、次リリース前にこれを1回だけトークンで初回 publish する**(スクリプトが他8をskipしこれだけ出す)。

2. **(初回一度きり・ユーザー、Web UI)** 各 crate の Settings → Trusted Publishing で GitHub を登録:
   owner=`ch32-riscv-ug` / repo=`ch32rv` / workflow=`release.yml`(environment は任意)。**全 9 crate 分**(0.2.0 の 8 は登録済み → 次は `ch32rv-usb-wch-win` の 1 個を追加登録)。

3. **(以降・毎回)** Actions の「Release」ワークフローを **画面から起動**(workflow_dispatch)。中で version bump → 検証 → commit/tag → OIDC で crates.io publish、までトークン埋め込み無しで走る。詳細は §2。

## 2. リリースワークフロー(`.github/workflows/release.yml`)

あなたの「画面から明示起動 → 内部で version を bump → build → release」に合わせた **単一の workflow_dispatch ワークフロー**を用意済み。タグ起点ではなく UI 起点。`permissions: id-token: write`(OIDC)+ `contents: write`(bump commit / tag / Release 作成)。ジョブ構成:

| job | 内容 |
|---|---|
| `prepare` | **version bump(`./scripts/release.sh <level>` フック)** → fmt/clippy/test/deny → commit + tag + push → GitHub Release 作成。bump 後の version は `cargo metadata` から読む(スクリプト出力形式に非依存)。db-check は隣接 data repo が要るので CI では回さない(生成物は commit 済みで hermetic、ローカルのドリフト検査に留める)。 |
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
- **注意**: 開発機は Linux(WSL2)+ usbipd 越しの Windows ネイティブ。**Linux x64 = verified**。**Windows x64 = verified**(2026-09-02、WCH 純正ドライバ経路 `ch32rv-usb-wch-win` で全5 probe の flash 往復まで実機確認。Zadig 不要。依頼 B-2 完了)。**macOS / arm = experimental**(未実機)。Release ノートにこの verified 状況を明記する。

## 3. 出荷済みの機能(0.2.0、全て実機検証済み)

| 系統 | コマンド |
|---|---|
| probe | list / info / firmware info・check |
| target | info(SKU/family/配線/容量)/ option get・set・reset・write-raw / protect |
| flash | flash(erase auto/sector/chip/none・restore-unwritten・preverify・verify・reset・confirm-run・sdi・monitor・repeat)/ verify / read / write / erase(all/range/region)/ reset / recover(power-off・nrst) |
| debug | dbg halt/resume/step/regs/reg/dmi / gdb server(HW+flash BP) |
| monitor | uart / sdi / dmdata |
| DB/診断 | db list・info / capabilities(live+static)/ doctor / version / complete |
| **arduino** | **discovery / monitor**(Pluggable、upload は flash) |

## 4. 0.2.0 に入れた小機能 / 0.2.0 後に追加(次リリース対象)

0.2.0 の小機能:
- ✅ `recover unprotect`(工場 option 書込=RDP off。protected は mass erase で復旧。実機検証済)
- ✅ `probe mode get`(現在 mode を VID:PID+firmware から表示。実機検証済)

0.2.0 後に実装済み(= **次リリースに含める**、CHANGELOG Unreleased 参照):
- ✅ **A-2 per-probe advisory lock**(`--lock-timeout`、exit 13。実機検証済)
- ✅ **A-3 `--capture`**(USB transaction を NDJSON 記録=replay fixture。実機検証済)
- ✅ **Windows の WCH 純正ドライバ対応**(`ch32rv-usb-wch-win`、Zadig 不要、依頼 B-2。実機検証済)
- 検討中: `option set` の別名(`rdp=`/`nrst=` 等)、`probe mode set`(再列挙対応要)

## 5. 後続(未実装/重い/未検証/電源系)

- `run`(HIL、semihosting exit-code)、`monitor rtt`、`recover unbrick`
- `probe firmware update`(IAP 再書込)、`probe mode set`
- `isp`(factory ISP 4348:55e0)、`boot`(UIAPduino/DFU/UF2/HID、後日実機)、`dap`(DAP server)
- `probe power`(3v3/5v/cycle)← **電源系はユーザー指示で保留**
- gap 7 series(V205/V407/V467/X305/X315/M030/M103)device 対応 ← データ側未発売でブロック
- option layout(register CSV)、multi-bit option、V4F FPU レジスタ、vFlash(load)
- macOS の実機 verified 昇格、arduino discovery の USB hotplug 追随、capture の replay(fixture 再生)。**Windows は WCH 純正ドライバ経路で verified 済み(依頼 B-2 完了)**

## 6. リリース前チェック

ブートストラップ(新規 crate ごと一度きり):

- [x] 0.2.0 で 8 crate を名前確保 + Trusted Publisher 登録済み
- [ ] **`ch32rv-usb-wch-win`(0.2 追加の新規)を初回トークン publish + TP 登録**(§1 手順 1・2。次リリース前に必須。first-publish.sh は既存8を skip しこれだけ出す)
- [x] `scripts/release.sh` 動作確認済み

毎回:

- [ ] `cargo fmt --check` / `cargo clippy --all-targets --all-features`(warning 0)/ `cargo test` / `cargo deny check`
- [ ] `cargo xtask db-check`(生成物が pinned data と一致)
- [ ] 6台ベンチで代表フロー(flash→verify、gdb、monitor、target info、capabilities)を再確認
- [ ] **Windows(WCH 純正ドライバ経路)で probe list / target info / flash 往復を再確認**(依頼 B-2 の回帰)
- [ ] CHANGELOG の `Unreleased` を新 version に切る(= release.sh がやる)
- [ ] README(repo)に crates.io バッジ / インストール手順 / verified OS 明記
- [ ] Actions「Release」を UI 起動 → crates.io publish(9 crate)と全 OS バイナリ添付を確認

## 7. リリース実行順

### 7.1 初回リリース(0.2.0)= 実施済み(2026-09-02)
初回は crates.io 制約(新規 crate の初回はトークン必須・TP は crate 存在後にしか登録できない)で特殊だった。8 crate をトークンで初回 publish → TP 登録 → Actions を `version=0.2.0` / `publish_crates=false` でバイナリ+Release、という順で完了。**記録として残す**。

### 7.2 次リリース(Windows 対応込み)の実行順
0.2.0 後に **`ch32rv-usb-wch-win` を新規追加**したので、その 1 crate だけ初回 bootstrap が要る。それ以外は通常フロー。

1. **未コミット分をコミット & push**(Windows crate / 自動化修正 / docs が `main` に載ること。ワークフローは `main` の release.yml を使う)。
2. **新規 crate を bootstrap**: `cargo login <token>` → `scripts/first-publish.sh`(既存 8 は skip、**`ch32rv-usb-wch-win` だけ現行 version でトークン publish**)。
3. **その crate の Trusted Publisher 登録**(§1 手順 2、`ch32rv-usb-wch-win` の 1 個)。
4. **リリース起動**: Actions「Release」を **`level=minor`(新機能なので)/ `publish_crates=true`** で起動 → version bump → 9 crate をトークンレス publish → 全 OS バイナリ添付。
5. これ以降は新規 crate を足さない限り **手順 4 だけ**(bootstrap 不要)。

> メモ: 新規 crate を追加した回だけ手順 2・3 が要る(crates.io は新規 crate の初回 publish にトークンが要り、TP は後付けだから)。既存 crate の版上げは常にトークンレス。
