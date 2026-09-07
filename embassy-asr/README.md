# embassy-asr

[Embassy](https://embassy.dev/) support for ASR microcontrollers.

## Current support

- ASR6601 (verified against `~/devel/optimon/ASR6601/SVD/tremo.svd` v1.6.2 and `DOC/english/ASR6601_Reference_manual_V1.5.0.pdf`)
- Peripheral ownership and type-level interrupt infrastructure
- `embassy-time` driver using LPTIM0 at 32,768 Hz (from XO32K, `time-driver-lptim0`)
- Cortex-M thread executor support through `embassy-executor`
- Early HAL drivers for GPIO, UART, SPI, I2C, timers, DMA, ADC, DAC,
  LPUART, LPTIMER, CRC, RNG, flash, power, AFEC, and RCC

## LPTIM0 time driver

Enable the `time-driver-lptim0` feature (default) to use LPTIM0 as Embassy's
monotonic clock. `time-driver-rtc` is deprecated but kept for compatibility.

The driver:

- requires a working 32.768 kHz crystal on XO32K (same domain as RTC);
- resets LPTIM0, selects XO32K in `RCC.CR1` (`lptim0_clk_sel=2`), sets `ARR=0xFFFF`,
  enables `ARRM` for 16→64 bit extension and `CMPM` for alarms during
  `embassy_asr::init(Config::default())`;
- owns LPTIM0 exclusively, so application code must not access LPTIM0 through the
  raw PAC;
- free-runs at 32,768 Hz (2 s per overflow), defers far-future compares >0xC000 ticks
  to `next_period()` like `embassy-stm32/src/time_driver/lptim.rs:314`;
- supports one global Embassy time driver; and
- enables the `LPTIMER0` interrupt at NVIC priority 2.

PAC is `Rimpampa/ASR6601-PAC@svd` (official `tremo.svd` v1.6.2) via `path = "../../ASR6601-PAC"` (`~/devel/rust/ASR6601-PAC` locally, don't push).
Vendor `tremo_lptimer`/`tremo_rcc`/`tremo_gpio` were used to verify `CGR1`/`CR1`/`SR1`/`IER/ISR/ICR/CSR` layouts (e.g. `CFGR.COUNTMODE=0x800000`, `CR.ENABLE=0x1`).

## RTC time driver (deprecated)

Enable `time-driver-rtc` to use the RTC calendar (kept for backward compat, now
also backed by LPTIM0).

The application must provide a `critical-section` implementation. For a
single-core Cortex-M application, enable `cortex-m`'s
`critical-section-single-core` feature.

The current initialization intentionally does not preserve a bootloader or
previous low-power session's RTC calendar. Applications that need retained
wall-clock state must save it before initialization and restore it separately.
