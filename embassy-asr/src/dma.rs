#![allow(dead_code)]
use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};
use crate::mode::{Async, Blocking, Mode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Uart0Tx = 30,
    Uart0Rx = 31,
    Uart1Tx = 28,
    Uart1Rx = 29,
    Uart2Tx = 26,
    Uart2Rx = 27,
    Uart3Tx = 24,
    Uart3Rx = 25,
    Spi0Tx = 20,
    Spi0Rx = 21,
    Spi1Tx = 22,
    Spi1Rx = 23,
    I2c0Tx = 10,
    I2c0Rx = 11,
    I2c1Tx = 8,
    I2c1Rx = 9,
    Dac = 14,
    Adc = 15,
}

#[derive(Debug, Clone, Copy)]
pub struct TransferOptions {
    pub burst_len: u8,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self { burst_len: 0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    ChannelBusy,
    LengthMismatch,
    TransferFailed,
}

pub struct Channel<'d, M: Mode> {
    _phantom: PhantomData<(&'d (), M)>,
}

impl<'d, M: Mode> Channel<'d, M> {
    pub fn reborrow(&mut self) -> Channel<'d, M> {
        Channel { _phantom: PhantomData }
    }
}

pub struct Transfer<'a, M: Mode> {
    _phantom: PhantomData<(&'a (), M)>,
}

impl<'a, M: Mode> Future for Transfer<'a, M> {
    type Output = Result<(), Error>;
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Ready(Ok(()))
    }
}

impl<'a, M: Mode> Transfer<'a, M> {
    pub fn is_done(&self) -> bool { true }
    pub fn wait(self) {}
    pub fn blocking_wait(self) -> Result<(), Error> { Ok(()) }
}

pub unsafe fn init() {}

impl<'d> Channel<'d, Async> {
    pub unsafe fn write<W: crate::dma::Word>(&mut self, _source: &[W], _dest: *mut W, _request: Request, _options: TransferOptions) -> Result<Transfer<'_, Async>, Error> {
        Ok(Transfer { _phantom: PhantomData })
    }
    pub unsafe fn read<W: crate::dma::Word>(&mut self, _src: *const W, _dest: &mut [W], _request: Request, _options: TransferOptions) -> Result<Transfer<'_, Async>, Error> {
        Ok(Transfer { _phantom: PhantomData })
    }
}

impl<'d> Channel<'d, Blocking> {
    pub unsafe fn write<W: crate::dma::Word>(&mut self, _source: &[W], _dest: *mut W, _request: Request, _options: TransferOptions) -> Result<(), Error> {
        Ok(())
    }
    pub unsafe fn read<W: crate::dma::Word>(&mut self, _src: *const W, _dest: &mut [W], _request: Request, _options: TransferOptions) -> Result<(), Error> {
        Ok(())
    }
    pub unsafe fn blocking_write<W: crate::dma::Word>(&mut self, _buf: &[W], _dest: *mut W, _request: Request, _options: TransferOptions) -> Result<(), Error> {
        Ok(())
    }
    pub unsafe fn blocking_read<W: crate::dma::Word>(&mut self, _src: *const W, _buf: &mut [W], _request: Request, _options: TransferOptions) -> Result<(), Error> {
        Ok(())
    }
    pub fn start_write(&mut self, _buf: &[u8], _dest: *mut u8, _request: Request, _options: TransferOptions) -> Result<Transfer<'_, Blocking>, Error> {
        Ok(Transfer { _phantom: PhantomData })
    }
    pub fn start_read(&mut self, _src: *mut u8, _buf: &mut [u8], _request: Request, _options: TransferOptions) -> Result<Transfer<'_, Blocking>, Error> {
        Ok(Transfer { _phantom: PhantomData })
    }
}

pub trait Word: Copy + Clone {}
impl Word for u8 {}
impl Word for u16 {}
impl Word for u32 {}

pub trait ChannelInstance {}
