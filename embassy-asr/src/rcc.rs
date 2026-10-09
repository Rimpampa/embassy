//! Reset and clock control for the ASR6601.
//!
//! The clock tree is deliberately configured before it is published through
//! [`clocks`]. Peripheral drivers may share RCC through the crate-private
//! helpers at the end of this module; their register updates are serialized by
//! a critical section.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::{Peri, pac, peripherals};

const RCO48M_HZ: u32 = 48_000_000;
const RCO4M_HZ: u32 = 3_600_000;
const XO24M_HZ: u32 = 24_000_000;
const XO32M_HZ: u32 = 32_000_000;
const LOW_SPEED_HZ: u32 = 32_768;

/// PAC gaps kept as named constants: the analog-window registers live outside
/// the PAC RCC block, and the PAC LORAC models a different register view
/// without these XO32M/reset fields.
const ANALOG_RCO32K_POWER_DOWN: u32 = 1 << 15;
const ANALOG_XO32K_POWER_DOWN: u32 = (1 << 13) | (1 << 14);
const ANALOG_XO24M_ENABLE: u32 = 1 << 3;
const ANALOG_XO24M_POWER_DOWN: u32 = 1 << 4;
const ANALOG_RCO48M_POWER_DOWN: u32 = 1 << 5;
const ANALOG_RCO4M_POWER_DOWN: u32 = 1 << 6;

const RCC_SR_ALL_DONE: u32 = 0x3f;

const LORAC_CR1_TCXO_ENABLE: u32 = 0x03;
const LORAC_CR1_XO32M_ENABLE: u32 = 1 << 2;
const LORAC_CR1_NRESET: u32 = 1 << 5;
const LORAC_CR1_POWER_ON_RESET: u32 = 1 << 7;
const LORAC_SR_XO32M_READY: u32 = 1 << 1;

const ASYNC_RESET_DELAY_US: u64 = 92;

static CLOCKS_INITIALIZED: AtomicBool = AtomicBool::new(false);
static SYSCLK_HZ: AtomicU32 = AtomicU32::new(0);
static HCLK_HZ: AtomicU32 = AtomicU32::new(0);
static PCLK0_HZ: AtomicU32 = AtomicU32::new(0);
static PCLK1_HZ: AtomicU32 = AtomicU32::new(0);

/// Oscillators controlled by RCC initialization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Oscillator {
    /// Internal 48 MHz RC oscillator.
    Rco48m,
    /// Internal 32.768 kHz RC oscillator.
    Rco32k,
    /// External 32.768 kHz crystal oscillator.
    Xo32k,
    /// External 24 MHz crystal oscillator.
    Xo24m,
    /// 32 MHz LoRa-radio crystal oscillator.
    Xo32m,
    /// Internal RC oscillator. Despite its name, the vendor reports 3.6 MHz.
    Rco4m,
}

/// System clock source.
///
/// The PAC also describes a PLL selector value. It is intentionally absent:
/// the vendor RCC API neither exposes PLL as a system-clock source nor
/// specifies its frequency or setup sequence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum SystemClockSource {
    /// Internal 48 MHz RC oscillator divided by two.
    #[default]
    Rco48mDiv2 = 0,
    /// Internal 32.768 kHz RC oscillator.
    Rco32k = 1,
    /// External 32.768 kHz crystal oscillator.
    Xo32k = 2,
    /// External 24 MHz crystal oscillator.
    Xo24m = 4,
    /// 32 MHz LoRa-radio crystal oscillator.
    Xo32m = 5,
    /// Internal RC oscillator (3.6 MHz according to the vendor clock API).
    Rco4m = 6,
    /// Internal 48 MHz RC oscillator.
    Rco48m = 7,
}

impl SystemClockSource {
    /// Nominal source frequency in hertz.
    pub const fn frequency_hz(self) -> u32 {
        match self {
            Self::Rco48mDiv2 => RCO48M_HZ / 2,
            Self::Rco32k | Self::Xo32k => LOW_SPEED_HZ,
            Self::Xo24m => XO24M_HZ,
            Self::Xo32m => XO32M_HZ,
            Self::Rco4m => RCO4M_HZ,
            Self::Rco48m => RCO48M_HZ,
        }
    }

    const fn oscillator(self) -> Oscillator {
        match self {
            Self::Rco48mDiv2 | Self::Rco48m => Oscillator::Rco48m,
            Self::Rco32k => Oscillator::Rco32k,
            Self::Xo32k => Oscillator::Xo32k,
            Self::Xo24m => Oscillator::Xo24m,
            Self::Xo32m => Oscillator::Xo32m,
            Self::Rco4m => Oscillator::Rco4m,
        }
    }
}

/// HCLK divider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum HclkDivider {
    /// Divide by 1.
    #[default]
    Div1 = 0,
    /// Divide by 2.
    Div2 = 1,
    /// Divide by 4.
    Div4 = 2,
    /// Divide by 8.
    Div8 = 3,
    /// Divide by 16.
    Div16 = 4,
    /// Divide by 32.
    Div32 = 5,
    /// Divide by 64.
    Div64 = 6,
    /// Divide by 128.
    Div128 = 7,
    /// Divide by 256.
    Div256 = 8,
    /// Divide by 512.
    Div512 = 9,
}

impl HclkDivider {
    /// Numeric divider.
    pub const fn divisor(self) -> u32 {
        1 << self as u8
    }
}

/// PCLK0 or PCLK1 divider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum PclkDivider {
    /// Divide by 1.
    #[default]
    Div1 = 0,
    /// Divide by 2.
    Div2 = 1,
    /// Divide by 4.
    Div4 = 2,
    /// Divide by 8.
    Div8 = 3,
    /// Divide by 16.
    Div16 = 4,
}

impl PclkDivider {
    /// Numeric divider.
    pub const fn divisor(self) -> u32 {
        1 << self as u8
    }
}

/// RCC initialization configuration.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Config {
    /// System clock source.
    pub system_clock: SystemClockSource,
    /// HCLK divider.
    pub hclk_divider: HclkDivider,
    /// PCLK0 divider relative to HCLK.
    pub pclk0_divider: PclkDivider,
    /// PCLK1 divider relative to HCLK.
    pub pclk1_divider: PclkDivider,
    /// Set the vendor TCXO-enable bits when starting the LoRa 32 MHz source.
    pub xo32m_uses_tcxo: bool,
    /// Maximum number of status-register polls for each readiness transition.
    ///
    /// This is a polling budget, not a duration: the CPU frequency can change
    /// while RCC is being initialized.
    pub readiness_poll_limit: u32,
}

impl Config {
    /// Configuration matching the documented reset clock tree.
    pub const fn new() -> Self {
        Self {
            system_clock: SystemClockSource::Rco48mDiv2,
            hclk_divider: HclkDivider::Div1,
            pclk0_divider: PclkDivider::Div1,
            pclk1_divider: PclkDivider::Div1,
            xo32m_uses_tcxo: false,
            readiness_poll_limit: 1_000_000,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

/// Initialized core and bus clock frequencies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct Clocks {
    /// System clock frequency in hertz.
    pub sysclk_hz: u32,
    /// CPU/AHB clock frequency in hertz.
    pub hclk_hz: u32,
    /// Peripheral bus 0 frequency in hertz.
    pub pclk0_hz: u32,
    /// Peripheral bus 1 frequency in hertz.
    pub pclk1_hz: u32,
}

impl Clocks {
    const fn from_config(config: Config) -> Self {
        let sysclk_hz = config.system_clock.frequency_hz();
        let hclk_hz = sysclk_hz / config.hclk_divider.divisor();
        Self {
            sysclk_hz,
            hclk_hz,
            pclk0_hz: hclk_hz / config.pclk0_divider.divisor(),
            pclk1_hz: hclk_hz / config.pclk1_divider.divisor(),
        }
    }
}

/// Hardware transition whose status did not become ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum WaitTarget {
    /// Internal 48 MHz RC oscillator.
    Rco48m,
    /// Internal 3.6 MHz RC oscillator.
    Rco4m,
    /// LoRa 32 MHz crystal oscillator.
    Xo32m,
    /// RCC clock-domain synchronization.
    ClockDomain,
}

/// RCC configuration or peripheral-control error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum Error {
    /// A bounded readiness wait expired.
    Timeout(WaitTarget),
    /// A reset requiring a timed release was requested before clock setup.
    ClocksNotInitialized,
    /// The vendor RCC does not provide a reset bit for this peripheral.
    ResetUnsupported(Peripheral),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Timeout(_) => f.write_str("RCC readiness wait expired"),
            Self::ClocksNotInitialized => f.write_str("RCC clocks are not initialized"),
            Self::ResetUnsupported(_) => f.write_str("peripheral has no RCC reset bit"),
        }
    }
}

impl core::error::Error for Error {}

/// Return the frequencies published by successful RCC initialization.
///
/// `None` is returned until [`init`] has completed all oscillator and clock
/// transitions.
pub fn clocks() -> Option<Clocks> {
    if !CLOCKS_INITIALIZED.load(Ordering::Acquire) {
        return None;
    }

    Some(Clocks {
        sysclk_hz: SYSCLK_HZ.load(Ordering::Relaxed),
        hclk_hz: HCLK_HZ.load(Ordering::Relaxed),
        pclk0_hz: PCLK0_HZ.load(Ordering::Relaxed),
        pclk1_hz: PCLK1_HZ.load(Ordering::Relaxed),
    })
}

/// Initialize the ASR6601 oscillator and core/bus clock tree.
///
/// The RCC singleton makes normal callers perform this operation once. The
/// selected oscillator is enabled before it is selected. PCLKs are temporarily
/// divided by 16, and HCLK/source ordering is chosen to avoid a transient clock
/// faster than either the existing or requested clock.
///
/// Matches vendor `system_init()` by enabling the AFEC clock before any
/// analog-window or `RAW_SR` access. Without that gate, oscillator readiness
/// bits read as clear and [`Error::Timeout`] is reported.
pub fn init(_rcc: Peri<'_, peripherals::RCC>, config: Config) -> Result<Clocks, Error> {
    CLOCKS_INITIALIZED.store(false, Ordering::Release);

    // Vendor `system_cm4.c` enables AFEC before oscillator or clock work.
    crate::afec::analog::enable_clock();

    enable_oscillator(
        config.system_clock.oscillator(),
        config.xo32m_uses_tcxo,
        config.readiness_poll_limit,
    )?;
    configure_clock_tree(config);

    let clocks = Clocks::from_config(config);
    SYSCLK_HZ.store(clocks.sysclk_hz, Ordering::Relaxed);
    HCLK_HZ.store(clocks.hclk_hz, Ordering::Relaxed);
    PCLK0_HZ.store(clocks.pclk0_hz, Ordering::Relaxed);
    PCLK1_HZ.store(clocks.pclk1_hz, Ordering::Relaxed);
    CLOCKS_INITIALIZED.store(true, Ordering::Release);
    Ok(clocks)
}

fn rcc() -> pac::Rcc {
    // RCC is a shared hardware service. Every read-modify-write performed by
    // this module is protected by a critical section.
    unsafe { pac::Rcc::steal() }
}

fn afec() -> pac::Afec {
    unsafe { pac::Afec::steal() }
}

fn lorac() -> pac::Lorac {
    unsafe { pac::Lorac::steal() }
}

fn update_bits(value: u32, mask: u32, set: bool) -> u32 {
    if set { value | mask } else { value & !mask }
}

fn current_system_frequency() -> Option<u32> {
    use pac::rcc::cr0::SysclkSel;
    // The hardware field is fully specified, so every bit pattern decodes;
    // only PLL has no vendor frequency to report.
    match rcc().cr0().read().sysclk_sel().variant() {
        SysclkSel::Rco48mDiv2 => Some(RCO48M_HZ / 2),
        SysclkSel::Rco32k | SysclkSel::Xo32k => Some(LOW_SPEED_HZ),
        SysclkSel::Pll => None,
        SysclkSel::Xo24m => Some(XO24M_HZ),
        SysclkSel::Xo32m => Some(XO32M_HZ),
        SysclkSel::Rco4m => Some(RCO4M_HZ),
        SysclkSel::Rco48m => Some(RCO48M_HZ),
    }
}

fn set_hclk_divider(divider: HclkDivider) {
    use pac::rcc::cr0::HclkDiv;
    let variant = match divider {
        HclkDivider::Div1 => HclkDiv::Value1,
        HclkDivider::Div2 => HclkDiv::Value2,
        HclkDivider::Div4 => HclkDiv::Value4,
        HclkDivider::Div8 => HclkDiv::Value8,
        HclkDivider::Div16 => HclkDiv::Value16,
        HclkDivider::Div32 => HclkDiv::Value32,
        HclkDivider::Div64 => HclkDiv::Value64,
        HclkDivider::Div128 => HclkDiv::Value128,
        HclkDivider::Div256 => HclkDiv::Value256,
        HclkDivider::Div512 => HclkDiv::Value512,
    };
    critical_section::with(|_| {
        rcc().cr0().modify(|_, w| w.hclk_div().variant(variant));
    });
}

fn set_pclk_dividers(pclk0: PclkDivider, pclk1: PclkDivider) {
    use pac::rcc::cr0::{Pclk0Div, Pclk1Div};
    fn map0(div: PclkDivider) -> Pclk0Div {
        match div {
            PclkDivider::Div1 => Pclk0Div::Value1,
            PclkDivider::Div2 => Pclk0Div::Value2,
            PclkDivider::Div4 => Pclk0Div::Value4,
            PclkDivider::Div8 => Pclk0Div::Value8,
            PclkDivider::Div16 => Pclk0Div::Value16,
        }
    }
    fn map1(div: PclkDivider) -> Pclk1Div {
        match div {
            PclkDivider::Div1 => Pclk1Div::Value1,
            PclkDivider::Div2 => Pclk1Div::Value2,
            PclkDivider::Div4 => Pclk1Div::Value4,
            PclkDivider::Div8 => Pclk1Div::Value8,
            PclkDivider::Div16 => Pclk1Div::Value16,
        }
    }
    critical_section::with(|_| {
        rcc().cr0().modify(|_, w| {
            w.pclk0_div().variant(map0(pclk0));
            w.pclk1_div().variant(map1(pclk1))
        });
    });
}

fn set_system_clock(source: SystemClockSource) {
    use pac::rcc::cr0::SysclkSel;
    let variant = match source {
        SystemClockSource::Rco48mDiv2 => SysclkSel::Rco48mDiv2,
        SystemClockSource::Rco32k => SysclkSel::Rco32k,
        SystemClockSource::Xo32k => SysclkSel::Xo32k,
        SystemClockSource::Xo24m => SysclkSel::Xo24m,
        SystemClockSource::Xo32m => SysclkSel::Xo32m,
        SystemClockSource::Rco4m => SysclkSel::Rco4m,
        SystemClockSource::Rco48m => SysclkSel::Rco48m,
    };
    critical_section::with(|_| {
        rcc().cr0().modify(|_, w| w.sysclk_sel().variant(variant));
    });
}

fn configure_clock_tree(config: Config) {
    let old_frequency = current_system_frequency();
    let new_frequency = config.system_clock.frequency_hz();

    // Protect both peripheral buses while HCLK and SYSCLK are in transition.
    set_pclk_dividers(PclkDivider::Div16, PclkDivider::Div16);

    match old_frequency {
        Some(old_frequency) if new_frequency >= old_frequency => {
            set_hclk_divider(config.hclk_divider);
            set_system_clock(config.system_clock);
        }
        Some(_) => {
            set_system_clock(config.system_clock);
            set_hclk_divider(config.hclk_divider);
        }
        None => {
            // PLL setup/frequency is not specified by the vendor RCC API.
            set_hclk_divider(HclkDivider::Div512);
            set_system_clock(config.system_clock);
            set_hclk_divider(config.hclk_divider);
        }
    }

    set_pclk_dividers(config.pclk0_divider, config.pclk1_divider);
}

fn wait_until(poll_limit: u32, target: WaitTarget, mut ready: impl FnMut() -> bool) -> Result<(), Error> {
    for _ in 0..poll_limit {
        if ready() {
            return Ok(());
        }
        core::hint::spin_loop();
    }

    if ready() { Ok(()) } else { Err(Error::Timeout(target)) }
}

fn enable_oscillator(oscillator: Oscillator, xo32m_uses_tcxo: bool, poll_limit: u32) -> Result<(), Error> {
    use crate::afec::analog::{REG_02, REG_06};

    // Analog-window helpers gate AFEC; keep the digital block live for RAW_SR.
    crate::afec::analog::enable_clock();

    match oscillator {
        Oscillator::Rco48m => {
            REG_06.clear_bits(ANALOG_RCO48M_POWER_DOWN);
            wait_until(poll_limit, WaitTarget::Rco48m, || {
                afec().raw_sr().read().rco24m_ready().bit_is_set()
            })
        }
        Oscillator::Rco32k => {
            REG_02.clear_bits(ANALOG_RCO32K_POWER_DOWN);
            Ok(())
        }
        Oscillator::Xo32k => {
            REG_02.clear_bits(ANALOG_XO32K_POWER_DOWN);
            Ok(())
        }
        Oscillator::Xo24m => {
            REG_06.modify(ANALOG_XO24M_ENABLE | ANALOG_XO24M_POWER_DOWN, ANALOG_XO24M_ENABLE);
            Ok(())
        }
        Oscillator::Xo32m => {
            set_peripheral_clock_raw(Peripheral::Lora, true, poll_limit)?;

            critical_section::with(|_| {
                lorac().cr1().modify(|r, w| {
                    let mut value = r.bits();
                    if value & LORAC_CR1_NRESET == 0 {
                        value |= LORAC_CR1_NRESET;
                        value &= !LORAC_CR1_POWER_ON_RESET;
                    }
                    if xo32m_uses_tcxo {
                        value |= LORAC_CR1_TCXO_ENABLE;
                    }
                    value |= LORAC_CR1_XO32M_ENABLE;
                    unsafe { w.bits(value) }
                });
            });

            wait_until(poll_limit, WaitTarget::Xo32m, || {
                lorac().sr().read().bits() & LORAC_SR_XO32M_READY != 0
            })
        }
        Oscillator::Rco4m => {
            REG_06.clear_bits(ANALOG_RCO4M_POWER_DOWN);
            wait_until(poll_limit, WaitTarget::Rco4m, || {
                afec().raw_sr().read().rco4m_ready().bit_is_set()
            })
        }
    }
}

/// RCC-controlled peripheral clock/reset identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum Peripheral {
    /// Smart access controller.
    Sac,
    /// Security engine.
    Sec,
    /// CRC engine.
    Crc,
    /// Real-time clock.
    Rtc,
    /// Window watchdog.
    Wdg,
    /// Independent watchdog.
    Iwdg,
    /// Low-power timer 0.
    Lptimer0,
    /// Basic timer 1.
    Bstimer1,
    /// Basic timer 0.
    Bstimer0,
    /// General-purpose timer 3.
    Timer3,
    /// General-purpose timer 2.
    Timer2,
    /// General-purpose timer 1.
    Timer1,
    /// General-purpose timer 0.
    Timer0,
    /// GPIO port A.
    GpioA,
    /// GPIO port B.
    GpioB,
    /// GPIO port C.
    GpioC,
    /// GPIO port D.
    GpioD,
    /// LoRa controller.
    Lora,
    /// DAC controller.
    Dac,
    /// LCD controller.
    Lcd,
    /// Analog front-end controller.
    Afec,
    /// ADC controller.
    Adc,
    /// I2C bus 2.
    I2c2,
    /// I2C bus 1.
    I2c1,
    /// I2C bus 0.
    I2c0,
    /// Quad-SPI controller.
    Qspi,
    /// Synchronous serial port 2.
    Ssp2,
    /// Synchronous serial port 1.
    Ssp1,
    /// Synchronous serial port 0.
    Ssp0,
    /// Low-power UART.
    Lpuart,
    /// UART3.
    Uart3,
    /// UART2.
    Uart2,
    /// UART1.
    Uart1,
    /// UART0.
    Uart0,
    /// DMA controller 1.
    Dma1,
    /// DMA controller 0.
    Dma0,
    /// I2S controller.
    I2s,
    /// Random number generator.
    Rng,
    /// Low-power timer 1.
    Lptimer1,
    /// System configuration controller.
    Syscfg,
    /// Power controller.
    Pwr,
}

#[derive(Clone, Copy)]
enum ClockRegister {
    Cgr0,
    Cgr1,
    Cgr2,
}

fn modify_clock_register(register: ClockRegister, mask: u32, enable: bool) {
    critical_section::with(|_| {
        let rcc = rcc();
        match register {
            ClockRegister::Cgr0 => rcc
                .cgr0()
                .modify(|r, w| unsafe { w.bits(update_bits(r.bits(), mask, enable)) }),
            ClockRegister::Cgr1 => rcc
                .cgr1()
                .modify(|r, w| unsafe { w.bits(update_bits(r.bits(), mask, enable)) }),
            ClockRegister::Cgr2 => rcc
                .cgr2()
                .modify(|r, w| unsafe { w.bits(update_bits(r.bits(), mask, enable)) }),
        };
    });
}

/// Gate one basic-domain peripheral clock.
///
/// Every peripheral except GPIOA..D has a generated clock-gate accessor;
/// GPIO gates are absent from the PAC RCC, so their CGR0 positions stay
/// computed (documented gap). Dual-domain peripherals (RTC/IWDG/LPTIMER/
/// LCD/LPUART) are not handled here — see `set_peripheral_clock_raw`.
fn set_basic_clock_gate(peripheral: Peripheral, enable: bool) {
    critical_section::with(|_| {
        let rcc = rcc();
        match peripheral {
            Peripheral::Timer3 => rcc.cgr0().modify(|_, w| w.gptim3_clk_en().bit(enable)),
            Peripheral::Timer2 => rcc.cgr0().modify(|_, w| w.gptim2_clk_en().bit(enable)),
            Peripheral::Timer1 => rcc.cgr0().modify(|_, w| w.gptim1_clk_en().bit(enable)),
            Peripheral::Timer0 => rcc.cgr0().modify(|_, w| w.gptim0_clk_en().bit(enable)),
            Peripheral::Lora => rcc.cgr0().modify(|_, w| w.lorac_clk_en().bit(enable)),
            Peripheral::Dac => rcc.cgr0().modify(|_, w| w.dacctrl_clk_en().bit(enable)),
            Peripheral::Afec => rcc.cgr0().modify(|_, w| w.afec_clk_en().bit(enable)),
            Peripheral::Adc => rcc.cgr0().modify(|_, w| w.adc_clk_en().bit(enable)),
            Peripheral::I2c2 => rcc.cgr0().modify(|_, w| w.i2c2_clk_en().bit(enable)),
            Peripheral::I2c1 => rcc.cgr0().modify(|_, w| w.i2c1_clk_en().bit(enable)),
            Peripheral::I2c0 => rcc.cgr0().modify(|_, w| w.i2c0_clk_en().bit(enable)),
            Peripheral::Ssp2 => rcc.cgr0().modify(|_, w| w.ssp2_clk_en().bit(enable)),
            Peripheral::Ssp1 => rcc.cgr0().modify(|_, w| w.ssp1_clk_en().bit(enable)),
            Peripheral::Ssp0 => rcc.cgr0().modify(|_, w| w.ssp0_clk_en().bit(enable)),
            Peripheral::Uart3 => rcc.cgr0().modify(|_, w| w.uart3_clk_en().bit(enable)),
            Peripheral::Uart2 => rcc.cgr0().modify(|_, w| w.uart2_clk_en().bit(enable)),
            Peripheral::Uart1 => rcc.cgr0().modify(|_, w| w.uart1_clk_en().bit(enable)),
            Peripheral::Uart0 => rcc.cgr0().modify(|_, w| w.uart0_clk_en().bit(enable)),
            Peripheral::Syscfg => rcc.cgr0().modify(|_, w| w.syscfg_clk_en().bit(enable)),
            Peripheral::GpioA => rcc.cgr0().modify(|r, w| unsafe {
                w.bits(if enable {
                    r.bits() | (1 << 25)
                } else {
                    r.bits() & !(1 << 25)
                })
            }),
            Peripheral::GpioB => rcc.cgr0().modify(|r, w| unsafe {
                w.bits(if enable {
                    r.bits() | (1 << 24)
                } else {
                    r.bits() & !(1 << 24)
                })
            }),
            Peripheral::GpioC => rcc.cgr0().modify(|r, w| unsafe {
                w.bits(if enable {
                    r.bits() | (1 << 23)
                } else {
                    r.bits() & !(1 << 23)
                })
            }),
            Peripheral::GpioD => rcc.cgr0().modify(|r, w| unsafe {
                w.bits(if enable {
                    r.bits() | (1 << 22)
                } else {
                    r.bits() & !(1 << 22)
                })
            }),
            Peripheral::Bstimer1 => rcc.cgr0().modify(|_, w| w.basictim1_clk_en().bit(enable)),
            Peripheral::Bstimer0 => rcc.cgr0().modify(|_, w| w.basictim0_clk_en().bit(enable)),
            Peripheral::Crc => rcc.cgr0().modify(|_, w| w.crc_clk_en().bit(enable)),
            Peripheral::Dma1 => rcc.cgr0().modify(|_, w| w.dmac1_clk_en().bit(enable)),
            Peripheral::Dma0 => rcc.cgr0().modify(|_, w| w.dmac0_clk_en().bit(enable)),
            Peripheral::Pwr => rcc.cgr0().modify(|_, w| w.pwr_clk_en().bit(enable)),
            Peripheral::Sec => rcc.cgr1().modify(|_, w| w.sec_clk_en().bit(enable)),
            Peripheral::Qspi => rcc.cgr1().modify(|_, w| w.qspi_clk_en().bit(enable)),
            Peripheral::Sac => rcc.cgr1().modify(|_, w| w.sac_clk_en().bit(enable)),
            Peripheral::I2s => rcc.cgr1().modify(|_, w| w.i2s_clk_en().bit(enable)),
            Peripheral::Rng => rcc.cgr1().modify(|_, w| w.rngc_clk_en().bit(enable)),
            Peripheral::Wdg => rcc.cgr1().modify(|_, w| {
                w.wwdg_clk_en().bit(enable);
                w.wwdg_cnt_clk_en().bit(enable)
            }),
            Peripheral::Rtc
            | Peripheral::Iwdg
            | Peripheral::Lptimer0
            | Peripheral::Lptimer1
            | Peripheral::Lcd
            | Peripheral::Lpuart => unreachable!("dual-domain clocks use set_peripheral_clock_raw"),
        }
    });
}

fn wait_rcc_status(mask: u32, poll_limit: u32) -> Result<(), Error> {
    wait_until(poll_limit, WaitTarget::ClockDomain, || {
        rcc().sr().read().bits() & mask == mask
    })
}

fn set_dual_domain_clock(
    functional_register: ClockRegister,
    functional_mask: u32,
    aon_mask: u32,
    enable: bool,
    poll_limit: u32,
) -> Result<(), Error> {
    modify_clock_register(functional_register, functional_mask, enable);
    wait_rcc_status(RCC_SR_ALL_DONE, poll_limit)?;
    modify_clock_register(ClockRegister::Cgr2, aon_mask, enable);
    wait_rcc_status(aon_mask, poll_limit)
}

fn set_lptimer_clock(
    functional_mask: u32,
    pclk_mask: u32,
    aon_mask: u32,
    enable: bool,
    poll_limit: u32,
) -> Result<(), Error> {
    if enable {
        modify_clock_register(ClockRegister::Cgr1, pclk_mask, true);
        let _ = wait_rcc_status(RCC_SR_ALL_DONE, poll_limit);
        modify_clock_register(ClockRegister::Cgr2, aon_mask, true);
        // v1.6.2 SVD's cgr2/sr always-on sync bits are flaky and a bootloader
        // may leave them in a state that never asserts. The functional gate in
        // cgr1 is sufficient for polling `now()`; don't fail init if the sync
        // never asserts.
        let _ = wait_rcc_status(aon_mask, poll_limit);
        modify_clock_register(ClockRegister::Cgr1, functional_mask, true);
    } else {
        modify_clock_register(ClockRegister::Cgr1, functional_mask, false);
        let _ = wait_rcc_status(RCC_SR_ALL_DONE, poll_limit);
        modify_clock_register(ClockRegister::Cgr2, aon_mask, false);
        let _ = wait_rcc_status(aon_mask, poll_limit);
        // The vendor intentionally leaves the PCLK gate enabled on disable.
    }
    Ok(())
}

fn set_peripheral_clock_raw(peripheral: Peripheral, enable: bool, poll_limit: u32) -> Result<(), Error> {
    match peripheral {
        Peripheral::Lpuart => set_dual_domain_clock(ClockRegister::Cgr0, 1 << 16, 1 << 2, enable, poll_limit),
        Peripheral::Lcd => set_dual_domain_clock(ClockRegister::Cgr0, 1 << 6, 1 << 3, enable, poll_limit),
        Peripheral::Rtc => set_dual_domain_clock(ClockRegister::Cgr1, 1 << 1, 1 << 1, enable, poll_limit),
        Peripheral::Iwdg => set_dual_domain_clock(ClockRegister::Cgr1, 1 << 3, 1 << 0, enable, poll_limit),
        Peripheral::Lptimer0 => set_lptimer_clock(1 << 4, 1 << 9, 1 << 4, enable, poll_limit),
        Peripheral::Lptimer1 => set_lptimer_clock(1 << 11, 1 << 12, 1 << 5, enable, poll_limit),
        _ => {
            set_basic_clock_gate(peripheral, enable);
            Ok(())
        }
    }
}

/// Enable a peripheral clock using the default bounded synchronization budget.
pub(crate) fn enable_peripheral(peripheral: Peripheral) -> Result<(), Error> {
    set_peripheral_clock_raw(peripheral, true, Config::new().readiness_poll_limit)
}

/// Disable a peripheral clock using the default bounded synchronization budget.
pub(crate) fn disable_peripheral(peripheral: Peripheral) -> Result<(), Error> {
    set_peripheral_clock_raw(peripheral, false, Config::new().readiness_poll_limit)
}

fn reset_needs_release_delay(peripheral: Peripheral) -> bool {
    matches!(
        peripheral,
        Peripheral::Lptimer0
            | Peripheral::Lptimer1
            | Peripheral::Lcd
            | Peripheral::Rtc
            | Peripheral::Iwdg
            | Peripheral::Lpuart
    )
}

/// Assert or release one peripheral's active-low reset line.
///
/// Every peripheral except GPIOA..D and Syscfg/Pwr has a generated reset
/// accessor; the GPIO reset bit has no PAC field (documented gap) and
/// Syscfg/Pwr have no reset at all (`Error::ResetUnsupported`).
fn set_reset_line(peripheral: Peripheral, asserted: bool) -> Result<(), Error> {
    // Active-low: asserting clears the bit.
    if matches!(peripheral, Peripheral::Syscfg | Peripheral::Pwr) {
        return Err(Error::ResetUnsupported(peripheral));
    }
    let release = !asserted;
    critical_section::with(|_| {
        let rcc = rcc();
        match peripheral {
            Peripheral::Sac => rcc.rst0().modify(|_, w| w.sac_rst_n().bit(release)),
            Peripheral::Sec => rcc.rst0().modify(|_, w| w.sec_rst_n().bit(release)),
            Peripheral::Crc => rcc.rst0().modify(|_, w| w.crc_rst_n().bit(release)),
            Peripheral::Rtc => rcc.rst0().modify(|_, w| w.rtc_rst_n().bit(release)),
            Peripheral::Wdg => rcc.rst0().modify(|_, w| w.wwdg_rst_n().bit(release)),
            Peripheral::Iwdg => rcc.rst0().modify(|_, w| w.iwdg_rst_n().bit(release)),
            Peripheral::Lptimer0 => rcc.rst0().modify(|_, w| w.lptim0_rst_n().bit(release)),
            Peripheral::Bstimer1 => rcc.rst0().modify(|_, w| w.basictim1_rst_n().bit(release)),
            Peripheral::Bstimer0 => rcc.rst0().modify(|_, w| w.basictim0_rst_n().bit(release)),
            Peripheral::Timer3 => rcc.rst0().modify(|_, w| w.gptim3_rst_n().bit(release)),
            Peripheral::Timer2 => rcc.rst0().modify(|_, w| w.gptim2_rst_n().bit(release)),
            Peripheral::Timer1 => rcc.rst0().modify(|_, w| w.gptim1_rst_n().bit(release)),
            Peripheral::Timer0 => rcc.rst0().modify(|_, w| w.gptim0_rst_n().bit(release)),
            Peripheral::GpioA | Peripheral::GpioB | Peripheral::GpioC | Peripheral::GpioD => {
                rcc.rst0().modify(|r, w| unsafe {
                    w.bits(if release {
                        r.bits() | (1 << 13)
                    } else {
                        r.bits() & !(1 << 13)
                    })
                })
            }
            Peripheral::Lora => rcc.rst0().modify(|_, w| w.lorac_rst_n().bit(release)),
            Peripheral::Dac => rcc.rst0().modify(|_, w| w.dacctrl_rst_n().bit(release)),
            Peripheral::Lcd => rcc.rst0().modify(|_, w| w.lcd_rst_n().bit(release)),
            Peripheral::Afec => rcc.rst0().modify(|_, w| w.afec_rst_n().bit(release)),
            Peripheral::Adc => rcc.rst0().modify(|_, w| w.adc_rst_n().bit(release)),
            Peripheral::I2c2 => rcc.rst0().modify(|_, w| w.i2c2_rst_n().bit(release)),
            Peripheral::I2c1 => rcc.rst0().modify(|_, w| w.i2c1_rst_n().bit(release)),
            Peripheral::I2c0 => rcc.rst0().modify(|_, w| w.i2c0_rst_n().bit(release)),
            Peripheral::Qspi => rcc.rst0().modify(|_, w| w.qspi_rst_n().bit(release)),
            Peripheral::Ssp2 => rcc.rst0().modify(|_, w| w.ssp2_rst_n().bit(release)),
            Peripheral::Ssp1 => rcc.rst0().modify(|_, w| w.ssp1_rst_n().bit(release)),
            Peripheral::Ssp0 => rcc.rst0().modify(|_, w| w.ssp0_rst_n().bit(release)),
            Peripheral::Lpuart => rcc.rst0().modify(|_, w| w.lpuart_rst_n().bit(release)),
            Peripheral::Uart3 => rcc.rst0().modify(|_, w| w.uart3_rst_n().bit(release)),
            Peripheral::Uart2 => rcc.rst0().modify(|_, w| w.uart2_rst_n().bit(release)),
            Peripheral::Uart1 => rcc.rst0().modify(|_, w| w.uart1_rst_n().bit(release)),
            Peripheral::Uart0 => rcc.rst0().modify(|_, w| w.uart0_rst_n().bit(release)),
            Peripheral::Dma1 => rcc.rst1().modify(|_, w| w.dmac1_rst_n().bit(release)),
            Peripheral::Dma0 => rcc.rst1().modify(|_, w| w.dmac0_rst_n().bit(release)),
            Peripheral::I2s => rcc.rst1().modify(|_, w| w.i2s_rst_n().bit(release)),
            Peripheral::Rng => rcc.rst1().modify(|_, w| w.rngc_rst_n().bit(release)),
            Peripheral::Lptimer1 => rcc.rst1().modify(|_, w| w.lptim1_rst_n().bit(release)),
            // Syscfg/Pwr rejected above: the vendor provides no reset bits.
            _ => unreachable!(),
        };
    });
    Ok(())
}

fn asynchronous_release_delay() -> Result<(), Error> {
    let hclk_hz = clocks().ok_or(Error::ClocksNotInitialized)?.hclk_hz as u64;
    let cycles = hclk_hz
        .saturating_mul(ASYNC_RESET_DELAY_US)
        .div_ceil(1_000_000)
        .min(u32::MAX as u64) as u32;
    cortex_m::asm::delay(cycles);
    Ok(())
}

/// Assert a peripheral's active-low reset line.
pub(crate) fn assert_peripheral_reset(peripheral: Peripheral) -> Result<(), Error> {
    set_reset_line(peripheral, true)
}

/// Release a peripheral reset and honor the vendor's asynchronous-domain delay.
pub(crate) fn release_peripheral_reset(peripheral: Peripheral) -> Result<(), Error> {
    if reset_needs_release_delay(peripheral) && clocks().is_none() {
        return Err(Error::ClocksNotInitialized);
    }
    set_reset_line(peripheral, false)?;
    if reset_needs_release_delay(peripheral) {
        asynchronous_release_delay()?;
    }
    Ok(())
}

/// Pulse a peripheral reset line.
pub(crate) fn reset_peripheral(peripheral: Peripheral) -> Result<(), Error> {
    if reset_needs_release_delay(peripheral) && clocks().is_none() {
        return Err(Error::ClocksNotInitialized);
    }
    assert_peripheral_reset(peripheral)?;
    release_peripheral_reset(peripheral)
}
