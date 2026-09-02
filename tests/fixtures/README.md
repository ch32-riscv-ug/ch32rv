# テスト用 fixtures

手動テスト([docs/testing.ja.md](../../docs/testing.ja.md))で転送に使う、ツールチェーン不要のバイナリ。

## flash 経路テスト(全 family 共通)

| ファイル | 内容 | 用途 |
|---|---|---|
| `pattern-4k.bin` | 4096 byte、`byte[i] = i & 0xFF`(00 01 02 … ff 00 … の ramp) | flash 往復(backup → 書込 → readback verify → restore)。決定的で verify が容易 |

実行可能ファームではない。安全に使うため先に対象領域を `read` で backup し、テスト後に戻す。

## 走行テスト(family 別、Linux 実機検証済み)

`runtest/runtest.ino`(GPIO 無しの counter loop)を family 別にビルドしたもの。`ch32rv flash <bin> --confirm-run pc` で「書いた image が実際に走る」ことを確認する。

| ファイル | family | Linux flash+confirm-run |
|---|---|---|
| `runtest-ch32v003.bin` | CH32V003 | OK |
| `runtest-ch32v103.bin` | CH32V103 | OK |
| `runtest-ch32v203.bin` | CH32V20x | OK |
| `runtest-ch32v307.bin` | CH32V30x | OK |
| `runtest-ch32l103.bin` | CH32L103 | OK |

他 family(X035 等)は基板を繋いで下記で追加できる。

## semihosting 走行テスト(`run` HIL、実機検証済み)

`ch32rv run <bin> --exit-on semihosting` の検証用。RISC-V semihosting の `slli/ebreak/srai`
シーケンスで、まず `SYS_WRITE0` で文字列を出力し、続いて `SYS_EXIT_EXTENDED` で終了コード **42**
を返す。stack を使わず flash 先頭(`0x08000000`)から走る base-ISA コードのみなので、CH32 RISC-V
の family を問わず動く。

| ファイル | 期待動作 | V307 実機 |
|---|---|---|
| `semihosting.bin` | `hello from semihosting\n` を出力し exit 42 | OK |

```sh
ch32rv run tests/fixtures/semihosting.bin --probe serial:<SN> --exit-on semihosting
# stdout: hello from semihosting
# 終了コード: 42
```

## 再生成

```sh
# pattern
./make-fixtures.sh
# runtest(family 別、arduino-cli + ch32-riscv-ug:ch32v core が必要)
arduino-cli compile -b ch32-riscv-ug:ch32v:<CH32V003|CH32V103|CH32V203|CH32V307|CH32L103|CH32X035|...> \
  --output-dir /tmp/rt runtest
cp /tmp/rt/runtest.ino.bin runtest-<family>.bin

# semihosting(riscv-none-embed-gcc / riscv-none-elf-gcc が必要)
GCC=riscv-none-embed-gcc  # 例: Arduino core 同梱の toolchain
$GCC -march=rv32imac -mabi=ilp32 -nostdlib -nostartfiles \
  -Wl,-Ttext=0x08000000 -Wl,-e,_start -o /tmp/semi.elf semihosting/semihosting.S
${GCC%-gcc}-objcopy -O binary /tmp/semi.elf semihosting.bin
```

## sha256

```
c8f5d0341d54d951a71b136e6e2afcb14d11ed8489a7ae126a8fee0df6ecf193  pattern-4k.bin
705bfa079494a464c15030473ec70d66da5d440b71d23162ab6fa4648dcd6b80  runtest-ch32v003.bin
5a2632919638df65b9a2b3007d570121bcbd2fff58c3d3db0d38a3cc2e88a3bc  runtest-ch32v103.bin
9dca894b7d846f07cd80073b35d2cc2e81f579c03addd4b17fe3b78673dee120  runtest-ch32v203.bin
2171910d36ecb57cba92daf34860b87b3908de039ab5c90812333337ffbb6e10  runtest-ch32v307.bin
e8a5fe50025788572a06c74e1abbd0c8a20d816030815426fa5da354dfa3a538  runtest-ch32l103.bin
c6049320d46adad08a3902ddc6883f66489f0d3fec06f5c0d8eec75efa4dab92  semihosting.bin
```
