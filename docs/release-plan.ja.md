# リリース計画(v0.1.0)

- 状態: **策定中(2026-09-02)**。安定したら英語 main + `.ja` twin に整える。
- 方針: 「**WCH-LinkE 経路で、6台ベンチで実機検証済みのもの**」を v0.1.0 とする。crates.io に library crate を正式配布、全対象 OS のバイナリを配布、Arduino 対応込み。重い/未検証/非LinkE経路/電源系は v0.2 以降。

## 1. 配布する crate(crates.io、依存順に publish)

version は workspace 一括 `0.1.0`、license MIT。**publish 順は依存順**(先に出したものが index に載ってから次):

1. `ch32rv-contract`(exit code / JSON envelope / policy 語彙)
2. `ch32rv-usb`(USB 列挙 / selector / lock / capture、nusb 非漏洩)
3. `ch32rv-dmi`(RISC-V Debug Module + 直接 FLASH controller)★再利用の目玉
4. `ch32rv-target`(生成 device DB。`generated/*.csv` を include_str! で同梱)
5. `ch32rv-wchlink`(WCH-Link protocol)★目玉
6. `ch32rv-flash`(erase/program/verify orchestration)
7. `ch32rv-debug`(run control / gdb server)
8. `ch32rv`(CLI バイナリ。`cargo install ch32rv` 可)

各 library に keywords / categories / README を付与済み。`ch32rv-contract` は `cargo publish --dry-run` 成功済み(他は contract が index に載れば通る)。

**publish しない**(空スタブ、`publish = false`): `ch32rv-monitor` / `ch32rv-isp` / `ch32rv-boot`。monitor の実体は現状 cli 側。v0.2 で crate 化する際に publish 検討。

### publish 実行(ユーザー依頼)

```sh
# 事前に crates.io トークン設定: cargo login <token>
for c in contract usb dmi target wchlink flash debug; do
  cargo publish -p ch32rv-$c   # 前の crate が index 反映されるまで数十秒待つ
done
cargo publish -p ch32rv        # CLI
```

## 2. 全 OS バイナリ配布(cargo-dist / `dist`)

`dist` は未インストール。ユーザー側で:

```sh
cargo install cargo-dist            # or: 各 OS の配布物 tooling
dist init                           # targets に Linux(x64/arm64)・macOS(x64/arm64)・Windows(x64) を選択、
                                    # installers=shell,powershell、ci=github を選ぶ
dist generate                       # .github/workflows/release.yml を生成 → commit
```

- **注意**: 手元実機は Linux(WSL2)のみ。**Linux x64 = verified、macOS/Windows = experimental** とリリースノートに明記する(Windows は WinUSB / driver binding、macOS は権限を実機確認後に verified 昇格)。ArduinoCore-CH32 の依頼 B-2(Windows 実機検証)は v0.1 後に。

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

- [ ] `cargo fmt --check` / `cargo clippy --all-targets --all-features`(warning 0)/ `cargo test` / `cargo deny check`
- [ ] `cargo xtask db-check`(生成物が pinned data と一致)
- [ ] 6台ベンチで代表フロー(flash→verify→run、gdb、monitor、target info、capabilities)を再確認
- [ ] CHANGELOG の `Unreleased` を `0.1.0 - <date>` に切る
- [ ] README(repo)に crates.io バッジ / インストール手順 / verified OS 明記
