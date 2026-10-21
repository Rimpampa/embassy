# Changelog for embassy-asr

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## Unreleased

- Initial HAL bring-up for the ASR6601.
- GPIO, UART, LPUART, SPI, I2C, timer, LPTIMER, DMA, ADC, DAC, CRC, RNG,
  flash, power, AFEC, watchdog, and RCC drivers.
- `embassy-time` driver using LPTIM0 at 32,768 Hz.
