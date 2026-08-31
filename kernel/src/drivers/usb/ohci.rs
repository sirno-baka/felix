//! OpenHCI (OHCI) USB 1.1 host controller.
//!
//! One driver for every OHCI chip (ALi M5237 on PCG-C1MAH, QEMU pci-ohci, …).
//! Detected by PCI class 0C:03:10, not by vendor id.
//!
//! ALi/ULi M5237 quirk: never touch HcFmInterval — the chip hard-locks.

use crate::memory::paging::{KERNEL_OFFSET, PAGING, PTEFlags};
use crate::pci::class::{class, subclass};
use crate::pci::device::PciDevice;
use crate::pci::{self};
use crate::println;
use crate::sync::mutex::Mutex;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ptr::{addr_of_mut, read_volatile, write_volatile};

// ——— PCI ———
const PROG_IF_OHCI: u8 = 0x10;
const ALI_VENDOR: u16 = 0x10B9;
const ALI_M5237: u16 = 0x5237;

// ——— MMIO window (NIC already uses 0xE000_0000) ———
const OHCI_MMIO_BASE: u32 = 0xE010_0000;
const OHCI_MMIO_STRIDE: u32 = 0x2000;

// ——— Operational registers ———
const HC_REVISION: usize = 0x00;
const HC_CONTROL: usize = 0x04;
const HC_CMDSTATUS: usize = 0x08;
const HC_INTSTATUS: usize = 0x0C;
const HC_INTEN: usize = 0x10;
const HC_INTDIS: usize = 0x14;
const HC_HCCA: usize = 0x18;
const HC_CONTROLHEADED: usize = 0x20;
const HC_CONTROLCURRENTED: usize = 0x24;
const HC_BULKHEADED: usize = 0x28;
const HC_BULKCURRENTED: usize = 0x2C;
const HC_DONEHEAD: usize = 0x30;
const HC_FMINTERVAL: usize = 0x34;
const HC_PERIODICSTART: usize = 0x40;
const HC_RHDESCA: usize = 0x48;
const HC_RHSTATUS: usize = 0x50;
const HC_RHPORTSTATUS: usize = 0x54;

// HcControl
const CTRL_PLE: u32 = 1 << 2;
const CTRL_CLE: u32 = 1 << 4;
const CTRL_BLE: u32 = 1 << 5;
const CTRL_HCFS_RESET: u32 = 0 << 6;
const CTRL_HCFS_OPERATIONAL: u32 = 2 << 6;
const CTRL_HCFS_MASK: u32 = 3 << 6;
const CTRL_IR: u32 = 1 << 8;
const CTRL_RWC: u32 = 1 << 9;

// HcCommandStatus
const CMD_HCR: u32 = 1 << 0;
const CMD_CLF: u32 = 1 << 1;
const CMD_BLF: u32 = 1 << 2;

// HcInterruptStatus / Enable
const INTR_WDH: u32 = 1 << 1;
const INTR_RHSC: u32 = 1 << 6;
const INTR_MIE: u32 = 1 << 31;

// Root hub port
const PS_CCS: u32 = 1 << 0;
const PS_PES: u32 = 1 << 1;
const PS_PRS: u32 = 1 << 4;
const PS_PPS: u32 = 1 << 8;
const PS_LSDA: u32 = 1 << 9;
const PS_CSC: u32 = 1 << 16;
const PS_PESC: u32 = 1 << 17;
const PS_PRSC: u32 = 1 << 20;

// HcRhStatus
const RHS_LPSC: u32 = 1 << 16;

// TD condition / direction
const TD_CC_SHIFT: u32 = 28;
const TD_CC_NOT_ACCESSED: u32 = 14;
const TD_DP_SETUP: u32 = 0 << 19;
const TD_DP_OUT: u32 = 1 << 19;
const TD_DP_IN: u32 = 2 << 19;
const TD_T_DATA0: u32 = 2 << 24;
const TD_T_DATA1: u32 = 3 << 24;
const TD_DI_NONE: u32 = 7 << 21;
const TD_R: u32 = 1 << 18;

const ED_SKIP: u32 = 1 << 14;

#[repr(C, align(256))]
struct Hcca {
    int_table: [u32; 32],
    frame_number: u16,
    pad: u16,
    done_head: u32,
    reserved: [u8; 120],
}

/// Endpoint Descriptor — 16-byte aligned, physical pointer for the HC.
#[repr(C, align(16))]
struct Ed {
    flags: u32,
    tail_td: u32,
    head_td: u32,
    next_ed: u32,
}

/// Transfer Descriptor.
#[repr(C, align(16))]
struct Td {
    flags: u32,
    cbp: u32,
    next_td: u32,
    be: u32,
}

impl Td {
    fn cc(&self) -> u32 {
        (unsafe { read_volatile(&self.flags) }) >> TD_CC_SHIFT
    }
}

fn virt_to_phys(ptr: *const u8) -> u32 {
    let v = ptr as u32;
    if v >= KERNEL_OFFSET {
        v - KERNEL_OFFSET
    } else {
        v
    }
}

fn spin_ms(ms: u32) {
    for _ in 0..ms {
        for _ in 0..30_000 {
            core::hint::spin_loop();
        }
    }
}

pub struct Ohci {
    pub(crate) mmio: usize,
    irq: u8,
    vendor: u16,
    device: u16,
    skip_fminterval: bool,
    nports: u8,
    hcca: *mut Hcca,
    hcca_phys: u32,
}

unsafe impl Send for Ohci {}
unsafe impl Sync for Ohci {}

pub(crate) static CONTROLLERS: Mutex<Vec<Ohci>> = Mutex::new(Vec::new());
static MMIO_SLOT: Mutex<u32> = Mutex::new(0);

impl Ohci {
    fn r32(&self, off: usize) -> u32 {
        unsafe { read_volatile((self.mmio + off) as *const u32) }
    }

    fn w32(&self, off: usize, val: u32) {
        unsafe { write_volatile((self.mmio + off) as *mut u32, val) }
    }

    fn port_status(&self, port: u8) -> u32 {
        self.r32(HC_RHPORTSTATUS + (port as usize) * 4)
    }

    fn write_port(&self, port: u8, val: u32) {
        self.w32(HC_RHPORTSTATUS + (port as usize) * 4, val);
    }

    fn map_bar(phys: u32, size: u32) -> Result<usize, &'static str> {
        let mut slot = MMIO_SLOT.lock();
        let virt = OHCI_MMIO_BASE + *slot * OHCI_MMIO_STRIDE;
        *slot += 1;
        drop(slot);

        let flags = PTEFlags::new().present().writable();
        let mut paging = unsafe { PAGING.lock() };
        paging.map_physical_range(phys, size.max(0x1000), virt, flags)?;
        Ok(virt as usize)
    }

    pub fn probe(dev: &PciDevice) -> Result<Self, &'static str> {
        let bar = dev
            .bars
            .iter()
            .find(|b| b.is_memory())
            .and_then(|b| b.address())
            .ok_or("OHCI: no MMIO BAR")?;

        dev.enable_bus_mastering();
        let mmio = Self::map_bar(bar, dev.bars.iter().find(|b| b.is_memory()).map(|b| b.size()).unwrap_or(0x1000))?;

        let skip_fminterval = dev.vendor_id == ALI_VENDOR && dev.device_id == ALI_M5237;

        let hcca = Box::leak(Box::new(Hcca {
            int_table: [0; 32],
            frame_number: 0,
            pad: 0,
            done_head: 0,
            reserved: [0; 120],
        }));
        let hcca_phys = virt_to_phys(hcca as *mut Hcca as *const u8);

        Ok(Self {
            mmio,
            irq: dev.interrupt_line,
            vendor: dev.vendor_id,
            device: dev.device_id,
            skip_fminterval,
            nports: 0,
            hcca,
            hcca_phys,
        })
    }

    /// Reset HC, take it from SMM, go operational, power ports.
    pub fn start(&mut self) -> Result<(), &'static str> {
        let rev = self.r32(HC_REVISION) & 0xFF;
        println!(
            "[ohci] {:04x}:{:04x} mmio=0x{:08x} irq={} rev=0x{:02x}{}",
            self.vendor,
            self.device,
            self.mmio,
            self.irq,
            rev,
            if self.skip_fminterval {
                " (ALi M5237, skip FmInterval)"
            } else {
                ""
            }
        );

        // Drop SMM ownership (IR) if the BIOS left the controller in IRQ routing mode.
        let mut ctrl = self.r32(HC_CONTROL);
        if ctrl & CTRL_IR != 0 {
            self.w32(HC_CONTROL, ctrl | CTRL_RWC);
            spin_ms(10);
            ctrl = self.r32(HC_CONTROL);
            self.w32(HC_CONTROL, ctrl & !CTRL_IR);
        }

        self.w32(HC_INTDIS, 0x8000_003F);
        self.w32(HC_INTSTATUS, 0x8000_003F);

        self.w32(HC_CMDSTATUS, CMD_HCR);
        for _ in 0..1000 {
            if self.r32(HC_CMDSTATUS) & CMD_HCR == 0 {
                break;
            }
            spin_ms(1);
        }
        if self.r32(HC_CMDSTATUS) & CMD_HCR != 0 {
            return Err("OHCI: HCR stuck");
        }

        if !self.skip_fminterval {
            // Default interval 0x2EDF, FSMPS in the upper half; periodic start ~90%.
            let fi = self.r32(HC_FMINTERVAL);
            self.w32(HC_FMINTERVAL, fi);
            self.w32(HC_PERIODICSTART, 0x2A2F);
        }

        self.w32(HC_HCCA, self.hcca_phys);
        self.w32(HC_CONTROLHEADED, 0);
        self.w32(HC_CONTROLCURRENTED, 0);
        self.w32(HC_BULKHEADED, 0);
        self.w32(HC_BULKCURRENTED, 0);

        // USBOPERATIONAL + control + bulk lists.
        self.w32(
            HC_CONTROL,
            CTRL_HCFS_OPERATIONAL | CTRL_CLE | CTRL_BLE | CTRL_RWC,
        );
        spin_ms(10);

        // Global power on root-hub ports.
        self.w32(HC_RHSTATUS, RHS_LPSC);
        spin_ms(20);

        let desca = self.r32(HC_RHDESCA);
        self.nports = (desca & 0xFF) as u8;
        if self.nports == 0 || self.nports > 15 {
            self.nports = 2;
        }
        println!("[ohci] ports={}", self.nports);

        // Acknowledge leftover port-change bits.
        for p in 0..self.nports {
            let s = self.port_status(p);
            self.write_port(p, s & 0xFFFF_0000);
            if s & PS_CCS != 0 {
                println!(
                    "[ohci] port {} connected{}",
                    p + 1,
                    if s & PS_LSDA != 0 { " (LS)" } else { " (FS)" }
                );
            }
        }
        Ok(())
    }

    pub fn reset_port(&self, port: u8) -> Result<bool, &'static str> {
        if port >= self.nports {
            return Err("OHCI: bad port");
        }
        let s = self.port_status(port);
        if s & PS_CCS == 0 {
            return Ok(false);
        }

        self.write_port(port, PS_PRS);
        for _ in 0..200 {
            if self.port_status(port) & PS_PRSC != 0 {
                break;
            }
            spin_ms(1);
        }
        self.write_port(port, PS_PRSC | PS_CSC | PS_PESC);
        self.write_port(port, PS_PES);
        spin_ms(10);

        let s = self.port_status(port);
        Ok(s & PS_PES != 0 && s & PS_CCS != 0)
    }

    /// Control transfer on ep0 of `addr` (0 during default state).
    pub fn control(
        &self,
        addr: u8,
        setup: &[u8; 8],
        data: &mut [u8],
        in_dir: bool,
    ) -> Result<usize, &'static str> {
        let dummy = Box::leak(Box::new(Td {
            flags: TD_CC_NOT_ACCESSED << TD_CC_SHIFT,
            cbp: 0,
            next_td: 0,
            be: 0,
        }));
        let dummy_phys = virt_to_phys(dummy as *mut Td as *const u8);

        let setup_buf = Box::leak(Box::new(*setup));
        let setup_phys = virt_to_phys(setup_buf.as_ptr());

        let data_phys = if !data.is_empty() {
            virt_to_phys(data.as_mut_ptr())
        } else {
            0
        };

        let setup_td = Box::leak(Box::new(Td {
            flags: (TD_CC_NOT_ACCESSED << TD_CC_SHIFT) | TD_DP_SETUP | TD_T_DATA0 | TD_DI_NONE,
            cbp: setup_phys,
            next_td: 0,
            be: setup_phys + 7,
        }));

        let status_td = Box::leak(Box::new(Td {
            flags: (TD_CC_NOT_ACCESSED << TD_CC_SHIFT)
                | if in_dir { TD_DP_OUT } else { TD_DP_IN }
                | TD_T_DATA1
                | TD_DI_NONE,
            cbp: 0,
            next_td: dummy_phys,
            be: 0,
        }));
        let status_phys = virt_to_phys(status_td as *mut Td as *const u8);

        let first_phys;
        if data.is_empty() {
            setup_td.next_td = status_phys;
            first_phys = virt_to_phys(setup_td as *mut Td as *const u8);
        } else {
            let last = data_phys + (data.len() as u32) - 1;
            let data_td = Box::leak(Box::new(Td {
                flags: (TD_CC_NOT_ACCESSED << TD_CC_SHIFT)
                    | if in_dir { TD_DP_IN } else { TD_DP_OUT }
                    | TD_T_DATA1
                    | TD_DI_NONE
                    | TD_R,
                cbp: data_phys,
                next_td: status_phys,
                be: last,
            }));
            setup_td.next_td = virt_to_phys(data_td as *mut Td as *const u8);
            first_phys = virt_to_phys(setup_td as *mut Td as *const u8);
            let _ = data_td;
        }

        let mps = 8u32;
        let ed = Box::leak(Box::new(Ed {
            flags: (addr as u32) | (mps << 16),
            tail_td: dummy_phys,
            head_td: first_phys,
            next_ed: 0,
        }));
        let ed_phys = virt_to_phys(ed as *mut Ed as *const u8);

        self.w32(HC_CONTROLHEADED, ed_phys);
        self.w32(HC_CONTROLCURRENTED, 0);
        let ctrl = self.r32(HC_CONTROL);
        self.w32(HC_CONTROL, (ctrl & !CTRL_HCFS_MASK) | CTRL_HCFS_OPERATIONAL | CTRL_CLE);
        self.w32(HC_CMDSTATUS, CMD_CLF);

        let mut ok = false;
        for _ in 0..500 {
            if setup_td.cc() != TD_CC_NOT_ACCESSED && status_td.cc() != TD_CC_NOT_ACCESSED {
                ok = true;
                break;
            }
            spin_ms(1);
        }

        // Unlink ED so the HC stops walking it.
        ed.flags |= ED_SKIP;
        self.w32(HC_CONTROLHEADED, 0);
        self.w32(HC_CONTROLCURRENTED, 0);

        if !ok {
            return Err("OHCI: control timeout");
        }
        if setup_td.cc() != 0 {
            println!("[ohci] SETUP cc={}", setup_td.cc());
            return Err("OHCI: SETUP failed");
        }
        if status_td.cc() != 0 && status_td.cc() != 9 {
            // 9 = data underrun / short packet — acceptable on IN
            println!("[ohci] STATUS cc={}", status_td.cc());
            return Err("OHCI: STATUS failed");
        }

        let _ = dummy;
        let _ = setup_buf;
        let _ = addr_of_mut!(*ed);
        Ok(data.len())
    }

    /// Bulk IN or OUT on `ep` (endpoint number, no direction bit).
    pub fn bulk(
        &self,
        addr: u8,
        ep: u8,
        mps: u16,
        data: &mut [u8],
        in_dir: bool,
    ) -> Result<usize, &'static str> {
        if data.is_empty() {
            return Ok(0);
        }
        let dummy = Box::leak(Box::new(Td {
            flags: TD_CC_NOT_ACCESSED << TD_CC_SHIFT,
            cbp: 0,
            next_td: 0,
            be: 0,
        }));
        let dummy_phys = virt_to_phys(dummy as *mut Td as *const u8);
        let data_phys = virt_to_phys(data.as_mut_ptr());
        let last = data_phys + (data.len() as u32) - 1;
        let td = Box::leak(Box::new(Td {
            flags: (TD_CC_NOT_ACCESSED << TD_CC_SHIFT)
                | if in_dir { TD_DP_IN } else { TD_DP_OUT }
                | TD_T_DATA0
                | TD_DI_NONE
                | TD_R,
            cbp: data_phys,
            next_td: dummy_phys,
            be: last,
        }));
        let td_phys = virt_to_phys(td as *mut Td as *const u8);
        let ed = Box::leak(Box::new(Ed {
            flags: (addr as u32) | ((ep as u32) << 7) | ((mps as u32) << 16),
            tail_td: dummy_phys,
            head_td: td_phys,
            next_ed: 0,
        }));
        let ed_phys = virt_to_phys(ed as *mut Ed as *const u8);

        self.w32(HC_BULKHEADED, ed_phys);
        self.w32(HC_BULKCURRENTED, 0);
        let ctrl = self.r32(HC_CONTROL);
        self.w32(
            HC_CONTROL,
            (ctrl & !CTRL_HCFS_MASK) | CTRL_HCFS_OPERATIONAL | CTRL_CLE | CTRL_BLE,
        );
        self.w32(HC_CMDSTATUS, CMD_BLF);

        let mut ok = false;
        for _ in 0..2000 {
            if td.cc() != TD_CC_NOT_ACCESSED {
                ok = true;
                break;
            }
            spin_ms(1);
        }
        ed.flags |= ED_SKIP;
        self.w32(HC_BULKHEADED, 0);
        self.w32(HC_BULKCURRENTED, 0);
        if !ok {
            return Err("OHCI: bulk timeout");
        }
        if td.cc() != 0 && td.cc() != 9 {
            println!("[ohci] bulk cc={}", td.cc());
            return Err("OHCI: bulk failed");
        }
        let _ = dummy;
        Ok(data.len())
    }

    /// Reset every connected port and bind a class driver.
    pub fn enumerate_ports(&self) {
        let mut next_addr = 1u8;
        for p in 0..self.nports {
            match self.reset_port(p) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    println!("[ohci] port {} reset: {}", p + 1, e);
                    continue;
                }
            }

            let setup_get = [0x80u8, 6, 0x00, 0x01, 0, 0, 18, 0];
            let mut desc = [0u8; 18];
            match self.control(0, &setup_get, &mut desc, true) {
                Ok(_) => {
                    let vid = u16::from_le_bytes([desc[8], desc[9]]);
                    let pid = u16::from_le_bytes([desc[10], desc[11]]);
                    println!(
                        "[ohci] port {} device desc len={} vid={:04x} pid={:04x}",
                        p + 1,
                        desc[0],
                        vid,
                        pid
                    );
                }
                Err(e) => {
                    println!("[ohci] port {} GET_DESCRIPTOR: {}", p + 1, e);
                    continue;
                }
            }

            let addr = next_addr;
            next_addr = next_addr.saturating_add(1);
            let setup_addr = [0x00u8, 5, addr, 0, 0, 0, 0, 0];
            let mut empty: [u8; 0] = [];
            match self.control(0, &setup_addr, &mut empty, false) {
                Ok(_) => {
                    println!("[ohci] port {} SET_ADDRESS {}", p + 1, addr);
                    spin_ms(2);
                    crate::drivers::usb::device::bind(self, addr, &desc);
                }
                Err(e) => println!("[ohci] port {} SET_ADDRESS: {}", p + 1, e),
            }
        }
    }
}

pub fn init_all() {
    let devices = pci::enumerate();
    let mut n = 0u32;
    for dev in devices.iter() {
        if dev.class_code != class::SERIAL_BUS
            || dev.subclass != subclass::USB
            || dev.prog_if != PROG_IF_OHCI
        {
            continue;
        }
        match Ohci::probe(dev) {
            Ok(mut hc) => {
                if let Err(e) = hc.start() {
                    println!("[ohci] start failed: {}", e);
                    continue;
                }
                CONTROLLERS.lock().push(hc);
                let idx = CONTROLLERS.lock().len() - 1;
                if let Some(hc) = CONTROLLERS.lock().get(idx) {
                    hc.enumerate_ports();
                }
                n += 1;
            }
            Err(e) => println!("[ohci] probe {:02x}:{:02x}.{}: {}", dev.bus, dev.device, dev.function, e),
        }
    }
    if n == 0 {
        println!("[ohci] no OHCI controller (class 0C:03:10)");
    } else {
        println!("[ohci] {} controller(s) ready", n);
    }
}
