//! OpenHCI (OHCI) USB 1.1 host controller.
//!
//! One driver for every OHCI chip (ALi M5237 on PCG-C1MAH, QEMU pci-ohci, …).
//! Detected by PCI class 0C:03:10, not by vendor id.
//!
//! ALi/ULi M5237 quirk: never touch HcFmInterval — the chip hard-locks.

use crate::memory::paging::{phys_to_virt, KERNEL_OFFSET, PAGING, PTEFlags};
use crate::pci::class::{class, subclass};
use crate::pci::device::PciDevice;
use crate::pci::{self};
use crate::println;
use crate::sync::mutex::Mutex;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ptr::{addr_of_mut, read_volatile, write_volatile};
use core::sync::atomic::{AtomicU32, Ordering};

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
const HC_FMREMAINING: usize = 0x38;
const HC_FMNUMBER: usize = 0x3C;
const HC_PERIODICSTART: usize = 0x40;
const HC_LSTHRESH: usize = 0x44;
const HC_RHDESCA: usize = 0x48;
const HC_RHSTATUS: usize = 0x50;
const HC_RHPORTSTATUS: usize = 0x54;

// HcControl
const CTRL_CBSR: u32 = 3 << 0;
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
const CMD_OCR: u32 = 1 << 3;

// HcInterruptStatus / Enable
const INTR_WDH: u32 = 1 << 1;
const INTR_RHSC: u32 = 1 << 6;
const INTR_OC: u32 = 1 << 30;
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
// OHCI CC=0xF means "Not Accessed". 0xE is reserved for HCD use and
// must not be used as the hardware-owned initial TD condition code.
const TD_CC_NOT_ACCESSED: u32 = 15;
const TD_DP_SETUP: u32 = 0 << 19;
const TD_DP_OUT: u32 = 1 << 19;
const TD_DP_IN: u32 = 2 << 19;
const TD_T_DATA0: u32 = 2 << 24;
const TD_T_DATA1: u32 = 3 << 24;
const TD_DI_NONE: u32 = 7 << 21;
const TD_R: u32 = 1 << 18;

const ED_SKIP: u32 = 1 << 14;
const ED_LOWSPEED: u32 = 1 << 13;

fn ed_flags(addr: u8, ep: u8, mps: u16, ls: bool) -> u32 {
    (addr as u32)
        | ((ep as u32) << 7)
        | ((mps as u32) << 16)
        | if ls { ED_LOWSPEED } else { 0 }
}

fn td_toggle(data1: bool) -> u32 {
    if data1 { TD_T_DATA1 } else { TD_T_DATA0 }
}

/// DATA0/1 per (addr, ep). Bulk/interrupt only — control toggle is fixed by spec.
static TOGGLE: Mutex<[u8; 128]> = Mutex::new([0; 128]);

fn toggle_of(addr: u8, ep: u8) -> bool {
    let i = ((addr as usize) & 0x7F);
    TOGGLE.lock()[i] & (1 << (ep & 7)) != 0
}

fn set_toggle(addr: u8, ep: u8, data1: bool) {
    let i = ((addr as usize) & 0x7F);
    let bit = 1u8 << (ep & 7);
    let mut g = TOGGLE.lock();
    if data1 {
        g[i] |= bit;
    } else {
        g[i] &= !bit;
    }
}

static NEXT_ADDR: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(1);

pub fn alloc_addr() -> u8 {
    NEXT_ADDR.fetch_add(1, core::sync::atomic::Ordering::Relaxed).max(1)
}

static EP0_MPS: Mutex<[u16; 128]> = Mutex::new([8; 128]);
static EP0_LS: Mutex<[u8; 128]> = Mutex::new([0; 128]);

fn ep0_mps(addr: u8) -> u16 {
    EP0_MPS.lock()[addr as usize].max(8)
}
fn ep0_ls(addr: u8) -> bool {
    EP0_LS.lock()[addr as usize] != 0
}
fn set_ep0(addr: u8, mps: u16, ls: bool) {
    EP0_MPS.lock()[addr as usize] = mps.max(8);
    EP0_LS.lock()[addr as usize] = if ls { 1 } else { 0 };
}

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

/// HCCA must be 256-aligned; ED/TD 16-aligned. Heap Box does not guarantee that.
#[repr(C, align(4096))]
struct DmaPage {
    hcca: Hcca,
    ed: Ed,
    dummy: Td,
    setup_td: Td,
    data_td: Td,
    status_td: Td,
    setup: [u8; 8],
    data: [u8; 512],
}

const fn empty_td() -> Td {
    Td { flags: 0, cbp: 0, next_td: 0, be: 0 }
}
const fn empty_ed() -> Ed {
    Ed { flags: 0, tail_td: 0, head_td: 0, next_ed: 0 }
}
const fn empty_hcca() -> Hcca {
    Hcca {
        int_table: [0; 32],
        frame_number: 0,
        pad: 0,
        done_head: 0,
        reserved: [0; 120],
    }
}

static DMA: Mutex<[DmaPage; 2]> = Mutex::new([
    DmaPage {
        hcca: empty_hcca(),
        ed: empty_ed(),
        dummy: empty_td(),
        setup_td: empty_td(),
        data_td: empty_td(),
        status_td: empty_td(),
        setup: [0; 8],
        data: [0; 512],
    },
    DmaPage {
        hcca: empty_hcca(),
        ed: empty_ed(),
        dummy: empty_td(),
        setup_td: empty_td(),
        data_td: empty_td(),
        status_td: empty_td(),
        setup: [0; 8],
        data: [0; 512],
    },
]);
static DMA_SLOT: Mutex<usize> = Mutex::new(0);

/// Persistent bulk endpoint state. OHCI expects bulk EDs to remain scheduled;
/// individual TDs are queued behind the endpoint's dummy tail.
struct BulkEp {
    mmio: usize,
    addr: u8,
    ep: u8,
    mps: u16,
    ed: *mut Ed,
    ed_phys: u32,
    dummy: *mut Td,
    dummy_phys: u32,
    next_data1: bool,
}

unsafe impl Send for BulkEp {}
unsafe impl Sync for BulkEp {}

static BULK_EPS: Mutex<Vec<BulkEp>> = Mutex::new(Vec::new());

const FI_DEFAULT: u32 = 0x2EDF;
const FSMPS_DEFAULT: u32 = 0x2778;
const PERIODIC_START: u32 = 0x2A2F;

fn spin_ms(ms: u32) {
    for _ in 0..ms {
        for _ in 0..30_000 {
            core::hint::spin_loop();
        }
    }
}

#[derive(Copy, Clone)]
pub struct Ohci {
    pub(crate) mmio: usize,
    irq: u8,
    vendor: u16,
    device: u16,
    skip_fminterval: bool,
    nports: u8,
    port_ls: u16,
    hcca: *mut Hcca,
    hcca_phys: u32,
}

unsafe impl Send for Ohci {}
unsafe impl Sync for Ohci {}

pub(crate) static CONTROLLERS: Mutex<Vec<Ohci>> = Mutex::new(Vec::new());
static MMIO_SLOT: Mutex<u32> = Mutex::new(0);

// USB transfers in this HCD are already completion-polled. On the C1M
// generation IRQ9 is a heavily shared legacy PCI INTx line (CardBus, USB,
// audio, etc.), while this kernel currently has one IDT handler per vector.
// Root-hub state is therefore polled from idle. Keep the divider small: at
// 200 Hz PIT this is roughly 20 root-hub polls/sec when idle runs every tick.
static ROOT_HUB_POLL_DIV: AtomicU32 = AtomicU32::new(0);
// One bit per root-hub port (16 bits reserved per controller). A failed
// enumeration is latched until the device is physically disconnected, so a
// fast PIT cannot hammer a broken/slow real controller with repeated resets.
static ENUM_FAIL_MASK: AtomicU32 = AtomicU32::new(0);

impl Ohci {
    fn r32(&self, off: usize) -> u32 {
        unsafe { read_volatile((self.mmio + off) as *const u32) }
    }

    fn w32(&self, off: usize, val: u32) {
        unsafe { write_volatile((self.mmio + off) as *mut u32, val) }
    }

    /// OHCI frame number advances once per USB frame (~1 ms) while HCFS is
    /// OPERATIONAL. Use it for USB protocol timing instead of CPU-speed based
    /// spin loops; this stays correct on both QEMU and the slow C1M Crusoe and
    /// does not depend on the PIT frequency.
    #[inline]
    fn frame_number(&self) -> u16 {
        (self.r32(HC_FMNUMBER) & 0xffff) as u16
    }

    #[inline]
    fn frames_since(&self, start: u16) -> u16 {
        self.frame_number().wrapping_sub(start)
    }

    fn wait_frames(&self, frames: u16) {
        if frames == 0 {
            return;
        }
        let start = self.frame_number();
        while self.frames_since(start) < frames {
            core::hint::spin_loop();
        }
    }

    fn port_status(&self, port: u8) -> u32 {
        self.r32(HC_RHPORTSTATUS + (port as usize) * 4)
    }

    fn write_port(&self, port: u8, val: u32) {
        self.w32(HC_RHPORTSTATUS + (port as usize) * 4, val);
    }

    #[inline]
    fn enum_fail_bit(&self, port: u8) -> u32 {
        let slot = ((self.mmio as u32).wrapping_sub(OHCI_MMIO_BASE) / OHCI_MMIO_STRIDE).min(1);
        1u32 << (slot * 16 + (port as u32 & 0x0f))
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

        // OHCI uses MMIO + DMA; it does not need PCI I/O-space decoding.
        // Keep firmware's other command bits and enable only Memory + BusMaster.
        let cmd_before = dev.read_u16(0x04);
        dev.write_u16(0x04, cmd_before | 0x0006);
        let mmio = Self::map_bar(bar, dev.bars.iter().find(|b| b.is_memory()).map(|b| b.size()).unwrap_or(0x1000))?;

        let skip_fminterval = dev.vendor_id == ALI_VENDOR && dev.device_id == ALI_M5237;

        let hcca = {
            let mut slot = DMA_SLOT.lock();
            let i = *slot;
            *slot = i + 1;
            drop(slot);
            let g = DMA.lock();
            let i = i.min(1);
            &g[i].hcca as *const Hcca as *mut Hcca
        };
        let hcca_phys = virt_to_phys(hcca as *const u8);

        Ok(Self {
            mmio,
            irq: dev.interrupt_line,
            vendor: dev.vendor_id,
            device: dev.device_id,
            skip_fminterval,
            nports: 0,
            port_ls: 0,
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

        let mut ctrl = self.r32(HC_CONTROL);

        // OHCI ownership handoff.  IR means interrupts are routed to SMM/BIOS;
        // software must request ownership through HcCommandStatus.OCR and wait
        // for firmware to clear IR.  Clearing IR directly in HcControl is not
        // the OHCI handoff protocol and is particularly unsafe on old ALi parts.
        if ctrl & CTRL_IR != 0 {
            self.w32(HC_INTDIS, 0xFFFF_FFFF);
            self.w32(HC_INTEN, INTR_OC);
            self.w32(HC_CMDSTATUS, CMD_OCR);

            let mut released = false;
            for _ in 0..500 {
                ctrl = self.r32(HC_CONTROL);
                if ctrl & CTRL_IR == 0 {
                    released = true;
                    break;
                }
                spin_ms(10);
            }
            self.w32(HC_INTDIS, INTR_OC | INTR_MIE);
            if !released {
                // FreeBSD/NetBSD-style recovery: old firmware can ignore OCR.
                // Continue with a controller reset instead of leaving IR/SMM
                // routing active when the kernel later executes STI.
                println!("[ohci] SMM did not release HC; forcing reset path");
            }
        }

        // Keep all HC interrupt sources disabled during reset and initial port
        // enumeration.  This prevents a stale RHSC from becoming the very first
        // interrupt immediately after the kernel executes STI.
        self.w32(HC_INTDIS, 0xFFFF_FFFF);
        self.w32(HC_INTSTATUS, 0xFFFF_FFFF);

        ctrl = self.r32(HC_CONTROL);
        let rwc = ctrl & CTRL_RWC;
        let old_hcfs = ctrl & CTRL_HCFS_MASK;

        // ALi/ULi M5237 is special in two independent ways:
        //   1) touching HcFmInterval can wedge the whole machine;
        //   2) rev03 parts are historically fragile around HCR itself.
        //
        // More importantly for this Sony, HCR would force us to reconstruct
        // frame-scheduler state that we are forbidden to read back safely.
        // Preserve firmware's FmInterval/FSMPS/PeriodicStart/LSThreshold and do
        // only a USB bus reset. This keeps the BIOS-programmed non-periodic
        // budget intact while still returning devices to the default state.
        if self.skip_fminterval {
            self.w32(HC_CONTROL, rwc | CTRL_HCFS_RESET);
            let _ = self.r32(HC_CONTROL); // flush RESET write
            spin_ms(50);

            unsafe {
                core::ptr::write_bytes(self.hcca as *mut u8, 0, core::mem::size_of::<Hcca>());
            }
            self.w32(HC_CONTROLHEADED, 0);
            self.w32(HC_CONTROLCURRENTED, 0);
            self.w32(HC_BULKHEADED, 0);
            self.w32(HC_BULKCURRENTED, 0);
            self.w32(HC_HCCA, self.hcca_phys);
            core::sync::atomic::fence(Ordering::SeqCst);
            self.w32(HC_CONTROL, CTRL_HCFS_OPERATIONAL | CTRL_CBSR | rwc);
            let _ = self.r32(HC_CONTROL);
        } else {
            if old_hcfs != CTRL_HCFS_RESET {
                self.w32(HC_CONTROL, rwc | CTRL_HCFS_RESET);
                let _ = self.r32(HC_CONTROL);
                spin_ms(50);
            }

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
            let fi = self.r32(HC_FMINTERVAL);
            let mut interval = fi & 0x3FFF;
            let mut fsmps = (fi >> 16) & 0x7FFF;
            if interval < 0x1000 {
                interval = FI_DEFAULT;
            }
            if fsmps < 0x1000 {
                fsmps = FSMPS_DEFAULT;
            }
            self.w32(HC_FMINTERVAL, interval | (fsmps << 16) | (1 << 31));

            self.w32(HC_PERIODICSTART, PERIODIC_START);
            self.w32(HC_LSTHRESH, 0x0628);

            self.w32(HC_HCCA, self.hcca_phys);
            self.w32(HC_CONTROLHEADED, 0);
            self.w32(HC_CONTROLCURRENTED, 0);
            self.w32(HC_BULKHEADED, 0);
            self.w32(HC_BULKCURRENTED, 0);

            self.w32(HC_CONTROL, CTRL_HCFS_OPERATIONAL | CTRL_CBSR | rwc);
            spin_ms(10);
        }

        // Global power on root-hub ports. HcRhDescriptorA.POTPGT tells the
        // HCD how long a newly powered port needs before it may be accessed;
        // the field is in 2 ms units. Real southbridges can need much longer
        // than QEMU, so do not hard-code one short delay for every controller.
        self.w32(HC_RHSTATUS, RHS_LPSC);
        let _ = self.r32(HC_RHSTATUS); // flush posted PCI write
        let desca = self.r32(HC_RHDESCA);
        let potpgt_ms = ((desca >> 24) & 0xFF) * 2;
        spin_ms(potpgt_ms.max(20));

        self.nports = (desca & 0xFF) as u8;
        if self.nports == 0 || self.nports > 15 {
            self.nports = 2;
        }

        // Power + ack leftover port-change bits.
        for p in 0..self.nports {
            self.write_port(p, PS_PPS);
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
        let reset_start = self.frame_number();
        let mut reset_done = false;
        while self.frames_since(reset_start) < 100 {
            if self.port_status(port) & PS_PRSC != 0 {
                reset_done = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !reset_done {
            return Err("OHCI: port reset timeout");
        }

        self.write_port(port, PS_PRSC | PS_CSC | PS_PESC);
        self.write_port(port, PS_PES);
        // USB requires recovery time after reset before address-0 traffic.
        self.wait_frames(10);

        let s = self.port_status(port);
        Ok(s & PS_PES != 0 && s & PS_CCS != 0)
    }

    pub fn port_low_speed(&self, port: u8) -> bool {
        self.port_status(port) & PS_LSDA != 0
    }

    /// Control transfer on ep0 of `addr` (0 during default state).
    pub fn control(
        &self,
        addr: u8,
        setup: &[u8; 8],
        data: &mut [u8],
        in_dir: bool,
    ) -> Result<usize, &'static str> {
        self.control_ex(addr, setup, data, in_dir, ep0_mps(addr), ep0_ls(addr))
    }

    pub fn control_ex(
        &self,
        addr: u8,
        setup: &[u8; 8],
        data: &mut [u8],
        in_dir: bool,
        mps: u16,
        ls: bool,
    ) -> Result<usize, &'static str> {
        if data.len() > 256 {
            return Err("OHCI: control data > 256");
        }
        // Use the controller's permanently allocated, physically contiguous
        // DMA page for EP0. This avoids handing real OHCI hardware a mixture of
        // heap objects and an idle-task stack buffer. QEMU tolerates that model,
        // but old PCI OHCI parts are much happier when the complete control
        // transaction lives in one stable DMA page.
        let dma = unsafe { &mut *(self.hcca as *mut DmaPage) };

        dma.setup.copy_from_slice(setup);
        if data.is_empty() {
            dma.data[..1].fill(0);
        } else if in_dir {
            dma.data[..data.len()].fill(0);
        } else {
            dma.data[..data.len()].copy_from_slice(data);
        }

        let ed_phys = virt_to_phys((&dma.ed as *const Ed).cast::<u8>());
        let dummy_phys = virt_to_phys((&dma.dummy as *const Td).cast::<u8>());
        let setup_td_phys = virt_to_phys((&dma.setup_td as *const Td).cast::<u8>());
        let data_td_phys = virt_to_phys((&dma.data_td as *const Td).cast::<u8>());
        let status_td_phys = virt_to_phys((&dma.status_td as *const Td).cast::<u8>());
        let setup_phys = virt_to_phys(dma.setup.as_ptr());
        let data_phys = virt_to_phys(dma.data.as_ptr());

        dma.dummy = Td {
            flags: TD_CC_NOT_ACCESSED << TD_CC_SHIFT,
            cbp: 0,
            next_td: 0,
            be: 0,
        };
        dma.status_td = Td {
            flags: (TD_CC_NOT_ACCESSED << TD_CC_SHIFT)
                | if in_dir { TD_DP_OUT } else { TD_DP_IN }
                | TD_T_DATA1
                | TD_DI_NONE,
            cbp: 0,
            next_td: dummy_phys,
            be: 0,
        };
        dma.data_td = Td {
            flags: (TD_CC_NOT_ACCESSED << TD_CC_SHIFT)
                | if in_dir { TD_DP_IN } else { TD_DP_OUT }
                | TD_T_DATA1
                | TD_DI_NONE
                | TD_R,
            cbp: if data.is_empty() { 0 } else { data_phys },
            next_td: status_td_phys,
            be: if data.is_empty() {
                0
            } else {
                data_phys + data.len() as u32 - 1
            },
        };
        dma.setup_td = Td {
            flags: (TD_CC_NOT_ACCESSED << TD_CC_SHIFT) | TD_DP_SETUP | TD_T_DATA0 | TD_DI_NONE,
            cbp: setup_phys,
            next_td: if data.is_empty() { status_td_phys } else { data_td_phys },
            be: setup_phys + 7,
        };
        dma.ed = Ed {
            flags: ed_flags(addr, 0, mps.max(8), ls),
            tail_td: dummy_phys,
            head_td: setup_td_phys,
            next_ed: 0,
        };

        // Make descriptor writes globally visible before the controller can DMA.
        core::sync::atomic::fence(Ordering::SeqCst);

        // Stop non-periodic schedules while replacing ControlHeadED. Linux
        // starts with CBSR=3 and enables CLE/BLE on demand; mirror that model
        // instead of leaving an empty bulk schedule enabled on ALi M5237.
        let ctrl = self.r32(HC_CONTROL);
        self.w32(HC_CONTROL, ctrl & !(CTRL_CLE | CTRL_BLE));
        let _ = self.r32(HC_CONTROL); // flush posted PCI write
        self.w32(HC_CONTROLHEADED, ed_phys);
        self.w32(HC_CONTROLCURRENTED, 0);
        core::sync::atomic::fence(Ordering::SeqCst);
        self.w32(
            HC_CONTROL,
            (ctrl & !(CTRL_HCFS_MASK | CTRL_CLE | CTRL_BLE | CTRL_CBSR))
                | CTRL_HCFS_OPERATIONAL
                | CTRL_CBSR
                | CTRL_CLE,
        );
        let _ = self.r32(HC_CONTROL); // flush posted writes
        self.w32(HC_CMDSTATUS, CMD_CLF);
        let _ = self.r32(HC_CMDSTATUS); // force posted write onto PCI

        let mut ok = false;
        let wait_start = self.frame_number();
        while self.frames_since(wait_start) < 500 {
            let setup_cc = dma.setup_td.cc();
            let status_cc = dma.status_td.cc();
            if setup_cc != TD_CC_NOT_ACCESSED && status_cc != TD_CC_NOT_ACCESSED {
                ok = true;
                break;
            }
            core::hint::spin_loop();
        }

        // Stop HC access to the descriptor before reusing this DMA page. Also
        // switch CLE off: after a failed transfer in particular, leaving an
        // empty control schedule enabled with CLF latched can poison the next
        // hotplug enumeration on old M5237 hardware.
        dma.ed.flags |= ED_SKIP;
        core::sync::atomic::fence(Ordering::SeqCst);
        let ctrl_after = self.r32(HC_CONTROL);
        self.w32(HC_CONTROL, ctrl_after & !CTRL_CLE);
        let _ = self.r32(HC_CONTROL);
        self.w32(HC_CONTROLHEADED, 0);
        self.w32(HC_CONTROLCURRENTED, 0);
        let _ = self.r32(HC_CONTROLHEADED);

        if !ok {
            println!(
                "[ohci] ctrl timeout mmio={:#x} ctrl={:08x} cmd={:08x} int={:08x} done={:08x} head={:08x} cur={:08x} frame={} hcca_frame={} rem={:08x} periodic={:08x} setup_cc={} data_cc={} stat_cc={}",
                self.mmio,
                self.r32(HC_CONTROL),
                self.r32(HC_CMDSTATUS),
                self.r32(HC_INTSTATUS),
                self.r32(HC_DONEHEAD),
                self.r32(HC_CONTROLHEADED),
                self.r32(HC_CONTROLCURRENTED),
                self.r32(HC_FMNUMBER) & 0xffff,
                unsafe { read_volatile(&(*self.hcca).frame_number) as u32 },
                self.r32(HC_FMREMAINING),
                self.r32(HC_PERIODICSTART),
                dma.setup_td.cc(),
                dma.data_td.cc(),
                dma.status_td.cc(),
            );
            return Err("OHCI: control timeout");
        }
        if dma.setup_td.cc() != 0 {
            println!("[ohci] SETUP cc={}", dma.setup_td.cc());
            return Err("OHCI: SETUP failed");
        }
        if !data.is_empty() {
            let data_cc = dma.data_td.cc();
            if data_cc != 0 && data_cc != 9 {
                println!("[ohci] DATA cc={}", data_cc);
                return Err("OHCI: DATA failed");
            }
        }
        if dma.status_td.cc() != 0 && dma.status_td.cc() != 9 {
            println!("[ohci] STATUS cc={}", dma.status_td.cc());
            return Err("OHCI: STATUS failed");
        }

        if in_dir && !data.is_empty() {
            data.copy_from_slice(&dma.data[..data.len()]);
        }
        Ok(data.len())
    }

    /// Bulk IN or OUT on `ep` (endpoint number, no direction bit).
    ///
    /// This follows the Linux OHCI queue model: one persistent ED per endpoint,
    /// a permanent dummy tail TD, and real TDs appended before that dummy. Bulk
    /// TDs use TD_T_TOGGLE so the controller carries DATA0/DATA1 in ED.HeadP.C.
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
        let n = data.len();
        // The transfer buffer itself is DMA-visible. Linux's OHCI HCD maps the
        // URB buffer and puts that DMA address directly in TD.hwCBP/hwBE.
        // Do the same here instead of bouncing every bulk transfer through one
        // shared DMA[0].data buffer. The latter is especially wrong with two
        // OHCI controllers and also adds an unnecessary copy on every IN.
        let data_phys = virt_to_phys(data.as_mut_ptr());
        let last = data_phys + (n as u32) - 1;

        let (ed, ed_phys, dummy, dummy_phys, first) = {
            let mut eps = BULK_EPS.lock();
            let pos = eps.iter().position(|e| e.mmio == self.mmio && e.addr == addr && e.ep == ep);
            let idx = match pos {
                Some(i) => i,
                None => {
                    let dummy = Box::leak(Box::new(Td {
                        flags: TD_CC_NOT_ACCESSED << TD_CC_SHIFT,
                        cbp: 0,
                        next_td: 0,
                        be: 0,
                    }));
                    let dummy_phys = virt_to_phys(dummy as *mut Td as *const u8);
                    let ed = Box::leak(Box::new(Ed {
                        flags: (addr as u32) | ((ep as u32) << 7) | ((mps as u32) << 16),
                        tail_td: dummy_phys,
                        head_td: dummy_phys,
                        next_ed: 0,
                    }));
                    let ed_phys = virt_to_phys(ed as *mut Ed as *const u8);
                    // BulkHeadED is the head of a linked ED list, not a single
                    // endpoint register. Keep all persistent bulk EDs chained.
                    if let Some(prev) = eps.iter().rev().find(|e| e.mmio == self.mmio) {
                        unsafe { (*prev.ed).next_ed = ed_phys; }
                        // println!("[ohci] bulk link ed=0x{:08x} -> ed=0x{:08x}", prev.ed_phys, ed_phys);
                    }
                    eps.push(BulkEp {
                        mmio: self.mmio,
                        addr,
                        ep,
                        mps,
                        ed,
                        ed_phys,
                        dummy,
                        dummy_phys,
                        next_data1: false,
                    });
                    eps.len() - 1
                }
            };
            let state = &mut eps[idx];
            let dummy = state.dummy;
            let dummy_phys = state.dummy_phys;

            // OHCI's queue is advanced by converting the current dummy TD into
            // the real TD, then installing a fresh dummy tail. This is the same
            // queue invariant used by Linux's OHCI HCD.
            let new_dummy = Box::leak(Box::new(Td {
                flags: TD_CC_NOT_ACCESSED << TD_CC_SHIFT,
                cbp: 0,
                next_td: 0,
                be: 0,
            }));
            let new_dummy_phys = virt_to_phys(new_dummy as *mut Td as *const u8);
            unsafe {
                (*dummy).flags = (TD_CC_NOT_ACCESSED << TD_CC_SHIFT)
                    | if in_dir { TD_DP_IN } else { TD_DP_OUT }
                    | TD_DI_NONE
                    | TD_R;
                (*dummy).cbp = data_phys;
                (*dummy).next_td = new_dummy_phys;
                (*dummy).be = last;
                (*state.ed).tail_td = new_dummy_phys;
            }
            state.dummy = new_dummy;
            state.dummy_phys = new_dummy_phys;
            (state.ed, state.ed_phys, dummy, dummy_phys, dummy_phys)
        };

        // println!(
        //     "[ohci] bulk begin addr={} ep={} {} len={} mps={} ed=0x{:08x} td=0x{:08x} buf=0x{:08x} head=0x{:08x}",
        //     addr, ep, if in_dir { "IN" } else { "OUT" }, n, mps, ed_phys, first, data_phys,
        //     unsafe { read_volatile(&(*ed).head_td) }
        // );

        // Schedule the persistent ED if it is not already the bulk-list head.
        let head = self.r32(HC_BULKHEADED) & !0xF;
        if head == 0 {
            self.w32(HC_BULKHEADED, ed_phys);
            self.w32(HC_BULKCURRENTED, 0);
            // println!("[ohci] bulk list head=0x{:08x}", ed_phys);
        } else {
            // println!("[ohci] bulk list existing head=0x{:08x}, ED=0x{:08x} already linked", head, ed_phys);
        }
        let ctrl = self.r32(HC_CONTROL);
        self.w32(
            HC_CONTROL,
            (ctrl & !CTRL_HCFS_MASK) | CTRL_HCFS_OPERATIONAL | CTRL_CLE | CTRL_BLE,
        );
        // Linux kicks the bulk list after the TD has been linked and memory is visible.
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.w32(HC_CMDSTATUS, CMD_BLF);
        // println!(
        //     "[ohci] bulk kick head=0x{:08x} current=0x{:08x} ctrl={:08x} cmd={:08x} ed_head=0x{:08x} ed_tail=0x{:08x}",
        //     self.r32(HC_BULKHEADED), self.r32(HC_BULKCURRENTED), self.r32(HC_CONTROL),
        //     self.r32(HC_CMDSTATUS), unsafe { read_volatile(&(*ed).head_td) },
        //     unsafe { read_volatile(&(*ed).tail_td) }
        // );

        let mut ok = false;
        let wait_start = self.frame_number();
        while self.frames_since(wait_start) < 2000 {
            if unsafe { read_volatile(&(*dummy).flags) } >> TD_CC_SHIFT != TD_CC_NOT_ACCESSED {
                ok = true;
                break;
            }
            core::hint::spin_loop();
        }

        if !ok {
            println!(
                "[ohci] bulk TIMEOUT cc={} ctrl={:08x} cmd={:08x} int={:08x} done={:08x} head=0x{:08x} current=0x{:08x} ed_head=0x{:08x} ed_tail=0x{:08x}",
                unsafe { read_volatile(&(*dummy).flags) } >> TD_CC_SHIFT,
                self.r32(HC_CONTROL), self.r32(HC_CMDSTATUS), self.r32(HC_INTSTATUS),
                self.r32(HC_DONEHEAD), self.r32(HC_BULKHEADED), self.r32(HC_BULKCURRENTED),
                unsafe { read_volatile(&(*ed).head_td) }, unsafe { read_volatile(&(*ed).tail_td) }
            );
            return Err("OHCI: bulk timeout");
        }

        let cc = unsafe { read_volatile(&(*dummy).flags) } >> TD_CC_SHIFT;
        // println!(
        //     // "[ohci] bulk done cc={} td_flags={:08x} cbp=0x{:08x} be=0x{:08x} ed_head=0x{:08x} ed_tail=0x{:08x}",
        //     cc,
        //     unsafe { read_volatile(&(*dummy).flags) },
        //     unsafe { read_volatile(&(*dummy).cbp) },
        //     unsafe { read_volatile(&(*dummy).be) },
        //     unsafe { read_volatile(&(*ed).head_td) },
        //     unsafe { read_volatile(&(*ed).tail_td) }
        // );
        if cc != 0 && cc != 9 {
            println!("[ohci] bulk cc={}", cc);
            return Err("OHCI: bulk failed");
        }

        // println!("[ohci] bulk return n={}", n);
        let _ = dummy_phys;
        Ok(n)
    }

    /// Interrupt IN/OUT on the periodic list (HID boot reports).
    pub fn interrupt(
        &self,
        addr: u8,
        ep: u8,
        mps: u16,
        data: &mut [u8],
        in_dir: bool,
        ls: bool,
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
        let data1 = toggle_of(addr, ep);
        let td = Box::leak(Box::new(Td {
            flags: (TD_CC_NOT_ACCESSED << TD_CC_SHIFT)
                | if in_dir { TD_DP_IN } else { TD_DP_OUT }
                | td_toggle(data1)
                | TD_DI_NONE
                | TD_R,
            cbp: data_phys,
            next_td: dummy_phys,
            be: last,
        }));
        let td_phys = virt_to_phys(td as *mut Td as *const u8);
        let ed = Box::leak(Box::new(Ed {
            // Interrupt endpoints may legitimately have very small packets
            // (CBI command-completion is two bytes). Use the descriptor's
            // wMaxPacketSize; the >=8 rule applies to EP0, not periodic EDs.
            flags: ed_flags(addr, ep, mps.max(1), ls),
            tail_td: dummy_phys,
            head_td: td_phys,
            next_ed: 0,
        }));
        let ed_phys = virt_to_phys(ed as *mut Ed as *const u8);
        unsafe {
            let hcca = &mut *self.hcca;
            for slot in hcca.int_table.iter_mut() {
                *slot = ed_phys;
            }
        }
        let ctrl = self.r32(HC_CONTROL);
        self.w32(HC_CONTROL, ctrl | CTRL_PLE | CTRL_HCFS_OPERATIONAL);

        let mut ok = false;
        let wait_start = self.frame_number();
        while self.frames_since(wait_start) < 200 {
            if td.cc() != TD_CC_NOT_ACCESSED {
                ok = true;
                break;
            }
            core::hint::spin_loop();
        }
        ed.flags |= ED_SKIP;
        unsafe {
            (*self.hcca).int_table = [0; 32];
        }
        if !ok {
            return Err("OHCI: interrupt timeout");
        }
        if td.cc() != 0 && td.cc() != 9 {
            return Err("OHCI: interrupt failed");
        }
        set_toggle(addr, ep, (ed.head_td & 2) != 0);
        let _ = dummy;
        Ok(data.len())
    }

    /// Drop host-controller endpoint state belonging to a physically removed
    /// USB address. Device/class/VFS state is removed by usb::device first;
    /// this removes the stale OHCI bulk EDs that otherwise remain linked in
    /// HcBulkHeadED across a disconnect/reconnect cycle.
    pub(crate) fn disconnect_address(&self, addr: u8) {
        let ctrl = self.r32(HC_CONTROL);
        self.w32(HC_CONTROL, ctrl & !(CTRL_CLE | CTRL_BLE));
        let _ = self.r32(HC_CONTROL); // flush before editing the ED chain
        self.w32(HC_CONTROLHEADED, 0);
        self.w32(HC_CONTROLCURRENTED, 0);

        let first_phys = {
            let mut eps = BULK_EPS.lock();
            eps.retain(|e| !(e.mmio == self.mmio && e.addr == addr));

            let indices: Vec<usize> = eps
                .iter()
                .enumerate()
                .filter_map(|(i, e)| if e.mmio == self.mmio { Some(i) } else { None })
                .collect();

            for (n, &idx) in indices.iter().enumerate() {
                let next = indices
                    .get(n + 1)
                    .map(|&next_idx| eps[next_idx].ed_phys)
                    .unwrap_or(0);
                unsafe {
                    (*eps[idx].ed).next_ed = next;
                }
            }

            indices.first().map(|&i| eps[i].ed_phys).unwrap_or(0)
        };

        self.w32(HC_BULKHEADED, first_phys);
        self.w32(HC_BULKCURRENTED, 0);
        core::sync::atomic::fence(Ordering::SeqCst);

        // Restore the bulk schedule only if this controller still owns at
        // least one bulk endpoint. Otherwise leave BLE off with an empty head.
        if first_phys != 0 {
            self.w32(HC_CONTROL, ctrl & !CTRL_CLE);
        } else {
            self.w32(HC_CONTROL, ctrl & !(CTRL_CLE | CTRL_BLE));
        }
        let _ = self.r32(HC_CONTROL);

        TOGGLE.lock()[addr as usize & 0x7f] = 0;
        EP0_MPS.lock()[addr as usize & 0x7f] = 8;
        EP0_LS.lock()[addr as usize & 0x7f] = 0;
    }

    /// Reset every connected port and bind a class driver.
    pub fn enumerate_ports(&self) {
        for p in 0..self.nports {
            self.enumerate_port(p);
        }
    }

    /// Enumerate one root-hub port. This is intentionally outside IRQ context.
    pub fn enumerate_port(&self, port: u8) {
        match self.reset_port(port) {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                println!("[ohci] port {} reset: {}", port + 1, e);
                return;
            }
        }
        let port_status = self.port_status(port);
        let ls = port_status & PS_LSDA != 0;
        match self.address_and_bind_on_port(port, ls) {
            Ok(addr) => {
                ENUM_FAIL_MASK.fetch_and(!self.enum_fail_bit(port), Ordering::Relaxed);
                println!("[ohci] port {} addr={}{}", port + 1, addr, if ls { " LS" } else { " FS" });
            }
            Err(e) => {
                ENUM_FAIL_MASK.fetch_or(self.enum_fail_bit(port), Ordering::Relaxed);
                println!("[ohci] port {}: {} (retry latched until disconnect)", port + 1, e);
            }
        }
    }

    /// Clear all root-hub change latches and the top-level RHSC bit.
    /// Keep this separate from enumeration so init can drain stale BIOS/reset
    /// events before the PIC line is ever unmasked.
    fn ack_root_hub_changes(&self) {
        for p in 0..self.nports {
            let status = self.port_status(p);
            let changes = status & 0xFFFF_0000;
            if changes != 0 {
                self.write_port(p, changes);
            }
        }
        self.w32(HC_INTSTATUS, INTR_RHSC);
        let _ = self.r32(HC_INTSTATUS); // flush posted write
    }

    /// Scan root-hub connection state after an RHSC interrupt.
    pub fn poll_root_hub(&self) {
        for p in 0..self.nports {
            // Do not infer power state from PPS here. Old ALi root hubs can
            // report surprising PPS values while disconnected, and repeatedly
            // writing SetPortPower on every poll prevents normal edge handling.
            // Initial root-hub power is established once in start().
            let status = self.port_status(p);
            let connected = status & PS_CCS != 0;
            let known = crate::drivers::usb::device::find(self.mmio, p).is_some();

            let fail_bit = self.enum_fail_bit(p);
            let failed = ENUM_FAIL_MASK.load(Ordering::Relaxed) & fail_bit != 0;

            if connected && !known && !failed {
                println!(
                    "[ohci] hotplug: mmio={:#x} port {} connected status={:08x}",
                    self.mmio,
                    p + 1,
                    status,
                );
                // Debounce against the controller's own 1 kHz USB frame clock,
                // not an uncalibrated CPU spin. Leave IRQs enabled while the
                // connector settles; only reset/enumeration is atomic against
                // the PIT scheduler on this old M5237.
                let debounce_start = self.frame_number();
                while self.frames_since(debounce_start) < 100 {
                    if self.port_status(p) & PS_CCS == 0 {
                        break;
                    }
                    core::hint::spin_loop();
                }
                if self.port_status(p) & PS_CCS != 0 {
                    interrupt_sync::without_interrupts(|| self.enumerate_port(p));
                }
            } else if !connected {
                // Physical disconnect arms a fresh enumeration attempt. A
                // failed enumeration never enters DEVICES, so log that case too
                // instead of making subsequent unplug look like "no event".
                ENUM_FAIL_MASK.fetch_and(!fail_bit, Ordering::Relaxed);
                if known {
                    println!("[ohci] hotplug: port {} disconnected", p + 1);
                    crate::drivers::usb::device::disconnect_port(self, p);
                } else if failed {
                    println!("[ohci] hotplug: port {} disconnected; retry rearmed", p + 1);
                }
            }

            // Port change bits are W1C and are the real source behind RHSC on
            // controllers that implement RHSC as a level interrupt.
            let changes = status & 0xFFFF_0000;
            if changes != 0 {
                self.write_port(p, changes);
            }
        }

        // Keep the top-level RHSC latch drained.  Hardware RHSC interrupts are
        // deliberately disabled for now: legacy IRQ9 is shared on the target
        // C1M platform and the kernel does not yet have a shared IRQ dispatcher.
        self.w32(HC_INTSTATUS, INTR_RHSC);
        let _ = self.r32(HC_INTSTATUS); // flush posted write
    }

    /// Default-state device on the wire → SET_ADDRESS → class bind.
    pub fn address_and_bind(&self, ls: bool) -> Result<u8, &'static str> {
        self.address_and_bind_on_port(0xFF, ls)
    }

    fn address_and_bind_on_port(&self, port: u8, ls: bool) -> Result<u8, &'static str> {
        let mps8 = 8;
        let mut hdr = [0u8; 8];
        self.control_ex(0, &[0x80, 6, 0x00, 0x01, 0, 0, 8, 0], &mut hdr, true, mps8, ls)?;
        let mps = if hdr[7] == 8 || hdr[7] == 16 || hdr[7] == 32 || hdr[7] == 64 {
            hdr[7] as u16
        } else {
            8
        };
        let mut desc = [0u8; 18];
        self.control_ex(0, &[0x80, 6, 0x00, 0x01, 0, 0, 18, 0], &mut desc, true, mps, ls)?;
        let vid = u16::from_le_bytes([desc[8], desc[9]]);
        let pid = u16::from_le_bytes([desc[10], desc[11]]);
        println!("[ohci] device {:04x}:{:04x} mps0={}", vid, pid, mps);

        let addr = alloc_addr();
        set_ep0(0, mps, ls);
        set_ep0(addr, mps, ls);
        let mut empty: [u8; 0] = [];
        self.control_ex(0, &[0x00, 5, addr, 0, 0, 0, 0, 0], &mut empty, false, mps, ls)?;
        self.wait_frames(2);
        crate::drivers::usb::device::bind_with_port(self, port, addr, &desc);
        Ok(addr)
    }
}

pub fn poll_hotplug() {
    // Keep polling cheap on the Crusoe but responsive at the current 200 Hz PIT.
    // Unlike the old /100 divider, /10 does not turn hotplug into multi-second
    // latency once other runnable tasks reduce the amount of idle CPU time.
    if ROOT_HUB_POLL_DIV.fetch_add(1, Ordering::Relaxed) % 10 != 0 {
        return;
    }
    let controllers: Vec<Ohci> = {
        let controllers = CONTROLLERS.lock();
        controllers.iter().copied().collect()
    };
    for hc in controllers {
        hc.poll_root_hub();
    }
}

pub fn init_all() {
    // Do NOT install a USB-specific IRQ9 gate here.  On the target Sony C1M
    // generation PCI INTx/IRQ9 is shared with CardBus and several other devices,
    // and PCMCIA may already own vector 41 by the time USB is initialized.
    // This HCD uses polling for transfer completion anyway, so root-hub hotplug
    // is polled from idle until the kernel gains a shared IRQ dispatcher.
    let devices = pci::enumerate();
    let mut n = 0u32;
    for dev in devices.iter() {
        if dev.class_code != class::SERIAL_BUS
            || dev.subclass != subclass::USB
            || dev.prog_if != PROG_IF_OHCI
        {
            continue;
        }
        // Do not skip an M5237 merely because firmware left Memory/BusMaster
        // decode disabled.  On the PCG-C1MHP (same C1MW platform as C1MAH),
        // 00:0f.0 starts as command 0x0010 and Linux deliberately enables it;
        // that controller owns the built-in Sony Memory Stick reader.  Probe()
        // enables only PCI Memory + BusMaster before touching OHCI registers.
        match Ohci::probe(dev) {
            Ok(mut hc) => {
                if let Err(e) = hc.start() {
                    println!("[ohci] start failed: {}", e);
                    continue;
                }
                let mmio = hc.mmio;

                // The HC interrupt block is still fully disabled here. Initial
                // port resets/enumeration themselves generate RHSC changes, so
                // doing this before enabling RHSC avoids a pending IRQ at STI.
                CONTROLLERS.lock().push(hc);
                let controller = {
                    let controllers = CONTROLLERS.lock();
                    controllers.iter().find(|hc| hc.mmio == mmio).copied()
                };
                if let Some(hc) = controller {
                    hc.enumerate_ports();
                    hc.ack_root_hub_changes();

                    // Never assert PCI INTx from OHCI in polling mode.  In
                    // particular, do not overwrite/unmask shared IRQ9 after the
                    // ToPIC/CardBus driver has already installed its handler.
                    hc.w32(HC_INTDIS, 0xFFFF_FFFF);
                    hc.w32(HC_INTSTATUS, 0xFFFF_FFFF);
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
