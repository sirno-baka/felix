//! USB Mass Storage — Bulk-Only Transport + SCSI (flash drives).

use super::desc::{self, Interface};
use super::ohci::{self, Ohci};
use crate::disk::interface::BlockDevice;
use crate::println;
use crate::sync::mutex::Mutex;
use alloc::vec;
use alloc::vec::Vec;

const CBW_SIG: u32 = 0x4342_5355;
const CSW_SIG: u32 = 0x5342_5355;

static DEVICES: Mutex<Vec<UsbMsc>> = Mutex::new(Vec::new());

#[derive(Clone)]
pub struct UsbMsc {
    mmio: usize,
    addr: u8,
    ep_out: u8,
    ep_in: u8,
    mps: u16,
    pub block_size: u32,
    pub blocks: u32,
}

impl UsbMsc {
    fn bot(&self, hc: &Ohci, cdb: &[u8], data: &mut [u8], din: bool) -> Result<(), &'static str> {
        let mut cbw = [0u8; 31];
        cbw[0..4].copy_from_slice(&CBW_SIG.to_le_bytes());
        cbw[4..8].copy_from_slice(&1u32.to_le_bytes());
        cbw[8..12].copy_from_slice(&(data.len() as u32).to_le_bytes());
        cbw[12] = if din { 0x80 } else { 0x00 };
        cbw[13] = 0;
        cbw[14] = cdb.len() as u8;
        cbw[15..15 + cdb.len()].copy_from_slice(cdb);

        hc.bulk(self.addr, self.ep_out, self.mps, &mut cbw, false)?;
        if !data.is_empty() {
            hc.bulk(self.addr, if din { self.ep_in } else { self.ep_out }, self.mps, data, din)?;
        }
        let mut csw = [0u8; 13];
        hc.bulk(self.addr, self.ep_in, self.mps, &mut csw, true)?;
        let sig = u32::from_le_bytes([csw[0], csw[1], csw[2], csw[3]]);
        if sig != CSW_SIG {
            return Err("MSC: bad CSW");
        }
        if csw[12] != 0 {
            return Err("MSC: SCSI status");
        }
        Ok(())
    }

    pub fn inquiry(&self, hc: &Ohci) -> Result<[u8; 36], &'static str> {
        let cdb = [0x12u8, 0, 0, 0, 36, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut buf = [0u8; 36];
        self.bot(hc, &cdb[..6], &mut buf, true)?;
        Ok(buf)
    }

    pub fn read_capacity(&self, hc: &Ohci) -> Result<(u32, u32), &'static str> {
        let cdb = [0x25u8, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut buf = [0u8; 8];
        self.bot(hc, &cdb, &mut buf, true)?;
        let last = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let size = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        Ok((last.saturating_add(1), size))
    }
}

impl BlockDevice for UsbMsc {
    fn read_sectors(&self, numsects: u8, lba: u32, buf: u32) -> Result<(), u8> {
        let n = numsects as usize;
        let bytes = n * self.block_size as usize;
        let dest = buf as *mut u8;
        let mut tmp = vec![0u8; bytes.max(512)];
        let mut cdb = [0u8; 10];
        cdb[0] = 0x28;
        cdb[2..6].copy_from_slice(&lba.to_be_bytes());
        cdb[8] = numsects;
        {
            let g = ohci::CONTROLLERS.lock();
            let hc = g.iter().find(|h| h.mmio == self.mmio).ok_or(1u8)?;
            self.bot(hc, &cdb, &mut tmp[..bytes], true).map_err(|_| 2u8)?;
        }
        unsafe {
            core::ptr::copy_nonoverlapping(tmp.as_ptr(), dest, bytes);
        }
        Ok(())
    }

    fn write_sectors(&mut self, numsects: u8, lba: u32, buf: u32) -> Result<(), u8> {
        let n = numsects as usize;
        let bytes = n * self.block_size as usize;
        let mut tmp = vec![0u8; bytes.max(512)];
        unsafe {
            core::ptr::copy_nonoverlapping(buf as *const u8, tmp.as_mut_ptr(), bytes);
        }
        let mut cdb = [0u8; 10];
        cdb[0] = 0x2A;
        cdb[2..6].copy_from_slice(&lba.to_be_bytes());
        cdb[8] = numsects;
        let g = ohci::CONTROLLERS.lock();
        let hc = g.iter().find(|h| h.mmio == self.mmio).ok_or(1u8)?;
        self.bot(hc, &cdb, &mut tmp[..bytes], false).map_err(|_| 2u8)
    }

    fn sector_size(&self) -> u32 {
        self.block_size.max(512)
    }
}

pub fn bind(hc: &Ohci, addr: u8, iface: &Interface) {
    if iface.subclass != desc::MSC_SCSI || iface.protocol != desc::MSC_BBB {
        println!(
            "[usb-msc] unsupported subclass=0x{:02x} proto=0x{:02x}",
            iface.subclass, iface.protocol
        );
        return;
    }
    let mut ep_in = None;
    let mut ep_out = None;
    let mut mps = 64u16;
    for ep in iface.endpoints.iter() {
        if !ep.is_bulk() {
            continue;
        }
        mps = ep.max_packet.max(8);
        if ep.dir_in() {
            ep_in = Some(ep.number());
        } else {
            ep_out = Some(ep.number());
        }
    }
    let (Some(ep_in), Some(ep_out)) = (ep_in, ep_out) else {
        println!("[usb-msc] need bulk IN+OUT");
        return;
    };

    let mut empty: [u8; 0] = [];
    let _ = hc.control(addr, &desc::setup(0x21, 0xFF, 0, iface.number as u16, 0), &mut empty, false); // Bulk-Only reset

    let mut dev = UsbMsc {
        mmio: hc.mmio,
        addr,
        ep_out,
        ep_in,
        mps,
        block_size: 512,
        blocks: 0,
    };
    match dev.inquiry(hc) {
        Ok(inq) => {
            let vendor = core::str::from_utf8(&inq[8..16]).unwrap_or("?");
            let product = core::str::from_utf8(&inq[16..32]).unwrap_or("?");
            println!("[usb-msc] {} {}", vendor.trim(), product.trim());
        }
        Err(e) => println!("[usb-msc] INQUIRY: {}", e),
    }
    match dev.read_capacity(hc) {
        Ok((blocks, size)) => {
            dev.blocks = blocks;
            dev.block_size = if size == 0 { 512 } else { size };
            println!("[usb-msc] {} × {} byte sectors", blocks, dev.block_size);
        }
        Err(e) => println!("[usb-msc] READ_CAPACITY: {}", e),
    }
    DEVICES.lock().push(dev);
}

pub fn devices() -> usize {
    DEVICES.lock().len()
}

/// Clone of the first enumerated flash drive, if any.
pub fn first() -> Option<UsbMsc> {
    DEVICES.lock().first().cloned()
}

pub fn get(index: usize) -> Option<UsbMsc> {
    DEVICES.lock().get(index).cloned()
}
