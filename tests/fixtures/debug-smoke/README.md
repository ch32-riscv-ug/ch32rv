# UARTなしのV205 / X315テスト

Arduino初期化・GPIO・UARTに依存しない小さなfirmware。LinkE経由でCPUの走行、RAM初期化、DMDATA / DMSEQ / RTTの双方向通信を切り分ける。V205のFlash別名アドレス0からの起動にも対応する。

既存のArduinoCore-CH32RVのGCCを使うか、`CH32_GCC_BIN`に`riscv-none-elf-gcc`と`objcopy`のあるディレクトリを指定する。

```sh
python3 tests/fixtures/debug-smoke/build.py --out target/debug-smoke
# 事前に元のFlash全体を退避する。flash / runは既存firmwareを変更する。
ch32rv read --region code -o backup.bin --probe serial:<SN>
# dmdata / dmseq / rttそれぞれについて実施。標準入力のZをtargetへ送る。
printf Z | ch32rv run target/debug-smoke/dmdata.elf --source dmdata --duration 2 --probe serial:<SN>
ch32rv read --range 0x20001000+20 --format hex-dump --probe serial:<SN>
ch32rv flash backup.bin --probe serial:<SN>
```

`0x20001000`にlittle-endianの32bit値が5個並ぶ。

| オフセット | 期待値 |
|---|---|
| +0 | `0x534d4f4b` (起動完了) |
| +4 | ループ回数、0より大きい |
| +8 | RAM自己テスト失敗数、0 |
| +12 | 最後に受信したbyte、Zなら90 |
| +16 | 受信数、1以上 |

DMDATAは`OK\n`、DMSEQは`O\n`、RTTは`RTT OK\n`を繰り返す。DMDATA firmwareはLinkEの`monitor --source sdi`の出力確認にも使える。スタックは両チップのRAM内の`0x20007ff0`、結果は`0x20001000`に固定するため、他のRAM配置のチップへそのまま流用しない。

CH32X315の`read --region code`はDBのzero-wait領域192 KiBまで。非zero-wait領域も退避する場合は総Flash480 KiBを`--range 0x08000000+491520`で読み出す。現状のflash入力の容量判定は192 KiBであり、全480 KiBのbackupをそのままflashに渡せない。今回の復元手順と実測結果は[実機記録](../../../docs/data-requests/measured/flash-debug-v205-x315-2026-10-08.md)を参照。
