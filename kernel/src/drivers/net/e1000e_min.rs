//! Minimal polling-only Intel e1000e-family driver.
//!
//! Supported profiles:
//! - 8086:10d3 Intel 82574L (QEMU `-device e1000e`)
//! - 8086:1502 Intel 82579LM (ThinkPad X220)
//!
//! The data path intentionally uses one RX queue, one TX queue, no interrupts,
//! no RSS, and no checksum/VLAN offload. 82574L uses legacy RX descriptors;
//! 82579LM uses the PCH2 extended 16-byte RX writeback format. The PHY
//! is left under firmware/hardware autonegotiation; Felix only owns MAC DMA.

use core::fmt;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::drivers::net::{RX_RING_SIZE, TX_BUF_SIZE, map_mmio};
use crate::memory::resources::{DmaBuffer as DmaAllocation, MmioMapping, dma_alloc_for};
use crate::pci;
use crate::println;
use crate::sync::mutex::Mutex;

const VENDOR_INTEL: u16 = 0x8086;
const DEVICE_82574L: u16 = 0x10d3;
const DEVICE_82579LM: u16 = 0x1502;

const TX_RING_SIZE: usize = 64;
const RX_BUF_SIZE: usize = 2048;

const REG_CTRL: usize = 0x0000;
const REG_STATUS: usize = 0x0008;
const REG_CTRL_EXT: usize = 0x0018;
const REG_FEXTNVM3: usize = 0x003c;
const REG_ICR: usize = 0x00c0;
const REG_IMC: usize = 0x00d8;
const REG_RCTL: usize = 0x0100;
const REG_TCTL: usize = 0x0400;
const REG_TIPG: usize = 0x0410;
const REG_RDBAL: usize = 0x2800;
const REG_RDBAH: usize = 0x2804;
const REG_RDLEN: usize = 0x2808;
const REG_RDH: usize = 0x2810;
const REG_RDT: usize = 0x2818;
const REG_RDTR: usize = 0x2820;
const REG_RXDCTL: usize = 0x2828;
const REG_RADV: usize = 0x282c;
const REG_TDBAL: usize = 0x3800;
const REG_TDBAH: usize = 0x3804;
const REG_TDLEN: usize = 0x3808;
const REG_TDH: usize = 0x3810;
const REG_TDT: usize = 0x3818;
const REG_TXDCTL: usize = 0x3828;
const REG_TARC0: usize = 0x3840;
const REG_TXDCTL1: usize = 0x3928;
const REG_TARC1: usize = 0x3940;
const REG_RFCTL: usize = 0x5008;
const REG_RAL0: usize = 0x5400;
const REG_RAH0: usize = 0x5404;
const REG_MRQC: usize = 0x5818;
const REG_FWSM: usize = 0x5b54;
const REG_GPRC: usize = 0x4074;
const REG_GPTC: usize = 0x4080;

const CTRL_GIO_MASTER_DISABLE: u32 = 1 << 2;
const CTRL_SLU: u32 = 1 << 6;
const CTRL_FRCSPD: u32 = 1 << 11;
const CTRL_FRCDPX: u32 = 1 << 12;
const CTRL_RST: u32 = 1 << 26;
const CTRL_EXT_RO_DIS: u32 = 1 << 17;
const CTRL_EXT_PCH_REQUIRED: u32 = 1 << 22;
const CTRL_EXT_DRV_LOAD: u32 = 1 << 28;
const STATUS_LU: u32 = 1 << 1;
const STATUS_LAN_INIT_DONE: u32 = 1 << 9;
const STATUS_PHYRA: u32 = 1 << 10;
const STATUS_GIO_MASTER_ENABLE: u32 = 1 << 19;

const RFCTL_NFSW_DIS: u32 = 1 << 6;
const RFCTL_NFSR_DIS: u32 = 1 << 7;
const RFCTL_EXTEN: u32 = 1 << 15;

const RCTL_EN: u32 = 1 << 1;
const RCTL_UPE: u32 = 1 << 3;
const RCTL_MPE: u32 = 1 << 4;
const RCTL_BAM: u32 = 1 << 15;
const RCTL_SECRC: u32 = 1 << 26;

const TCTL_EN: u32 = 1 << 1;
const TCTL_PSP: u32 = 1 << 3;
const TCTL_CT: u32 = 0x0f << 4;
const TCTL_COLD: u32 = 0x3f << 12;
const TCTL_RTLC: u32 = 1 << 24;
const TCTL_MULR: u32 = 1 << 28;
const TCTL_CT_MASK: u32 = 0x0000_0ff0;
const TCTL_COLD_MASK: u32 = 0x003f_f000;

const QUEUE_ENABLE: u32 = 1 << 25;
const RXDCTL_PCH_POLLING: u32 = 1 << 24;
const TXDCTL_PCH_POLLING: u32 = (1 << 24) | (1 << 22) | (1 << 16) | 0x1f;
const TXDCTL_PTHRESH: u32 = 0x0000_003f;
const TXDCTL_WTHRESH: u32 = 0x003f_0000;
const TXDCTL_LWTHRESH: u32 = 1 << 25;

const FWSM_FW_VALID: u32 = 0x0000_8000;
const FWSM_PCIM2PCI: u32 = 0x0100_0000;
const FWSM_PCIM2PCI_COUNT: usize = 2000;
const LAN_INIT_POLL_COUNT: usize = 1500;
const FEXTNVM3_PHY_CFG_COUNTER_MASK: u32 = 0x0c00_0000;
const FEXTNVM3_PHY_CFG_COUNTER_50MSEC: u32 = 0x0800_0000;

const RXD_STAT_DD: u8 = 1 << 0;
const RXD_STAT_EOP: u8 = 1 << 1;
const RXD_ERR_FRAME: u8 = 0x97;
const RXDEXT_STATERR_DD: u32 = 1 << 0;
const RXDEXT_STATERR_EOP: u32 = 1 << 1;
const RXDEXT_ERR_FRAME_ERR_MASK: u32 = 0x9700_0000;

const TXD_CMD_EOP: u8 = 1 << 0;
const TXD_CMD_IFCS: u8 = 1 << 1;
const TXD_CMD_RS: u8 = 1 << 3;
const TXD_STAT_DD: u8 = 1 << 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Profile {
    I82574L,
    I82579LM,
}

impl Profile {
    fn name(self) -> &'static str {
        match self {
            Profile::I82574L => "82574L",
            Profile::I82579LM => "82579LM",
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RxDescLegacy {
    address: u64,
    length: u16,
    checksum: u16,
    status: u8,
    errors: u8,
    special: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RxDescExtRead {
    buffer_addr: u64,
    reserved: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RxDescExtWriteback {
    mrq: u32,
    rss_or_csum: u32,
    status_error: u32,
    length: u16,
    vlan: u16,
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
union RxDesc {
    legacy: RxDescLegacy,
    ext_read: RxDescExtRead,
    ext_wb: RxDescExtWriteback,
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct TxDesc {
    address: u64,
    length: u16,
    checksum_offset: u8,
    command: u8,
    status: u8,
    checksum_start: u8,
    special: u16,
}

const _: () = {
    assert!(core::mem::size_of::<RxDesc>() == 16);
    assert!(core::mem::size_of::<TxDesc>() == 16);
};

pub struct E1000e {
    profile: Profile,
    mmio: usize,
    _mmio_mapping: MmioMapping,
    mac: [u8; 6],

    rx_ring_phys: u32,
    tx_ring_phys: u32,
    rx_ring: *mut RxDesc,
    tx_ring: *mut TxDesc,
    rx_buffers_phys: u32,
    tx_buffers_phys: u32,
    rx_buffers: *mut u8,
    tx_buffers: *mut u8,

    _rx_ring_dma: DmaAllocation,
    _tx_ring_dma: DmaAllocation,
    _rx_buffers_dma: DmaAllocation,
    _tx_buffers_dma: DmaAllocation,

    rx_head: AtomicUsize,
    tx_head: AtomicUsize,
    rx_packets: AtomicUsize,
    tx_packets: AtomicUsize,
    rx_drops: AtomicUsize,
    tx_busy: AtomicUsize,
    hw_rx_total: AtomicUsize,
    hw_tx_total: AtomicUsize,
    initialized: AtomicBool,
}

unsafe impl Send for E1000e {}
unsafe impl Sync for E1000e {}

pub static NET: Mutex<Option<E1000e>> = Mutex::new(None);

fn alloc_dma(owner: &'static str, bytes: usize) -> Result<DmaAllocation, &'static str> {
    dma_alloc_for(owner, bytes, 4096, u32::MAX as u64)
        .map_err(|_| "e1000e-min DMA allocation failed")
}

#[inline]
fn dma_sync() {
    crate::memory::resources::dma_mb();
}

#[inline]
fn hw_delay_us(us: usize) {
    let waits = us.saturating_mul(32).saturating_add(99) / 100;
    for _ in 0..waits.max(1) {
        crate::io::io_wait();
    }
}

#[inline]
fn hw_delay_ms(ms: usize) {
    hw_delay_us(ms.saturating_mul(1000));
}

#[inline]
fn valid_mac(mac: [u8; 6]) -> bool {
    mac != [0; 6] && mac != [0xff; 6] && mac[0] & 1 == 0
}

impl E1000e {
    pub fn init() -> Result<(), &'static str> {
        let (dev, profile) = if let Some(dev) = pci::find_device(VENDOR_INTEL, DEVICE_82579LM) {
            (dev, Profile::I82579LM)
        } else if let Some(dev) = pci::find_device(VENDOR_INTEL, DEVICE_82574L) {
            (dev, Profile::I82574L)
        } else {
            return Err("supported e1000e NIC not found");
        };

        println!(
            "e1000e-min: found {} {:02x}:{:02x}.{} rev {:02x} IRQ {}",
            profile.name(),
            dev.bus,
            dev.device,
            dev.function,
            dev.revision_id,
            dev.interrupt_line,
        );

        dev.enable_bus_mastering();
        // This baseline is polling-only. Keep legacy INTx disabled as well as
        // masking all interrupt causes in the NIC.
        dev.write_u16(0x04, dev.read_u16(0x04) | 0x0400);

        let (bar_phys, bar_size) = match dev.get_bar(0) {
            Some(pci::bar::Bar::Memory { address, size, .. }) => (*address, *size),
            _ => return Err("e1000e-min BAR0 is not MMIO"),
        };
        if bar_size < 0x6000 {
            return Err("e1000e-min BAR0 too small");
        }
        let mmio_mapping = map_mmio(bar_phys, bar_size)?;
        let mmio = mmio_mapping.as_usize();

        let rx_ring_dma = alloc_dma(
            "e1000e-min RX ring",
            core::mem::size_of::<RxDesc>() * RX_RING_SIZE,
        )?;
        let tx_ring_dma = alloc_dma(
            "e1000e-min TX ring",
            core::mem::size_of::<TxDesc>() * TX_RING_SIZE,
        )?;
        let rx_buffers_dma = alloc_dma("e1000e-min RX buffers", RX_BUF_SIZE * RX_RING_SIZE)?;
        let tx_buffers_dma = alloc_dma("e1000e-min TX buffers", TX_BUF_SIZE * TX_RING_SIZE)?;

        let mut nic = E1000e {
            profile,
            mmio,
            _mmio_mapping: mmio_mapping,
            mac: [0; 6],
            rx_ring_phys: rx_ring_dma.phys.0,
            tx_ring_phys: tx_ring_dma.phys.0,
            rx_ring: rx_ring_dma.as_mut_ptr() as *mut RxDesc,
            tx_ring: tx_ring_dma.as_mut_ptr() as *mut TxDesc,
            rx_buffers_phys: rx_buffers_dma.phys.0,
            tx_buffers_phys: tx_buffers_dma.phys.0,
            rx_buffers: rx_buffers_dma.as_mut_ptr(),
            tx_buffers: tx_buffers_dma.as_mut_ptr(),
            _rx_ring_dma: rx_ring_dma,
            _tx_ring_dma: tx_ring_dma,
            _rx_buffers_dma: rx_buffers_dma,
            _tx_buffers_dma: tx_buffers_dma,
            rx_head: AtomicUsize::new(0),
            tx_head: AtomicUsize::new(0),
            rx_packets: AtomicUsize::new(0),
            tx_packets: AtomicUsize::new(0),
            rx_drops: AtomicUsize::new(0),
            tx_busy: AtomicUsize::new(0),
            hw_rx_total: AtomicUsize::new(0),
            hw_tx_total: AtomicUsize::new(0),
            initialized: AtomicBool::new(false),
        };

        nic.mask_interrupts();
        let preserved_mac = if profile == Profile::I82579LM {
            Some(nic.read_current_mac())
        } else {
            None
        };
        if profile == Profile::I82579LM {
            nic.pch2_takeover();
            // RAL/RAH can already be restored even when LAN_INIT_DONE is not
            // observable on some PXE/ME combinations. Only wait for flash/NVM
            // reload when the post-reset address is actually invalid.
            if !valid_mac(nic.read_current_mac()) {
                nic.wait_lan_init();
            }
            nic.clear_phyra();
        } else {
            nic.stop_dma();
        }
        nic.read_or_program_mac(
            dev.bus,
            dev.device,
            dev.function,
            dev.revision_id,
            preserved_mac,
        );
        nic.setup_descriptors();
        // GPRC/GPTC are clear-on-read statistics registers. Drop any counts
        // inherited from firmware/PXE before starting our own accounting.
        let _ = nic.read(REG_GPRC);
        let _ = nic.read(REG_GPTC);
        nic.start_dma();
        nic.initialized.store(true, Ordering::Release);

        println!(
            "e1000e-min: {} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} link={} polling-only",
            nic.profile.name(),
            nic.mac[0],
            nic.mac[1],
            nic.mac[2],
            nic.mac[3],
            nic.mac[4],
            nic.mac[5],
            nic.link_up(),
        );

        *NET.lock() = Some(nic);
        Ok(())
    }

    #[inline]
    fn read(&self, register: usize) -> u32 {
        unsafe { read_volatile((self.mmio + register) as *const u32) }
    }

    #[inline]
    fn prepare_mmio_write(&self) {
        if self.profile != Profile::I82579LM || self.read(REG_FWSM) & FWSM_FW_VALID == 0 {
            return;
        }
        let mut remaining = FWSM_PCIM2PCI_COUNT;
        while self.read(REG_FWSM) & FWSM_PCIM2PCI != 0 && remaining > 1 {
            hw_delay_us(50);
            remaining -= 1;
        }
    }

    #[inline]
    fn write(&self, register: usize, value: u32) {
        self.prepare_mmio_write();
        unsafe { write_volatile((self.mmio + register) as *mut u32, value) };
        // Flush posted PCI/MMIO writes.
        let _ = self.read(REG_STATUS);
    }

    fn mask_interrupts(&self) {
        self.write(REG_IMC, u32::MAX);
        let _ = self.read(REG_ICR);
    }

    fn stop_dma(&self) {
        self.write(REG_RCTL, 0);
        self.write(REG_TCTL, self.read(REG_TCTL) & !TCTL_EN);
        self.write(REG_MRQC, 0);
        if self.profile == Profile::I82579LM {
            self.write(
                REG_RFCTL,
                self.read(REG_RFCTL) | RFCTL_NFSW_DIS | RFCTL_NFSR_DIS | RFCTL_EXTEN,
            );
        } else {
            self.write(REG_RFCTL, self.read(REG_RFCTL) & !RFCTL_EXTEN);
        }
    }

    fn pch2_takeover(&self) {
        // 82579/PCH2 may still be owned/configured by PXE/ME. Stop DMA,
        // drain PCIe master requests, then reset only the MAC (not the PHY).
        // This follows the conservative part of Linux e1000e's ich8lan reset
        // sequence while keeping the new driver polling-only.
        self.mask_interrupts();
        self.write(REG_RCTL, 0);
        self.write(REG_TCTL, TCTL_PSP);
        hw_delay_ms(10);

        let mut ctrl = self.read(REG_CTRL) | CTRL_GIO_MASTER_DISABLE;
        self.write(REG_CTRL, ctrl);
        let mut master_drained = false;
        for _ in 0..800 {
            if self.read(REG_STATUS) & STATUS_GIO_MASTER_ENABLE == 0 {
                master_drained = true;
                break;
            }
            hw_delay_us(100);
        }
        if !master_drained {
            println!(
                "e1000e-min: 82579 PCIe master drain timeout STATUS={:#010x}",
                self.read(REG_STATUS)
            );
        }

        // Do not use write() for CTRL.RST. PCH2's reset path deliberately
        // avoids the immediate STATUS read used to flush ordinary MMIO writes.
        ctrl &= !(CTRL_FRCSPD | CTRL_FRCDPX);
        ctrl |= CTRL_RST;
        self.prepare_mmio_write();
        unsafe { write_volatile((self.mmio + REG_CTRL) as *mut u32, ctrl) };
        hw_delay_ms(20);

        let mut fextnvm3 = self.read(REG_FEXTNVM3);
        fextnvm3 &= !FEXTNVM3_PHY_CFG_COUNTER_MASK;
        fextnvm3 |= FEXTNVM3_PHY_CFG_COUNTER_50MSEC;
        self.write(REG_FEXTNVM3, fextnvm3);

        self.mask_interrupts();
        self.write(
            REG_CTRL_EXT,
            self.read(REG_CTRL_EXT)
                | CTRL_EXT_DRV_LOAD
                | CTRL_EXT_RO_DIS
                | CTRL_EXT_PCH_REQUIRED,
        );
        let ctrl = (self.read(REG_CTRL) | CTRL_SLU) & !(CTRL_FRCSPD | CTRL_FRCDPX);
        self.write(REG_CTRL, ctrl);
    }

    fn wait_lan_init(&self) {
        for _ in 0..LAN_INIT_POLL_COUNT {
            if self.read(REG_STATUS) & STATUS_LAN_INIT_DONE != 0 {
                return;
            }
            hw_delay_us(100);
        }
        println!(
            "e1000e-min: 82579 LAN_INIT_DONE timeout STATUS={:#010x}",
            self.read(REG_STATUS)
        );
    }

    fn clear_phyra(&self) {
        let status = self.read(REG_STATUS);
        if status & STATUS_PHYRA != 0 {
            self.write(REG_STATUS, status & !STATUS_PHYRA);
            hw_delay_us(100);
        }
    }

    fn read_current_mac(&self) -> [u8; 6] {
        let ral = self.read(REG_RAL0);
        let rah = self.read(REG_RAH0);
        [
            ral as u8,
            (ral >> 8) as u8,
            (ral >> 16) as u8,
            (ral >> 24) as u8,
            rah as u8,
            (rah >> 8) as u8,
        ]
    }

    fn program_mac(&self, mac: [u8; 6]) {
        let ral = u32::from_le_bytes([mac[0], mac[1], mac[2], mac[3]]);
        let rah = (mac[4] as u32) | ((mac[5] as u32) << 8) | (1 << 31);
        self.write(REG_RAL0, ral);
        self.write(REG_RAH0, rah);
    }

    fn read_or_program_mac(
        &mut self,
        bus: u8,
        device: u8,
        function: u8,
        revision: u8,
        preserved: Option<[u8; 6]>,
    ) {
        let mut mac = self.read_current_mac();
        if !valid_mac(mac) {
            if let Some(saved) = preserved.filter(|saved| valid_mac(*saved)) {
                mac = saved;
            } else {
                mac = [0x02, 0x80, 0x86, bus, device, function ^ revision];
            }
            self.program_mac(mac);
        }
        self.mac = mac;
    }

    unsafe fn rearm_rx_desc(&self, slot: usize) {
        let desc = unsafe { self.rx_ring.add(slot) };
        let address = (self.rx_buffers_phys as u64) + (slot * RX_BUF_SIZE) as u64;
        match self.profile {
            Profile::I82574L => unsafe {
                write_volatile(
                    desc,
                    RxDesc {
                        legacy: RxDescLegacy {
                            address,
                            length: 0,
                            checksum: 0,
                            status: 0,
                            errors: 0,
                            special: 0,
                        },
                    },
                );
            },
            Profile::I82579LM => unsafe {
                write_volatile(
                    desc,
                    RxDesc {
                        ext_read: RxDescExtRead {
                            buffer_addr: address,
                            reserved: 0,
                        },
                    },
                );
            },
        }
    }

    fn setup_descriptors(&mut self) {
        unsafe {
            for i in 0..RX_RING_SIZE {
                self.rearm_rx_desc(i);
            }
            for i in 0..TX_RING_SIZE {
                let desc = self.tx_ring.add(i);
                write_volatile(
                    desc,
                    TxDesc {
                        address: (self.tx_buffers_phys as u64) + (i * TX_BUF_SIZE) as u64,
                        length: 0,
                        checksum_offset: 0,
                        command: 0,
                        status: TXD_STAT_DD,
                        checksum_start: 0,
                        special: 0,
                    },
                );
            }
        }
        dma_sync();
    }

    fn start_dma(&self) {
        if self.profile == Profile::I82579LM {
            // MAC reset in pch2_takeover() restores RFCTL to reset defaults.
            // Linux e1000e explicitly enables extended status descriptors
            // again during RX configuration. Without EXTEN the 82579 writes
            // legacy RX writeback while Felix interprets the ring as extended.
            self.write(
                REG_CTRL_EXT,
                self.read(REG_CTRL_EXT)
                    | CTRL_EXT_DRV_LOAD
                    | CTRL_EXT_RO_DIS
                    | CTRL_EXT_PCH_REQUIRED,
            );
            self.write(
                REG_RFCTL,
                self.read(REG_RFCTL) | RFCTL_NFSW_DIS | RFCTL_NFSR_DIS | RFCTL_EXTEN,
            );
        } else {
            self.write(REG_RFCTL, self.read(REG_RFCTL) & !RFCTL_EXTEN);
        }

        self.write(REG_MRQC, 0);

        if self.profile == Profile::I82579LM {
            // PCH2 transmit arbiter setup used by Linux e1000e/ich8lan.
            self.write(
                REG_TARC0,
                self.read(REG_TARC0) | (1 << 23) | (1 << 24) | (1 << 26) | (1 << 27),
            );
            let mut tarc1 = self.read(REG_TARC1);
            if self.read(REG_TCTL) & TCTL_MULR != 0 {
                tarc1 &= !(1 << 28);
            } else {
                tarc1 |= 1 << 28;
            }
            tarc1 |= (1 << 24) | (1 << 26) | (1 << 30);
            self.write(REG_TARC1, tarc1);
        }

        self.write(REG_RDBAL, self.rx_ring_phys);
        self.write(REG_RDBAH, 0);
        self.write(
            REG_RDLEN,
            (core::mem::size_of::<RxDesc>() * RX_RING_SIZE) as u32,
        );
        self.write(REG_RDH, 0);
        if self.profile == Profile::I82579LM {
            self.write(REG_RDTR, 0);
            self.write(REG_RADV, 0);
        }
        let wanted_rdt = (RX_RING_SIZE - 1) as u32;
        self.write(REG_RDT, wanted_rdt);
        if self.profile == Profile::I82579LM {
            let actual = self.read(REG_RDT);
            if actual != wanted_rdt {
                println!(
                    "e1000e-min: 82579 RDT init lost wanted={} actual={} FWSM={:#010x}",
                    wanted_rdt, actual, self.read(REG_FWSM)
                );
                // ME/PCIm2PCI may drop a posted tail store; try again after
                // the write-arbitration wait in write().
                self.write(REG_RDT, wanted_rdt);
            }
        }

        self.write(REG_TDBAL, self.tx_ring_phys);
        self.write(REG_TDBAH, 0);
        self.write(
            REG_TDLEN,
            (core::mem::size_of::<TxDesc>() * TX_RING_SIZE) as u32,
        );
        self.write(REG_TDH, 0);
        self.write(REG_TDT, 0);

        // 82574 uses explicit queue-enable bits. 82579's PCH2 layout treats
        // bit 25 differently, so leave its queue controls at the legacy value.
        if self.profile == Profile::I82574L {
            self.write(REG_RXDCTL, self.read(REG_RXDCTL) | QUEUE_ENABLE);
            self.write(REG_TXDCTL, self.read(REG_TXDCTL) | QUEUE_ENABLE);
        } else {
            // PCH2: bit 25 is not queue-enable. Use descriptor-granularity
            // polling/writeback policy and mirror it on the second TX queue so
            // no stale BIOS/PXE threshold state survives warm takeover.
            self.write(REG_RXDCTL, RXDCTL_PCH_POLLING);
            let mut txdctl = self.read(REG_TXDCTL);
            txdctl &= !(TXDCTL_PTHRESH | TXDCTL_WTHRESH | TXDCTL_LWTHRESH);
            txdctl |= TXDCTL_PCH_POLLING;
            self.write(REG_TXDCTL, txdctl);
            self.write(REG_TXDCTL1, txdctl);
        }

        self.write(REG_TIPG, 10 | (8 << 10) | (6 << 20));
        if self.profile == Profile::I82579LM {
            // Preserve PCH/firmware-owned TCTL bits; replace only collision
            // fields and force the transmitter on.
            let mut tctl = self.read(REG_TCTL);
            tctl &= !(TCTL_CT_MASK | TCTL_COLD_MASK);
            tctl |= TCTL_EN | TCTL_PSP | TCTL_CT | TCTL_COLD | TCTL_RTLC;
            self.write(REG_TCTL, tctl);
        } else {
            self.write(
                REG_TCTL,
                TCTL_EN | TCTL_PSP | TCTL_CT | TCTL_COLD | TCTL_RTLC,
            );
        }
        self.write(REG_CTRL, self.read(REG_CTRL) | CTRL_SLU);
        self.write(
            REG_RCTL,
            RCTL_EN | RCTL_UPE | RCTL_MPE | RCTL_BAM | RCTL_SECRC,
        );
        dma_sync();
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    pub fn link_up(&self) -> bool {
        self.read(REG_STATUS) & STATUS_LU != 0
    }

    pub fn can_transmit(&self) -> bool {
        if !self.initialized.load(Ordering::Acquire) {
            return false;
        }
        let slot = self.tx_head.load(Ordering::Relaxed) % TX_RING_SIZE;
        dma_sync();
        unsafe { read_volatile(&(*self.tx_ring.add(slot)).status) & TXD_STAT_DD != 0 }
    }

    pub fn send(&self, data: &[u8]) -> Result<(), &'static str> {
        if !self.initialized.load(Ordering::Acquire) {
            return Err("e1000e-min not initialized");
        }
        if data.is_empty() || data.len() > TX_BUF_SIZE {
            return Err("e1000e-min invalid TX length");
        }

        let slot = self.tx_head.load(Ordering::Relaxed) % TX_RING_SIZE;
        dma_sync();
        let desc = unsafe { self.tx_ring.add(slot) };
        let done = unsafe { read_volatile(&(*desc).status) & TXD_STAT_DD != 0 };
        if !done {
            self.tx_busy.fetch_add(1, Ordering::Relaxed);
            return Err("e1000e-min TX busy");
        }

        unsafe {
            core::ptr::copy_nonoverlapping(
                data.as_ptr(),
                self.tx_buffers.add(slot * TX_BUF_SIZE),
                data.len(),
            );
            write_volatile(&mut (*desc).length, data.len() as u16);
            write_volatile(&mut (*desc).checksum_offset, 0);
            write_volatile(&mut (*desc).checksum_start, 0);
            write_volatile(&mut (*desc).special, 0);
            write_volatile(&mut (*desc).status, 0);
            write_volatile(
                &mut (*desc).command,
                TXD_CMD_EOP | TXD_CMD_IFCS | TXD_CMD_RS,
            );
        }
        dma_sync();

        let next = (slot + 1) % TX_RING_SIZE;
        self.write(REG_TDT, next as u32);
        self.tx_head.store(next, Ordering::Relaxed);
        self.tx_packets.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub fn recv(&self, output: &mut [u8]) -> Option<usize> {
        if !self.initialized.load(Ordering::Acquire) {
            return None;
        }

        let slot = self.rx_head.load(Ordering::Relaxed) % RX_RING_SIZE;
        let desc = unsafe { self.rx_ring.add(slot) };
        dma_sync();

        let (ready, eop, frame_error, length) = match self.profile {
            Profile::I82574L => {
                let d = unsafe { read_volatile(&raw const (*desc).legacy) };
                (
                    d.status & RXD_STAT_DD != 0,
                    d.status & RXD_STAT_EOP != 0,
                    d.errors & RXD_ERR_FRAME != 0,
                    d.length as usize,
                )
            }
            Profile::I82579LM => {
                let d = unsafe { read_volatile(&raw const (*desc).ext_wb) };
                (
                    d.status_error & RXDEXT_STATERR_DD != 0,
                    d.status_error & RXDEXT_STATERR_EOP != 0,
                    d.status_error & RXDEXT_ERR_FRAME_ERR_MASK != 0,
                    d.length as usize,
                )
            }
        };

        if !ready {
            return None;
        }

        let valid =
            eop && !frame_error && length >= 14 && length <= RX_BUF_SIZE && length <= output.len();

        if valid {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    self.rx_buffers.add(slot * RX_BUF_SIZE),
                    output.as_mut_ptr(),
                    length,
                );
            }
            self.rx_packets.fetch_add(1, Ordering::Relaxed);
        } else {
            self.rx_drops.fetch_add(1, Ordering::Relaxed);
        }

        unsafe { self.rearm_rx_desc(slot) };
        dma_sync();
        self.write(REG_RDT, slot as u32);
        self.rx_head
            .store((slot + 1) % RX_RING_SIZE, Ordering::Relaxed);

        valid.then_some(length)
    }
}

impl E1000e {
    fn fmt_debug(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Intel statistics registers are clear-on-read. Keep a software total
        // so repeated F12 dumps do not misleadingly jump back to zero.
        let hw_rx_delta = self.read(REG_GPRC) as usize;
        let hw_tx_delta = self.read(REG_GPTC) as usize;
        let hw_rx_total = self.hw_rx_total.fetch_add(hw_rx_delta, Ordering::Relaxed) + hw_rx_delta;
        let hw_tx_total = self.hw_tx_total.fetch_add(hw_tx_delta, Ordering::Relaxed) + hw_tx_delta;

        let tx_slot = self.tx_head.load(Ordering::Relaxed) % TX_RING_SIZE;
        let tx_desc_status = unsafe { read_volatile(&(*self.tx_ring.add(tx_slot)).status) };
        let rx_slot = self.rx_head.load(Ordering::Relaxed) % RX_RING_SIZE;
        let (rx_desc_status, rx_desc_len) = match self.profile {
            Profile::I82574L => {
                let d = unsafe { read_volatile(&raw const (*self.rx_ring.add(rx_slot)).legacy) };
                (((d.errors as u32) << 8) | d.status as u32, d.length as usize)
            }
            Profile::I82579LM => {
                let d = unsafe { read_volatile(&raw const (*self.rx_ring.add(rx_slot)).ext_wb) };
                (d.status_error, d.length as usize)
            }
        };

        let rx_raw0 = unsafe { read_volatile(self.rx_ring.add(rx_slot) as *const u64) };
        let rx_raw1 = unsafe { read_volatile((self.rx_ring.add(rx_slot) as *const u64).add(1)) };

        writeln!(
            f,
            "e1000e-min {} link={} CTRL={:#010x} STATUS={:#010x} CTRL_EXT={:#010x} FWSM={:#010x}",
            self.profile.name(), self.link_up(), self.read(REG_CTRL), self.read(REG_STATUS),
            self.read(REG_CTRL_EXT), self.read(REG_FWSM),
        )?;
        writeln!(
            f,
            "RXCFG RCTL={:#010x} RFCTL={:#010x} RXDCTL={:#010x} MRQC={:#010x}",
            self.read(REG_RCTL), self.read(REG_RFCTL), self.read(REG_RXDCTL), self.read(REG_MRQC),
        )?;
        writeln!(
            f,
            "TX sw={} busy={} hw_total={} (+{}) head={} TDH={} TDT={} desc={:#04x}",
            self.tx_packets.load(Ordering::Relaxed), self.tx_busy.load(Ordering::Relaxed),
            hw_tx_total, hw_tx_delta, tx_slot, self.read(REG_TDH), self.read(REG_TDT), tx_desc_status,
        )?;
        writeln!(
            f,
            "RX sw={} drop={} hw_total={} (+{}) head={} RDH={} RDT={} desc={:#010x} len={} raw={:016x}:{:016x}",
            self.rx_packets.load(Ordering::Relaxed), self.rx_drops.load(Ordering::Relaxed),
            hw_rx_total, hw_rx_delta, rx_slot, self.read(REG_RDH), self.read(REG_RDT),
            rx_desc_status, rx_desc_len, rx_raw0, rx_raw1,
        )
    }
}

pub struct DebugSnapshot;

impl fmt::Display for DebugSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Before net::stack::init_e1000e(), the NIC lives in this module's
        // temporary NET slot. bring_up() then moves it into NET_STACK, so
        // looking only at NET made F12 incorrectly say "not active" after a
        // successful network init.
        if let Some(net) = NET.try_lock() {
            if let Some(nic) = net.as_ref() {
                return nic.fmt_debug(f);
            }
        }

        let Some(stack) = crate::net::stack::NET_STACK.try_lock() else {
            return writeln!(f, "e1000e-min: NET_STACK lock busy");
        };
        let Some(stack) = stack.as_ref() else {
            return writeln!(f, "e1000e-min: net stack not active");
        };
        match &stack.device {
            crate::drivers::net::AnyNic::E1000e(nic) => nic.fmt_debug(f),
            _ => writeln!(f, "e1000e-min: another NIC is active"),
        }
    }
}
