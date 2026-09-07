//! `embassy-time` driver using the ASR6601 LPTIM0.
//!
//! This follows the STM32 LPTIM time-driver pattern adapted to the ASR6601
//! vendor `tremo_lptimer` programming model:
//! * LPTIM0 is clocked from the always-on XO32K at 32_768 Hz
//! * The 16-bit counter free-runs with ARR = 0xFFFF and its overflow (ARRM)
//!   extends the counter to 64 bit
//! * CMP provides one-shot wakeups. Far-future alarms are deferred until
//!   the overflow period brings them within half the counter range.

use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use core::task::Waker;

use critical_section::{CriticalSection, Mutex};
use embassy_hal_internal::interrupt::{InterruptExt, Priority};
use embassy_time_driver::{Driver, TICK_HZ};
use embassy_time_queue_utils::Queue;

use crate::afec;
use defmt::debug;

pub static INTERRUPT_COUNT: AtomicU32 = AtomicU32::new(0);

pub fn interrupt_count() -> u32 {
    INTERRUPT_COUNT.load(Ordering::Relaxed)
}
use crate::pac::{self, Interrupt, interrupt};
use crate::rcc::{self, Peripheral as RccPeripheral};

const TIMER_TICK_HZ: u64 = 32_768;
const ARR_MAX: u16 = 0xFFFF;

// LPTIM0 register bit definitions (vendor tremo_lptimer.h / RM)
const ISR_CMPM: u32 = 1 << 0;
const ISR_ARRM: u32 = 1 << 1;
const ISR_CMPOK: u32 = 1 << 3;
const ISR_ARROK: u32 = 1 << 4;
const ISR_CFGROK: u32 = 1 << 7;
const ISR_CROK: u32 = 1 << 8;

const IER_ARRM: u32 = 1 << 1;
const IER_CMPM: u32 = 1 << 0;

const ICR_ARRM: u32 = 1 << 1;
const ICR_CMPM: u32 = 1 << 0;

const CSR_ARRM: u32 = 1 << 1;
const CSR_CMPM: u32 = 1 << 0;

const CR_ENABLE: u32 = 1 << 0;
const CR_SNGSTRT: u32 = 1 << 1;
const CR_CNTSTRT: u32 = 1 << 2;

const CFGR_PRESC_MASK: u32 = 0xe00;

// Analog XO32K power-down bits (AFEC REG_02 bits 13/14)
const ANALOG_XO32K_POWER_DOWN: u32 = (1 << 13) | (1 << 14);

const POLL_LIMIT: u32 = 1_000_000;

// If the alarm is more than ~75% of the 16-bit range in the future we
// defer enabling the compare interrupt until the next overflow. This avoids
// aliasing where the 16-bit compare value shadows an earlier period.
const COMPARE_THRESHOLD: u64 = 0xc000;

struct LptimTimeDriver {
    initialized: AtomicBool,
    period: AtomicU32,
    alarm: Mutex<Cell<u64>>,
    queue: Mutex<RefCell<Queue>>,
}

embassy_time_driver::time_driver_impl!(static DRIVER: LptimTimeDriver = LptimTimeDriver {
    initialized: AtomicBool::new(false),
    period: AtomicU32::new(0),
    alarm: Mutex::new(Cell::new(u64::MAX)),
    queue: Mutex::new(RefCell::new(Queue::new())),
});

fn lptim() -> pac::Lptimer0 {
    unsafe { pac::Lptimer0::steal() }
}

fn wait_isr(mask: u32) {
    for _ in 0..POLL_LIMIT {
        if lptim().isr().read().bits() & mask == mask {
            return;
        }
        core::hint::spin_loop();
    }
}

fn wait_csr(mask: u32) {
    for _ in 0..POLL_LIMIT {
        if lptim().csr().read().bits() & mask == mask {
            return;
        }
        core::hint::spin_loop();
    }
}

fn set_lptim0_clock_source() -> Result<(), rcc::Error> {
    // Vendor sequence: gate functional clock, wait for sync clear, then
    // program CR1. Mirrors `tremo_lptimer` and `rcc_set_lptimer0_clk_source`.
    let sync = || unsafe { pac::Rcc::steal() }.sr1().read().lptim0_clk_en_sync().bit_is_set();
    if sync() {
        critical_section::with(|_| {
            unsafe { pac::Rcc::steal() }
                .cgr1()
                .modify(|_, w| w.lptim0_clk_en().clear_bit());
        });
        for _ in 0..POLL_LIMIT {
            if !sync() {
                break;
            }
            core::hint::spin_loop();
        }
        if sync() {
            return Err(rcc::Error::Timeout(rcc::WaitTarget::ClockDomain));
        }
    }
    critical_section::with(|_| {
        let rcc = unsafe { pac::Rcc::steal() };
        rcc.cr1().modify(|_, w| unsafe {
            w.lptim0_ext_clk_sel().clear_bit();
            w.lptim0_clk_sel().bits(2) // 2 = XO32K per tremo_regs.h
        });
    });
    Ok(())
}

impl LptimTimeDriver {
    fn init(&'static self) {
        debug!("lptim0 init");
        assert!(TICK_HZ == TIMER_TICK_HZ, "embassy-asr: time tick rate must be 32768 Hz");

        // XO32K is in the always-on domain. Clear its AFEC power-down bits
        // (matches vendor `rcc_enable_oscillator(RCC_OSC_XO32K)`).
        afec::analog::REG_02.clear_bits(ANALOG_XO32K_POWER_DOWN);

        // Gate, reset, select XO32K, re-enable. This mirrors the SDK
        // `rcc_enable_peripheral_clk` / `rcc_rst_peripheral` sequence and the
        // vendor 92 µs async delay after reset release.
        let _ = rcc::disable_peripheral(RccPeripheral::Lptimer0);
        let _ = rcc::reset_peripheral(RccPeripheral::Lptimer0);
        let _ = set_lptim0_clock_source();
        let _ = rcc::enable_peripheral(RccPeripheral::Lptimer0);

        // Configure LPTIM0: continuous, prescaler /1, no preload/wave, max ARR
        wait_isr(ISR_CFGROK);
        // COUNTMODE = 0 (internal), PRESC = 0 (/1), PRELOAD = 0, WAVPOL = 0
        lptim().cfgr().modify(|r, w| unsafe {
            let mut bits = r.bits() & !0x800000u32; // COUNTMODE
            bits &= !CFGR_PRESC_MASK;
            bits &= !0x400000u32; // PRELOAD
            bits &= !0x200000u32; // WAVPOL
            w.bits(bits)
        });
        wait_isr(ISR_CFGROK);

        // Enable peripheral and wait for CROK
        lptim().cr().modify(|_, w| unsafe { w.bits(CR_ENABLE) });
        wait_isr(ISR_CROK);

        // ARR = max, wait for ARROK
        unsafe { lptim().arr().write_with_zero(|w| w.bits(ARR_MAX as u32)) };
        wait_isr(ISR_ARROK);

        // Ensure CMP is 0 and CMPOK
        unsafe { lptim().cmp().write_with_zero(|w| w.bits(0)) };
        wait_isr(ISR_CMPOK);

        // Clear any pending flags via ICR + CSR wait
        unsafe { lptim().icr().write_with_zero(|w| w.bits(ICR_ARRM | ICR_CMPM)) };
        wait_csr(CSR_ARRM | CSR_CMPM);

        // Enable overflow interrupt
        lptim().ier().modify(|r, w| unsafe { w.bits(r.bits() | IER_ARRM) });

        // Start continuous counting
        lptim().cr().modify(|r, w| unsafe { w.bits(r.bits() | CR_CNTSTRT) });
        wait_isr(ISR_CROK);

        self.period.store(0, Ordering::Release);
        self.initialized.store(true, Ordering::Release);

        Interrupt::LPTIMER0.unpend();
        Interrupt::LPTIMER0.set_priority(Priority::P2);
        unsafe { Interrupt::LPTIMER0.enable() };
    }

    fn now_inner(&self) -> u64 {
        // Must be called with interrupts masked (critical_section) to avoid
        // tearing between `period` and `cnt` across an overflow.
        let period = self.period.load(Ordering::Relaxed);
        let cnt = lptim().cnt().read().bits() as u16 as u64;
        // If ARRM is pending, the counter has wrapped but `period` has not yet
        // been incremented by the ISR. Account for it here.
        let pending_overflow = if lptim().isr().read().bits() & ISR_ARRM != 0 { 1 } else { 0 };
        ((period as u64 + pending_overflow as u64) << 16) | cnt
    }

    fn set_alarm(&self, cs: CriticalSection, timestamp: u64) -> bool {
        self.alarm.borrow(cs).set(timestamp);

        if timestamp == u64::MAX {
            // No alarm: disable compare interrupt
            lptim().ier().modify(|r, w| unsafe { w.bits(r.bits() & !IER_CMPM) });
            return true;
        }

        let now = self.now_inner();
        if timestamp <= now {
            lptim().ier().modify(|r, w| unsafe { w.bits(r.bits() & !IER_CMPM) });
            self.alarm.borrow(cs).set(u64::MAX);
            return false;
        }

        let cmp = (timestamp & 0xFFFF) as u16 as u32;
        unsafe { lptim().cmp().write_with_zero(|w| w.bits(cmp)) };
        wait_isr(ISR_CMPOK);

        // Only enable CMPM if the alarm is near enough to avoid aliasing.
        let diff = timestamp - now;
        if diff < COMPARE_THRESHOLD {
            lptim().ier().modify(|r, w| unsafe { w.bits(r.bits() | IER_CMPM) });
        } else {
            lptim().ier().modify(|r, w| unsafe { w.bits(r.bits() & !IER_CMPM) });
        }

        // Re-check after programming in case time has advanced.
        let now2 = self.now_inner();
        if timestamp <= now2 {
            lptim().ier().modify(|r, w| unsafe { w.bits(r.bits() & !IER_CMPM) });
            self.alarm.borrow(cs).set(u64::MAX);
            return false;
        }

        true
    }

    fn next_period(&self) {
        let period = self.period.load(Ordering::Relaxed) + 1;
        self.period.store(period, Ordering::Release);
        let t = (period as u64) << 16;

        critical_section::with(|cs| {
            let alarm = self.alarm.borrow(cs).get();
            if alarm != u64::MAX {
                let diff = alarm.saturating_sub(t);
                if diff < COMPARE_THRESHOLD {
                    // Alarm is now within range, enable compare
                    lptim().ier().modify(|r, w| unsafe { w.bits(r.bits() | IER_CMPM) });
                }
            }
        });
    }

    fn trigger_alarm(&self, cs: CriticalSection) {
        // Disable compare interrupt until next alarm is programmed
        lptim().ier().modify(|r, w| unsafe { w.bits(r.bits() & !IER_CMPM) });
        self.alarm.borrow(cs).set(u64::MAX);

        let mut next = self.queue.borrow(cs).borrow_mut().next_expiration(self.now_inner());
        while !self.set_alarm(cs, next) {
            next = self.queue.borrow(cs).borrow_mut().next_expiration(self.now_inner());
        }
    }

    fn on_interrupt(&self) {
        INTERRUPT_COUNT.fetch_add(1, Ordering::Relaxed);
        let isr = lptim().isr().read().bits();
        let ier = lptim().ier().read().bits();
        let pending = isr & ier & (ISR_ARRM | ISR_CMPM);
        if pending == 0 {
            return;
        }

        // Clear via ICR and wait for CSR ack where applicable
        unsafe { lptim().icr().write_with_zero(|w| w.bits(pending & (ICR_ARRM | ICR_CMPM))) };
        let csr_mask = (if pending & ISR_ARRM != 0 { CSR_ARRM } else { 0 })
            | (if pending & ISR_CMPM != 0 { CSR_CMPM } else { 0 });
        if csr_mask != 0 {
            wait_csr(csr_mask);
        }

        if pending & ISR_ARRM != 0 {
            self.next_period();
        }
        if pending & ISR_CMPM != 0 {
            critical_section::with(|cs| self.trigger_alarm(cs));
        }
    }
}

impl Driver for LptimTimeDriver {
    fn now(&self) -> u64 {
        if !self.initialized.load(Ordering::Acquire) {
            return 0;
        }
        // Mask interrupts to get atomic snapshot of period + cnt + pending ARRM
        critical_section::with(|_| self.now_inner())
    }

    fn schedule_wake(&self, at: u64, waker: &Waker) {
        critical_section::with(|cs| {
            let mut queue = self.queue.borrow(cs).borrow_mut();
            if queue.schedule_wake(at, waker) {
                let mut next = queue.next_expiration(self.now());
                while !self.set_alarm(cs, next) {
                    next = queue.next_expiration(self.now());
                }
            }
        });
    }
}

pub(crate) fn init() {
    DRIVER.init();
}

#[interrupt]
fn LPTIMER0() {
    DRIVER.on_interrupt();
}
