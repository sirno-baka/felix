//! USB Mass Storage — Bulk-Only Transport + SCSI (flash drives).

use super::desc::{self, Interface};
use super::driver::UsbMatch;
use super::ohci::{self, Ohci};
use crate::disk::interface::BlockDevice;
use crate::println;
use crate::sync::mutex::Mutex;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

const CBW_SIG: u32 = 0x4342_5355;
const CSW_SIG: u32 = 0x5342_5355;

static DEVICES: Mutex<Vec<UsbMsc>> = Mutex::new(Vec::new());

pub static DRIVER: super::driver::UsbDriver = super::driver::UsbDriver {
    name: "usb-storage",
    matches: &[
        UsbMatch::InterfaceClass {
            class: desc::CLASS_MSC,
            subclass: None,
            protocol: Some(PROTO_BBB),
        },
        UsbMatch::InterfaceClass {
            class: desc::CLASS_MSC,
            subclass: None,
            protocol: Some(PROTO_CBI),
        },
        UsbMatch::InterfaceClass {
            class: desc::CLASS_MSC,
            subclass: None,
            protocol: Some(PROTO_CB),
        },
    ],
    probe: probe,
    disconnect: disconnect,
};

const PROTO_CBI: u8 = 0x00;
const PROTO_CB: u8 = 0x01;
const PROTO_BBB: u8 = 0x50;

#[derive(Clone)]
pub struct UsbMsc {
    hc: Ohci,
    mmio: usize,
    addr: u8,
    ep_out: u8,
    ep_in: u8,
    ep_intr: u8,
    iface: u8,
    proto: u8,
    mps: u16,
    pub block_size: u32,
    pub blocks: u32,
    mount_point: Option<String>,
}

impl UsbMsc {
    fn bot(&self, hc: &Ohci, cdb: &[u8], data: &mut [u8], din: bool) -> Result<(), &'static str> {
        if self.proto != PROTO_BBB {
            return self.cbi(hc, cdb, data, din);
        }
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
            let ep = if din { self.ep_in } else { self.ep_out };
            let n = hc.bulk(self.addr, ep, self.mps, data, din)?;
            if din && n != data.len() {
                return Err("MSC: short DATA IN");
            }
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

    fn cbi(&self, hc: &Ohci, cdb: &[u8], data: &mut [u8], din: bool) -> Result<(), &'static str> {
        let mut cmd = [0u8; 12];
        let n = cdb.len().min(12);
        cmd[..n].copy_from_slice(&cdb[..n]);
        hc.control(
            self.addr,
            &desc::setup(0x21, 0x00, 0, self.iface as u16, 12),
            &mut cmd,
            false,
        )?;
        if !data.is_empty() {
            hc.bulk(
                self.addr,
                if din { self.ep_in } else { self.ep_out },
                self.mps,
                data,
                din,
            )?;
        }
        Ok(())
    }

    pub fn inquiry(&self, hc: &Ohci) -> Result<[u8; 36], &'static str> {
        let cdb = [0x12u8, 0, 0, 0, 36, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
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
        if n == 0 || buf == 0 || self.block_size == 0 {
            return Err(1u8);
        }
        let bytes = n * self.block_size as usize;
        let dest = buf as *mut u8;
        let mut cdb = [0u8; 10];
        cdb[0] = 0x28;
        cdb[2..6].copy_from_slice(&lba.to_be_bytes());
        cdb[8] = numsects;
        unsafe {
            let data = core::slice::from_raw_parts_mut(dest, bytes);
            self.bot(&self.hc, &cdb, data, true).map_err(|_| 2u8)?;
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
        self.bot(&self.hc, &cdb, &mut tmp[..bytes], false).map_err(|_| 2u8)
    }

    fn sector_size(&self) -> u32 {
        self.block_size.max(512)
    }
}

pub fn bind(hc: &Ohci, addr: u8, iface: &Interface) {
    let proto = iface.protocol;
    if proto != PROTO_BBB && proto != PROTO_CBI && proto != PROTO_CB {
        println!("[usb-msc] unsupported subclass=0x{:02x} proto=0x{:02x}", iface.subclass, iface.protocol);
        return;
    }
    let mut ep_in = None;
    let mut ep_out = None;
    let mut ep_intr = 0u8;
    let mut mps = 64u16;
    for ep in iface.endpoints.iter() {
        if ep.is_interrupt() && ep.dir_in() {
            ep_intr = ep.number();
            continue;
        }
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

    if proto == PROTO_BBB {
        let mut empty: [u8; 0] = [];
        let _ = hc.control(addr, &desc::setup(0x21, 0xFF, 0, iface.number as u16, 0), &mut empty, false);
    }

    println!(
        "[usb-msc] addr={} proto={} bulk {}/{} irq={} mps={}",
        addr,
        if proto == PROTO_BBB { "BBB" } else { "CBI" },
        ep_out,
        ep_in,
        ep_intr,
        mps
    );

    let mut dev = UsbMsc {
        hc: *hc,
        mmio: hc.mmio,
        addr,
        ep_out,
        ep_in,
        ep_intr,
        iface: iface.number,
        proto,
        mps,
        block_size: 512,
        blocks: 0,
        mount_point: None,
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

fn probe(hc: &Ohci, addr: u8, _device: &crate::drivers::usb::device::UsbDevice, iface: Option<&Interface>) -> Result<(), &'static str> {
    let Some(iface) = iface else { return Err("MSC: no interface"); };
    bind(hc, addr, iface);

    let dev = DEVICES
        .lock()
        .iter()
        .position(|d| d.mmio == hc.mmio && d.addr == addr && d.iface == iface.number)
        .ok_or("MSC: probe failed")?;

    let mount_point = {
        let device = DEVICES.lock()[dev].clone();
        if device.blocks == 0 {
            println!("[usb-msc] no media, skip mount");
            None
        } else {
            use alloc::sync::Arc;
            use crate::filesystem::init::mount_removable;
            use crate::spin;

            let arc: Arc<spin::Mutex<dyn BlockDevice>> =
                Arc::new(spin::Mutex::new(device));
            mount_removable(arc, "usb")
        }
    };

    if let Some(path) = mount_point {
        DEVICES.lock()[dev].mount_point = Some(path);
    }
    Ok(())
}

fn disconnect(hc: &Ohci, addr: u8, iface: u8) {
    let mount_point = {
        let mut devices = DEVICES.lock();
        let pos = devices
            .iter()
            .position(|d| d.mmio == hc.mmio && d.addr == addr && d.iface == iface);
        pos.and_then(|i| devices.remove(i).mount_point)
    };

    if let Some(path) = mount_point {
        use crate::filesystem::vfs::VFS;
        VFS.get().unmount(&path);
    }
    println!("[usb-msc] disconnect addr={} iface={}", addr, iface);
}

pub fn devices() -> usize {
    DEVICES.lock().len()
}

pub fn first() -> Option<UsbMsc> {
    DEVICES.lock().first().cloned()
}

pub fn get(index: usize) -> Option<UsbMsc> {
    DEVICES.lock().get(index).cloned()
}
