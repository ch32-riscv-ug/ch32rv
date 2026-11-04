# リリース計画・手順

- 方針: ベンチで実機検証したものを、crates.io の crate、全対象 OS のバイナリ、Arduino 対応込みで配布する。
- **0.x はβ位置づけ**: 依存プロジェクト(ArduinoCore-CH32RV 等)に先に使ってもらい、要望・不足機能を取り込んでから **1.0 で正式リリース**。CHANGELOG は版ごとの差分。

## 1. 配布する crate(crates.io、依存順に publish)

version は workspace 一括、リリースごとに`release.sh`がbump、license MIT。**publish順は依存順**(先に出したものがindexに載ってから次。`ch32rv-usb-wch-win`は`ch32rv-usb`のcfg(windows)依存なのでusbより前):

1. `ch32rv-contract`(exit code / JSON envelope / policy 語彙)
2. `ch32rv-usb-wch-win`(Windows の WCH 純正ドライバ経路。`ch32rv-usb` より先)
3. `ch32rv-usb`(USB 列挙 / selector / lock / capture、backend 型非漏洩)
4. `ch32rv-dmi`(RISC-V Debug Module + 直接 FLASH controller)★再利用の目玉
5. `ch32rv-target`(生成 device DB。`generated/*.csv` を include_str! で同梱)
6. `ch32rv-isp`(WCH の factory ISP bootloader client。USB と UART)
7. `ch32rv-wchlink`(WCH-Link protocol)★目玉
8. `ch32rv-flash`(erase/program/verify orchestration)
9. `ch32rv-oep`(OEP v1 の host。`ch32rv-flash` の後: dev-dependency の `ch32rv-flash` が index に要る)
10. `ch32rv-boot`(rv003usb/UIAPduino HID bootloader client)
11. `ch32rv-debug`(run control / gdb server)
12. `ch32rv`(CLI バイナリ。`cargo install ch32rv` 可)

各 library に keywords / categories / README を付ける。

**publish しない**(空スタブ、`publish = false`): `ch32rv-monitor`。monitor の実体は cli 側にある。

### publish のやり方(3段階)

Rust/crates.io は、あなたの他プロジェクトの分類にこう対応する:

- **「公開リポジトリから許可 GitHub を登録するだけ」= Trusted Publishing(OIDC)**。GitHub 側にトークンを埋めず、Actions が実行時に crates.io から 30 分の短命トークンを OIDC で受け取る(`rust-lang/crates-io-auth-action`)。設定は crates.io 側で crate ごとに「この owner/repo の release.yml からの publish を許可」を登録するだけ。
- **「認証トークンが必要なので CLI から明示リリース」= 初回だけ必要**。**新規 crate の初回 publish はトークンが要る**(crates.io に pending/事前予約が無いため、Trusted Publisher を後付けするにも一度 crate を存在させる必要がある)。JS でブラウザ+パスキーに当たるのがこれ。

手順:

1. **(新規 crate ごとに一度きり・ユーザー、CLI)** crates.ioでAPIトークンを発行 → `cargo login <token>` → **`scripts/first-publish.sh`**（依存順にpublish、既存crateは自動skip、最後に手順2の登録先を表示）。新規 crate の main が、公開済みの依存 crate に無い API を使っていると package verify が通らない。その crate は、スクリプトが古い commit を一時 worktree へ展開して名前を確保する(`ch32rv-boot` は `fad0881`、`ch32rv-oep` は依存が `thiserror` だけの `ac2b647`)。main や作業 tree は変更しない。

2. **(初回一度きり・ユーザー、Web UI)** 各 crate の Settings → Trusted Publishing で GitHub を登録:
   owner=`ch32-riscv-ug` / repo=`ch32rv` / workflow=`release.yml`（environmentは任意）。

3. **(以降・毎回)** Actions の「Release」ワークフローを **画面から起動**(workflow_dispatch)。中で version bump → 検証 → commit/tag → OIDC で crates.io publish、までトークン埋め込み無しで走る。詳細は §2。

## 2. リリースワークフロー(`.github/workflows/release.yml`)

あなたの「画面から明示起動 → 内部で version を bump → build → release」に合わせた **単一の workflow_dispatch ワークフロー**を用意済み。タグ起点ではなく UI 起点。`permissions: id-token: write`(OIDC)+ `contents: write`(bump commit / tag / Release 作成)。ジョブ構成:

| job | 内容 |
|---|---|
| `prepare` | **埋め込み device DB の drift 検査(`ch32-device-data` を checkout → `cargo xtask db-check`)** → **version bump(`./scripts/release.sh <level>` フック)** → fmt/clippy/test/deny → commit + tag + push → GitHub Release 作成。bump 後の version は `cargo metadata` から読む(スクリプト出力形式に非依存)。db-check は隣接 data repo が要るので CI では回さない(生成物は commit 済みで hermetic、ローカルのドリフト検査に留める)。 |
| `crates-io` | tag を checkout → `rust-lang/crates-io-auth-action`(OIDC 短命トークン)→ 依存順に `cargo publish`。`inputs.publish_crates=false` で無効化可。 |
| `binaries` | matrix(下表)で `cargo build --release --locked` → tar.gz(Unix)/ zip(Windows)+ `.sha256` → 同じ Release に `gh release upload`。 |

バイナリ matrix(cross 不使用。macOS の x86_64 だけは Apple Silicon の runner で `--target` を替えて作り、実行の確認はしない):

| runner | target |
|---|---|
| ubuntu-latest | x86_64-unknown-linux-gnu |
| ubuntu-24.04-arm | aarch64-unknown-linux-gnu |
| macos-15 | aarch64-apple-darwin |
| macos-15 | x86_64-apple-darwin |
| windows-latest | x86_64-pc-windows-msvc |

要対応(ユーザー):

- **スクリプトは 2 本**(役割が別):
  - `scripts/release.sh <patch|minor|major|X.Y.Z>` = **毎回**の bump フック。workspace `version` と Cargo.toml 内部 pin(全メンバーは `version.workspace=true` 継承なのでルートのみ)+ Cargo.lock を bump、CHANGELOG の `Unreleased` を新 version に切る。
  - `scripts/first-publish.sh` = **初回一度きり**の crates.io ブートストラップ(名前のとおり初回専用)。§1 手順 1 の crate 確保を実行。以後は使わない。
- **初回の crate 確保 + Trusted Publisher 登録**(§1 の手順 1・2)を済ませないと `crates-io` job は通らない。
- `ubuntu-24.04-arm`(GitHub の arm64 ランナー)が使えない環境なら、その行を外すか cross に差し替える。
- main が **branch protection** だと Actions からの bump commit push がブロックされうる。bot に例外を許すか、専用リリースブランチ運用にする。
- **cargo-dist は使わない**(タグ起点で UI-bump フローに噛み合わないため手書きにした)。
- **`prepare` の Verify が `cargo deny check` で落ちたら yank を疑う**: 依存 crate が crates.io で yank されると `error[yanked]: detected yanked crate (try cargo update -p <crate>)` で止まる。直前までローカルで通っていても CI は最新 index を見るので起こる。対処は提案どおり `cargo update -p <crate>`(patch 版へ)→ ゲート再実行 → lock を commit → workflow 再起動。CI 側は `Commit, tag, push` に達していないので remote に半端な状態は残らない。
- **注意**: 開発機は Linux(WSL2)+ usbipd 越しの Windows ネイティブ。**Linux x64 = verified**。**Windows x64 = verified**(WCH 純正ドライバ経路 `ch32rv-usb-wch-win` で全5 probe の flash 往復まで実機確認。Zadig 不要)。**macOS / arm = experimental**(未実機)。Release ノートにこの verified 状況を明記する。

### 2.1 crates.io への公開が途中で止まったとき

binary と GitHub Release は揃っても、crates.io への公開が途中で止まることがある。`cargo publish` は直前に出した crate が index に載るのは待つが、それより前に出した crate は待たない。index の配信が遅れると、後の crate の package が `failed to select a version for the requirement ch32rv-dmi = "^<版>"` のように落ちる。既に出た crate は同じ版で出し直せない。

- 公開の loop は、同じ版が crates.io にある crate を飛ばし、失敗した crate は 0 / 60 / 120 / 240 / 480 秒あけて最大 5 回やり直す(index の遅れはこれで吸収する)。
- 途中で止まったら、Actions の「Release」を **`republish_tag=v<版>`**(`publish_crates=true`)で起動する。版を上げず、tag も binary も作らず、その tag の source で残りの crate だけを出す(Trusted Publishing は `release.yml` に結び付いているので、同じ workflow の中でやる)。

## 3. リリース前チェック

ブートストラップ(新規 crate ごと一度きり):

- [ ] 新規 crate を `scripts/first-publish.sh` で初回トークン publish + TP 登録(`cargo login` のトークンが切れていれば更新する)

毎回:

- [ ] `cargo fmt --all --check`(出力 0 行)/ `cargo clippy --all-targets -- -D warnings` / `cargo test` / `cargo deny check`
- [ ] `cargo xtask db-check`(埋め込み device DB が `ch32-device-data` と一致。ずれていれば `db-gen` して commit。**release workflow でも版 bump 前に検査される**)
- [ ] ベンチの各 family で代表フロー(flash→verify、gdb、monitor、target info、capabilities)を再確認
- [ ] **Windows(WCH 純正ドライバ経路)で probe list / target info / flash 往復を再確認**: probe を `usbipd detach` で Windows へ戻し、Windows ネイティブビルドで確認する。**detach 対象は `usbipd list` の DEVICE 欄が `WCH-Link SERIAL` の個体のみ**(docs/testing.ja.md)
- [ ] CHANGELOG の `Unreleased` を新 version に切る(= release.sh がやる)
- [ ] README(repo)に crates.io バッジ / インストール手順 / verified OS 明記
- [ ] Actions「Release」をUI起動 → crates.io publish（全 crate）と全OSバイナリ添付を確認

## 4. リリース実行順

1. **未コミット分をコミット & push**(ワークフローは `main` の release.yml を使う)。
2. **新規 crate があれば bootstrap**: `cargo login <token>` → `scripts/first-publish.sh`（既存 crate は skip）。
3. **その crate の Trusted Publisher 登録**（§1手順2）。
4. **リリース起動**: Actions「Release」を **`level=minor` / `publish_crates=true`** で起動 → version bump → 全 crate をトークンレスpublish → 全OSバイナリ添付。
5. これ以降は新規 crate を足さない限り **手順 4 だけ**(bootstrap 不要)。

> メモ: 新規 crate を追加した回だけ手順 2・3 が要る(crates.io は新規 crate の初回 publish にトークンが要り、TP は後付けだから)。既存 crate の版上げは常にトークンレス。
