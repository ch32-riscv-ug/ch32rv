// en: Pin-agnostic run self-test. No GPIO, so it compiles and runs on every CH32 family.
// It just spins doing volatile work; `ch32rv flash --confirm-run` verifies the PC stays in flash
// (i.e. the chip actually executes the programmed image). Not a visible blink - use it as a
// "does the flashed image run" fixture. Build per family with arduino-cli (see ../README.md).
// ja: ピン非依存の走行自己テスト。GPIO を使わないので全 family でコンパイル・走行する。
// volatile なループを回すだけで、`ch32rv flash --confirm-run` が PC が flash 内に留まること
// (=書いた image が実際に実行される)を検証する。LED は光らない。
volatile unsigned long counter;
void setup() { counter = 0; }
void loop() { counter++; delay(1); }
