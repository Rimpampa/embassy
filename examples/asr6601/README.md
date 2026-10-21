# ASR6601 examples

Examples for the `embassy-asr` HAL.

- `blinky` — toggles PA0 at 1 Hz and exercises the LPTIM0 time driver.
- `uart` — echoes bytes on UART0 (PB0 TX, PB1 RX, AF1).

Build from this directory:

```sh
cargo build --release
```

The runner uses `probe-rs run --chip ASR6601`; adjust the chip name or use
your board's flashing tool if it differs. The `memory.x` file uses the
128 KiB main-flash variant; change it for 256 KiB parts.
