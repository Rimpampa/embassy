//! Window / system watchdog (ARM SP805-style WDG block).
//!
//! Register programming follows the vendor `tremo_wdg` driver.
//!
//! PAC gap: the PAC WWDG registers (LOAD, VALUE, CONTROL, INTCLR, RIS, MIS, LOCK)
//! expose register accessors but no field accessors for individual bits.
//! Bit-level operations use raw constants matching the vendor header and Reference Manual.
//!
//! The PAC provides register-level accessors (load(), value(), control(), intclr(),
//! ris(), mis(), lock()) but no field-level accessors for individual bits like
//! CONTROL.RESEN, CONTROL.INTEN, LOCK.UNLOCK, etc. Raw bit manipulation is used.

use crate::rcc::{self, Peripheral};
use crate::{Peri, pac, peripherals};

const LOCK_TOKEN: u32 = 0x1acc_e551;
const CONTROL_RESEN: u32 = 1 << 1;
const CONTROL_INTEN: u32 = 1 << 0;
const RCC_RST_CR_WDG_RESET_REQ_EN: u32 = 1 << 4;

/// Owned system watchdog.
pub struct Watchdog<'d> {
    _peri: Peri<'d, peripherals::WDG>,
}

impl<'d> Watchdog<'d> {
    /// Enable the WDG clocks and leave the watchdog stopped.
    pub fn new(peri: Peri<'d, peripherals::WDG>) -> Self {
        let _ = rcc::enable_peripheral(Peripheral::Wdg);
        Self { _peri: peri }
    }

    /// Start the watchdog with the given reload value (raw counter ticks).
    ///
    /// The tick rate is the WDG block clock; there is no published divider to
    /// convert wall time, so callers tune this against the vendor example.
    /// Per the vendor driver, the first timeout asserts the WDG interrupt and
    /// reloads the counter; a second timeout without a feed resets the system.
    pub fn start(&self, reload_value: u32) {
        unlock();
        let regs = Self::regs();
        unsafe { regs.load().write_with_zero(|w| w.bits(reload_value)) };
        // Use modify to set only the needed bits (RESEN|INTEN), preserving others
        regs.control().modify(|_, w| unsafe { w.bits(CONTROL_RESEN | CONTROL_INTEN) });
        lock();

        critical_section::with(|_| {
            let rcc = unsafe { pac::Rcc::steal() };
            rcc.rst_cr()
                .modify(|_, w| w.wwdg_reset_req_en().set_bit());
        });
    }

    /// Feed the watchdog and clear its interrupt.
    pub fn pet(&self) {
        unlock();
        unsafe { Self::regs().intclr().write_with_zero(|w| w.bits(1)) };
        lock();
    }

    /// Stop the watchdog and disable its reset request.
    pub fn stop(&self) {
        critical_section::with(|_| {
            let rcc = unsafe { pac::Rcc::steal() };
            rcc.rst_cr()
                .modify(|_, w| w.wwdg_reset_req_en().clear_bit());
        });

        unlock();
        let regs = Self::regs();
        regs.control().modify(|_, w| unsafe { w.bits(0) });
        regs.load().modify(|_, w| unsafe { w.bits(0xffff_ffff) });
        lock();
    }

    /// Current down-counter value.
    pub fn value(&self) -> u32 {
        Self::regs().value().read().bits()
    }

    #[inline]
    fn regs() -> pac::Wwdg {
        unsafe { pac::Wwdg::steal() }
    }
}

fn unlock() {
    unsafe {
        Watchdog::regs().lock().write_with_zero(|w| w.bits(LOCK_TOKEN));
    }
}

fn lock() {
    unsafe {
        Watchdog::regs().lock().write_with_zero(|w| w.bits(!LOCK_TOKEN));
    }
}

impl Drop for Watchdog<'_> {
    fn drop(&mut self) {
        // Stop on drop so a discarded handle cannot reset the system out
        // from under the new owner; keep the handle alive to keep watching.
        self.stop();
        let _ = rcc::disable_peripheral(Peripheral::Wdg);
    }
}
