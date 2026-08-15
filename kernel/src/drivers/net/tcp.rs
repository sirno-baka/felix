use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use smoltcp::wire::EthernetAddress;
use crate::drivers::net::i8255x::I8255x;
use crate::drivers::net::{RX_BUF_SIZE, TX_BUF_SIZE};
// ===================== smoltcp integration =====================

pub struct I8255xRxToken {
    data: [u8; RX_BUF_SIZE],
    len: usize,
}

pub struct I8255xTxToken {
    nic: *mut I8255x, // сырой указатель — аккуратно, только внутри токена
}

impl RxToken for I8255xRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.data[..self.len])
    }
}

impl TxToken for I8255xTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = [0u8; TX_BUF_SIZE];
        let result = f(&mut buf[..len]);

        unsafe {
            if !self.nic.is_null() {
                let _ = (*self.nic).send(&buf[..len]);
            }
        }
        result
    }
}

impl Device for I8255x {
    type RxToken<'a> = I8255xRxToken;
    type TxToken<'a> = I8255xTxToken;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let mut buf = [0u8; RX_BUF_SIZE];
        if let Some(len) = self.recv(&mut buf) {
            let rx = I8255xRxToken { data: buf, len };
            let tx = I8255xTxToken { nic: self as *mut _ };
            Some((rx, tx))
        } else {
            None
        }
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(I8255xTxToken {
            nic: self as *mut _,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.max_burst_size = Some(1);
        caps.medium = Medium::Ethernet;
        caps
    }
}

impl I8255x {
    pub fn ethernet_address(&self) -> EthernetAddress {
        EthernetAddress(self.mac)
    }
}