# embassy-asr

[Embassy](https://embassy.dev/) support for ASR microcontrollers.

## Current support

- ASR6601, verified against the official `tremo.svd` v1.6.2 and the ASR6601
  Reference Manual v1.5.0
- Peripheral ownership and type-level interrupt infrastructure
- `embassy-time` driver using LPTIM0 at 32,768 Hz from XO32K
  (`time-driver-lptim0`, enabled by default)
- Cortex-M thread executor support through `embassy-executor`
- Drivers for GPIO, UART, LPUART, SPI, I2C, timers, DMA, ADC, DAC, CRC, RNG,
  flash, power, AFEC, and RCC

## Documentation

- [API documentation](https://docs.embassy.dev/embassy-asr/)
- [Source](https://github.com/embassy-rs/embassy/tree/main/embassy-asr)

## Time driver

The default `time-driver-lptim0` feature uses LPTIM0 as Embassy's monotonic
clock. It requires a working 32.768 kHz crystal on XO32K, takes exclusive
ownership of LPTIM0, and enables the `LPTIM0` interrupt at NVIC priority 2.
Application code must not access LPTIM0 through the raw PAC.

Register programming follows the vendor `tremo_lptimer` / `tremo_rcc` drivers
and the SDK `lptimer_wakeup_stop` example. The PAC is the official
`asr6601-pac` 0.1.1 from crates.io.

## Critical sections

The application must provide a `critical-section` implementation. For a
single-core Cortex-M application, enable `cortex-m`'s
`critical-section-single-core` feature.

## Limitations

- The time driver requires an external 32.768 kHz crystal. Boards without one
  cannot use `time-driver-lptim0`.
- The LPTIM0 compare channel on this silicon can miss very short deadlines;
  sub-millisecond alarms may be deferred by up to one 16-bit counter period.
  See `src/time_driver.rs` for details.
- DMA, SPI, I2C, ADC, DAC, flash, and the watchdog drivers have had limited
  hardware validation so far. Test them on your board before relying on them
  in production.
