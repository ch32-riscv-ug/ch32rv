# リリース計画(v0.1.0)

- 状態: **策定中(2026-09-02)**。安定したら英語 main + `.ja` twin に整える。
- 方針: 「**WCH-LinkE 経路で、6台ベンチで実機検証済みのもの**」を v0.1.0 とする。crates.io に library crate を配布、全対象 OS のバイナリを配布、Arduino 対応込み。重い/未検証/非LinkE経路/電源系は v0.2 以降。
- **0.x はβ位置づけ**: 依存プロジェクト(ArduinoCore-CH32 等)に先行利用してもらい、要望・不足機能を取り込んでから **1.0 で正式リリース**。CHANGELOG は初回だけ差分でなく「何が入っているか」のスナップショットにする。

## 1. 配布する crate(crates.io、依存順に publish)

version は workspace 一括 `0.1.0`、license MIT。**publish 順は依存順**(先に出したものが index に載ってから次):

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

1. **(初回一度きり・ユーザー、CLI)** crates.io で API トークンを発行 → `cargo login <token>` → **`scripts/first-publish.sh`** を実行して 8 crate を依存順に publish・crate 名を確保する(既に存在する crate は自動 skip、最後に手順 2 の登録先を表示)。

2. **(初回一度きり・ユーザー、Web UI)** 各 crate の Settings → Trusted Publishing で GitHub を登録:
   owner=`ch32-riscv-ug` / repo=`ch32rv` / workflow=`release.yml`(environment は任意)。8 crate 分登録する。

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
- **注意**: 手元実機は Linux(WSL2)のみ。**Linux x64 = verified、macOS/Windows/arm = experimental** と Release ノートに明記(Windows は WinUSB / driver binding、macOS は権限を実機確認後に verified 昇格)。ArduinoCore-CH32 の依頼 B-2(Windows 実機検証)は v0.1 後に。

## 3. v0.1.0 に入れる機能(全て実機検証済み)

| 系統 | コマンド |
|---|---|
| probe | list / info / firmware info・check |
| target | info(SKU/family/配線/容量)/ option get・set・reset・write-raw / protect |
| flash | flash(erase auto/sector/chip/none・restore-unwritten・preverify・verify・reset・confirm-run・sdi・monitor・repeat)/ verify / read / write / erase(all/range/region)/ reset / recover(power-off・nrst) |
| debug | dbg halt/resume/step/regs/reg/dmi / gdb server(HW+flash BP) |
| monitor | uart / sdi / dmdata |
| DB/診断 | db list・info / capabilities(live+static)/ doctor / version / complete |
| **arduino** | **discovery / monitor**(Pluggable、upload は flash) |

## 4. v0.1.0 に「小さいので入れる」もの

- ✅ `recover unprotect`(工場 option 書込=RDP off。protected は mass erase で復旧。実機検証済)
- ✅ `probe mode get`(現在 mode を VID:PID+firmware から表示。実機検証済)
- 検討中: `option set` の別名(`rdp=`/`nrst=` 等)— DB の single-bit RM 名は済。別名 map は family 固有なので慎重に(重ければ v0.2)。`probe mode set` は再列挙対応が要るので v0.2

## 5. v0.2 以降(重い/未検証/非LinkE/電源系)

- `run`(HIL、semihosting exit-code)、`monitor rtt`、`recover unbrick`
- `probe firmware update`(IAP 再書込)、`probe mode set`
- `isp`(factory ISP 4348:55e0)、`boot`(UIAPduino/DFU/UF2/HID、後日実機)、`dap`(DAP server)
- `probe power`(3v3/5v/cycle)← **電源系はユーザー指示で保留**
- gap 7 series(V205/V407/V467/X305/X315/M030/M103)device 対応 ← データ側未発売でブロック
- option layout(register CSV)、multi-bit option、V4F FPU レジスタ、vFlash(load)
- Windows/macOS の実機 verified 昇格(依頼 B-2)、arduino discovery の USB hotplug 追随

## 6. リリース前チェック

初回ブートストラップ(一度きり):

- [ ] `cargo login <token>` → `scripts/first-publish.sh` で 8 crate を名前確保(§1 手順 1)
- [ ] 各 crate に Trusted Publisher 登録(owner/repo/`release.yml`、§1 手順 2)
- [ ] `scripts/release.sh` の bump 挙動が自プロジェクト慣習に合うか確認(実装済み。必要なら調整)

毎回:

- [ ] `cargo fmt --check` / `cargo clippy --all-targets --all-features`(warning 0)/ `cargo test` / `cargo deny check`
- [ ] `cargo xtask db-check`(生成物が pinned data と一致)
- [ ] 6台ベンチで代表フロー(flash→verify→run、gdb、monitor、target info、capabilities)を再確認
- [ ] CHANGELOG の `Unreleased` を新 version に切る(= release.sh がやる)
- [ ] README(repo)に crates.io バッジ / インストール手順 / verified OS 明記
- [ ] Actions「Release」を UI 起動 → crates.io publish と全 OS バイナリ添付を確認

## 7. 初回リリース(0.1.0)の実行順

初回だけ crates.io の制約(新規 crate の初回はトークン必須・Trusted Publisher は crate 存在後にしか登録できない)で手順が特殊。2 回目以降は「Actions を起動するだけ」。

1. **本ブランチの未コミット分をコミット & push**(修正済みの `release.yml` / `scripts/` / CHANGELOG が `main` に載っていること。ワークフローは `main` の release.yml を使う)。
2. **crate 名を確保**: `cargo login <token>` → `scripts/first-publish.sh`。8 crate を 0.1.0 で依存順に publish。
3. **Trusted Publisher 登録**: 各 crate の crates.io Settings で owner `ch32-riscv-ug` / repo `ch32rv` / `release.yml` を登録(§1 手順 2)。
4. **バイナリ + GitHub Release**: Actions「Release」を **`version=0.1.0` / `publish_crates=false`** で起動。CHANGELOG を 0.1.0 に切って commit・tag `v0.1.0`・Release 作成し、全 OS バイナリを添付する(crates は手順 2 で済んでいるので skip)。
5. 以降(0.1.1〜)は **`level=patch|minor|major` / `publish_crates=true`** で起動 = 完全トークンレス(bump → publish → binaries)。

> メモ: 手順 4 で `version` を明示するのは、初回だけ「bump せず 0.1.0 のまま」出したいため。通常リリースは `level` を選ぶ(`version` 空欄)。
