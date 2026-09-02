# テスト用 fixtures

手動テスト([docs/testing-non-linux.ja.md](../../docs/testing-non-linux.ja.md))で転送に使う、ツールチェーン不要のバイナリ。

| ファイル | 内容 | 用途 |
|---|---|---|
| `pattern-4k.bin` | 4096 byte、`byte[i] = i & 0xFF`(00 01 02 … ff 00 … の ramp) | flash 往復テスト(backup → 書込 → readback verify → restore)。決定的なので verify が容易 |
| `make-fixtures.sh` | 上を再生成(Python3) | provenance / 再生成 |

- **これは実行可能ファームではない**(ただのパターン)。flash 経路(erase/program/verify)の検証専用。chip を走らせる確認には実 blink を別途用意する(手順書参照)。
- sha256(pattern-4k.bin): `c8f5d0341d54d951a71b136e6e2afcb14d11ed8489a7ae126a8fee0df6ecf193`
- **安全に使う**: 先に対象領域を `read` で backup し、テスト後に backup を書き戻す。reset vector を潰さないよう高位ページに書くのが無難(手順書の非破壊フロー参照)。
