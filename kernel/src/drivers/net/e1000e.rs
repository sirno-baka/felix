//! Minimal Intel 82579LM (PCH2 / e1000e family) Ethernet driver.
//!
//! The first implementation intentionally uses polling,
//! extended RX / legacy TX descriptors and no checksum/VLAN offload.
//! The 82579 PHY and firmware retain responsibility
//! for copper autonegotiation; Felix owns the MAC DMA rings.

use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{compiler_fence, AtomicBool, AtomicUsize, Ordering};

use crate::drivers::net::{map_mmio, RX_RING_SIZE, TX_BUF_SIZE};
use crate::memory::paging::{KERNEL_OFFSET, PAGE_SIZE, PAGING};
use crate::pci;
use crate::println;
use crate::sync::mutex::Mutex;

const VENDOR_INTEL: u16 = 0x8086;
const DEVICE_82579LM: u16 = 0x1502;
const E1000E_TX_RING_SIZE: usize = 64;
const E1000E_RX_BUF_SIZE: usize = 2048;

const REG_CTRL: usize = 0x0000;
const REG_STATUS: usize = 0x0008;
const REG_CTRL_EXT: usize = 0x0018;
const REG_FEXTNVM3: usize = 0x003c;
const REG_MDIC: usize = 0x0020;
const REG_ICR: usize = 0x00c0;
const REG_IMC: usize = 0x00d8;
const REG_RCTL: usize = 0x0100;
const REG_TCTL: usize = 0x0400;
const REG_RFCTL: usize = 0x5008;
const REG_MRQC: usize = 0x5818;
const REG_MANC: usize = 0x5820;
const REG_PBA: usize = 0x1000;
const REG_CRCERRS: usize = 0x4000;
const REG_RXERRC: usize = 0x400c;
const REG_MPC: usize = 0x4010;
const REG_GPRC: usize = 0x4074;
const REG_RNBC: usize = 0x40a0;
const REG_TPR: usize = 0x40d0;
const REG_FWSM: usize = 0x5b54;
const REG_TDFH: usize = 0x3410;
const REG_TDFT: usize = 0x3418;
const REG_TDFHS: usize = 0x3420;
const REG_TDFTS: usize = 0x3428;
const REG_TDFPC: usize = 0x3430;
const REG_GPTC: usize = 0x4080;
const REG_TPT: usize = 0x40d4;
const REG_TNCRS: usize = 0x4034;
const REG_ECOL: usize = 0x4018;
const REG_LATECOL: usize = 0x4020;
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
const REG_TXDCTL0: usize = 0x3828;
const REG_TXDCTL1: usize = 0x3928;
const REG_TARC0: usize = 0x3840;
const REG_TARC1: usize = 0x3940;
const REG_RAL0: usize = 0x5400;
const REG_RAH0: usize = 0x5404;

const CTRL_GIO_MASTER_DISABLE: u32 = 1 << 2;
const CTRL_SLU: u32 = 1 << 6;
const CTRL_FRCSPD: u32 = 1 << 11;
const CTRL_FRCDPX: u32 = 1 << 12;
const CTRL_RST: u32 = 1 << 26;
const CTRL_EXT_DRV_LOAD: u32 = 1 << 28;
const CTRL_EXT_RO_DIS: u32 = 1 << 17;
const CTRL_EXT_PCH_REQUIRED: u32 = 1 << 22;
const STATUS_LU: u32 = 1 << 1;
const STATUS_LAN_INIT_DONE: u32 = 1 << 9;
const STATUS_PHYRA: u32 = 1 << 10;
const STATUS_GIO_MASTER_ENABLE: u32 = 1 << 19;

const MDIC_PHY_ADDR: u32 = 1;
const MDIC_OP_WRITE: u32 = 1 << 26;
const MDIC_OP_READ: u32 = 2 << 26;
const MDIC_READY: u32 = 1 << 28;
const MDIC_ERROR: u32 = 1 << 30;
const PHY_CONTROL: u8 = 0;
const PHY_STATUS: u8 = 1;
const PHY_CTRL_RESTART_AUTONEG: u16 = 1 << 9;
const PHY_CTRL_ISOLATE: u16 = 1 << 10;
const PHY_CTRL_POWER_DOWN: u16 = 1 << 11;
const PHY_CTRL_AUTONEG_ENABLE: u16 = 1 << 12;

const RCTL_EN: u32 = 1 << 1;
const RCTL_UPE: u32 = 1 << 3;
const RCTL_MPE: u32 = 1 << 4;
const RCTL_BAM: u32 = 1 << 15;
const RCTL_SECRC: u32 = 1 << 26;
const TCTL_EN: u32 = 1 << 1;
const TCTL_PSP: u32 = 1 << 3;
const TCTL_CT_MASK: u32 = 0x0000_0ff0;
const TCTL_COLD_MASK: u32 = 0x003f_f000;
const TCTL_CT: u32 = 0x0f << 4;
const TCTL_COLD: u32 = 0x3f << 12;
const TCTL_RTLC: u32 = 1 << 24;
const TCTL_MULR: u32 = 1 << 28;

const FWSM_FW_VALID: u32 = 0x0000_8000;
const FWSM_PCIM2PCI: u32 = 0x0100_0000;
const FWSM_PCIM2PCI_COUNT: usize = 2000;
const FEXTNVM3_PHY_CFG_COUNTER_MASK: u32 = 0x0c00_0000;
const FEXTNVM3_PHY_CFG_COUNTER_50MSEC: u32 = 0x0800_0000;

// 82579/PCH2 uses the classic e1000e TXDCTL layout.  Bit 25 is part of
// LWTHRESH here; it is NOT the per-queue enable bit used by newer igb/igc
// controllers.
const TXDCTL_PTHRESH: u32 = 0x0000_003f;
const TXDCTL_WTHRESH: u32 = 0x003f_0000;
const TXDCTL_LWTHRESH: u32 = 0xfe00_0000;
const TXDCTL_GRAN: u32 = 1 << 24;
const TXDCTL_COUNT_DESC: u32 = 1 << 22;
const TXDCTL_FULL_TX_DESC_WB: u32 = TXDCTL_GRAN | (1 << 16);
const TXDCTL_MAX_TX_DESC_PREFETCH: u32 = TXDCTL_GRAN | 0x1f;
const RFCTL_NFSW_DIS: u32 = 1 << 6;
const RFCTL_NFSR_DIS: u32 = 1 << 7;
const RFCTL_EXTEN: u32 = 1 << 15;
// Polling needs prompt completion of isolated packets (DHCP/ARP). Disable
// prefetch and writeback batching: PTHRESH=HTHRESH=WTHRESH=0, GRAN=1.
// 82579 datasheet 12.0.3.4.13 limits descriptor thresholds to 0..31;
// the former PTHRESH=0x20 was outside that range. Nonzero WTHRESH also
// requires nonzero RDTR/RADV, which this polling driver does not use.
const RXDCTL_POLLING: u32 = 1 << 24;

const RXDEXT_STATERR_DD: u32 = 1 << 0;
const RXDEXT_STATERR_EOP: u32 = 1 << 1;
// Intel's frame-error mask deliberately excludes TCP/IP checksum errors:
// CE | SE | SEQ | CXE | RXE.
const RXDEXT_ERR_FRAME_ERR_MASK: u32 = 0x9700_0000;
const TXD_CMD_EOP: u8 = 1 << 0;
const TXD_CMD_IFCS: u8 = 1 << 1;
const TXD_CMD_RS: u8 = 1 << 3;
const TXD_STAT_DD: u8 = 1 << 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct RxDescRead {
    buffer_addr: u64,
    reserved: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RxDescWriteback {
    mrq: u32,
    rss_or_csum: u32,
    status_error: u32,
    length: u16,
    vlan: u16,
}

/// 82579/e1000e extended receive descriptor. The hardware consumes the
/// `read` view and overwrites all 16 bytes with the `wb` view on completion.
#[repr(C, align(16))]
#[derive(Clone, Copy)]
union RxDesc {
    read: RxDescRead,
    wb: RxDescWriteback,
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

// These offsets are a hardware ABI, including on the 32-bit kernel target.
const _: () = {
    assert!(core::mem::size_of::<RxDesc>() == 16);
    assert!(core::mem::size_of::<TxDesc>() == 16);
    assert!(core::mem::offset_of!(RxDescWriteback, status_error) == 8);
    assert!(core::mem::offset_of!(RxDescWriteback, length) == 12);
    assert!(core::mem::offset_of!(TxDesc, status) == 12);
};

pub struct E1000e {
    mmio: usize,
    mac: [u8; 6],
    rx_ring_phys: u32,
    tx_ring_phys: u32,
    rx_ring: *mut RxDesc,
    tx_ring: *mut TxDesc,
    rx_buffers_phys: u32,
    tx_buffers_phys: u32,
    rx_buffers: *mut u8,
    tx_buffers: *mut u8,
    rx_head: AtomicUsize,
    tx_head: AtomicUsize,
    tx_packets: AtomicUsize,
    rx_packets: AtomicUsize,
    tx_stall_reported: AtomicBool,
    link_up: AtomicBool,
    initialized: AtomicBool,
}

unsafe impl Send for E1000e {}
unsafe impl Sync for E1000e {}

pub static NET: Mutex<Option<E1000e>> = Mutex::new(None);

fn alloc_dma(bytes: usize) -> Result<(u32, *mut u8), &'static str> {
    let pages = bytes.div_ceil(PAGE_SIZE);
    if pages == 0 {
        return Err("zero-sized DMA allocation");
    }
    let mut paging = unsafe { PAGING.lock() };
    let first = paging.alloc_contiguous_frames(pages as u32);
    let phys = first << 12;
    let virt_addr = phys
        .checked_add(KERNEL_OFFSET)
        .ok_or("DMA virtual address overflow")?;

    // DMA buffers rely on the permanent RAM direct map.  If some MMIO mapping
    // has replaced this VA, touching the buffer would hit device registers (or
    // another physical page) and turn a first TX into arbitrary corruption.
    if paging.dir.translate(virt_addr) != Some(phys) {
        return Err("DMA direct-map alias was overwritten");
    }

    let virt = virt_addr as *mut u8;
    unsafe { core::ptr::write_bytes(virt, 0, pages * PAGE_SIZE) };
    Ok((phys, virt))
}

#[inline]
fn dma_sync() {
    // PCIe DMA on the 82579 is cache-coherent. A global WBINVD here can race
    // descriptor writeback: four 16-byte RX descriptors share one cache line,
    // so writing that line back from the CPU can overwrite a neighboring
    // descriptor that the NIC just completed. We only need ordering before
    // publishing descriptors/tails to the device.
    compiler_fence(Ordering::SeqCst);
    unsafe { asm!("mfence", options(nostack, preserves_flags)) };
    compiler_fence(Ordering::SeqCst);
}

impl E1000e {
    pub fn init() -> Result<(), &'static str> {
        let dev = pci::find_device(VENDOR_INTEL, DEVICE_82579LM).ok_or("82579LM not found")?;
        println!(
            "e1000e: found {:02x}:{:02x}.{} rev {:02x} IRQ {}",
            dev.bus, dev.device, dev.function, dev.revision_id, dev.interrupt_line
        );
        dev.enable_bus_mastering();

        let (bar_phys, bar_size) = match dev.get_bar(0) {
            Some(pci::bar::Bar::Memory { address, size, .. }) => (*address, *size),
            _ => return Err("82579LM BAR0 is not MMIO"),
        };
        let fallback_mac = [
            0x02,
            0x80,
            0x86,
            dev.bus,
            dev.device,
            dev.function ^ dev.revision_id,
        ];
        if bar_size < 0x6000 {
            return Err("82579LM MMIO BAR is too small");
        }
        let mmio = map_mmio(bar_phys, bar_size)?;

        let (rx_ring_phys, rx_ring) = alloc_dma(core::mem::size_of::<RxDesc>() * RX_RING_SIZE)?;
        let (tx_ring_phys, tx_ring) =
            alloc_dma(core::mem::size_of::<TxDesc>() * E1000E_TX_RING_SIZE)?;
        let (rx_buffers_phys, rx_buffers) = alloc_dma(E1000E_RX_BUF_SIZE * RX_RING_SIZE)?;
        let (tx_buffers_phys, tx_buffers) = alloc_dma(TX_BUF_SIZE * E1000E_TX_RING_SIZE)?;

        let mut nic = Self {
            mmio,
            mac: [0; 6],
            rx_ring_phys,
            tx_ring_phys,
            rx_ring: rx_ring.cast(),
            tx_ring: tx_ring.cast(),
            rx_buffers_phys,
            tx_buffers_phys,
            rx_buffers,
            tx_buffers,
            rx_head: AtomicUsize::new(0),
            tx_head: AtomicUsize::new(0),
            tx_packets: AtomicUsize::new(0),
            rx_packets: AtomicUsize::new(0),
            tx_stall_reported: AtomicBool::new(false),
            link_up: AtomicBool::new(false),
            initialized: AtomicBool::new(false),
        };

        // Preserve the address installed by BIOS/PXE before taking ownership.
        let pre_reset_mac = nic.read_mac();
        nic.quiesce();
        nic.mac = nic.read_mac();
        if !valid_mac(nic.mac) {
            nic.mac = pre_reset_mac;
        }
        if !valid_mac(nic.mac) {
            nic.wait_lan_init();
            nic.mac = nic.read_mac();
        }
        if !valid_mac(nic.mac) {
            // Some PXE/ME combinations temporarily hide the flash-backed
            // address after ownership changes. A stable locally-administered
            // address keeps Ethernet usable until native PCH flash access is
            // implemented; it is explicitly marked non-global (02 prefix).
            nic.mac = fallback_mac;
            println!(
                "e1000e: RAL/RAH empty, using local MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                nic.mac[0], nic.mac[1], nic.mac[2], nic.mac[3], nic.mac[4], nic.mac[5]
            );
        }
        nic.program_mac();
        nic.setup_copper_link()?;
        nic.setup_rings();
        nic.start();
        let status = nic.read(REG_STATUS);
        nic.link_up
            .store(status & STATUS_LU != 0, Ordering::Release);
        nic.initialized.store(true, Ordering::Release);
        println!(
            "e1000e: ready MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} link={} status={:#010x}",
            nic.mac[0],
            nic.mac[1],
            nic.mac[2],
            nic.mac[3],
            nic.mac[4],
            nic.mac[5],
            if status & STATUS_LU != 0 {
                "up"
            } else {
                "negotiating"
            },
            status
        );
        println!(
            "e1000e: rings RX={:#010x} TX={:#010x} buffers RX={:#010x} TX={:#010x}",
            nic.rx_ring_phys, nic.tx_ring_phys, nic.rx_buffers_phys, nic.tx_buffers_phys
        );
        println!(
            "e1000e: regs CTRL={:#010x} RCTL={:#010x} TCTL={:#010x} RXDCTL={:#010x} TXDCTL0={:#010x} TXDCTL1={:#010x}",
            nic.read(REG_CTRL),
            nic.read(REG_RCTL),
            nic.read(REG_TCTL),
            nic.read(REG_RXDCTL),
            nic.read(REG_TXDCTL0),
            nic.read(REG_TXDCTL1)
        );
        println!(
            "e1000e: heads RDH={} RDT={} TDH={} TDT={}",
            nic.read(REG_RDH),
            nic.read(REG_RDT),
            nic.read(REG_TDH),
            nic.read(REG_TDT)
        );
        println!(
            "e1000e: hw rings RDBA={:#010x}:{:#010x} RDLEN={} TDBA={:#010x}:{:#010x} TDLEN={}",
            nic.read(REG_RDBAH),
            nic.read(REG_RDBAL),
            nic.read(REG_RDLEN),
            nic.read(REG_TDBAH),
            nic.read(REG_TDBAL),
            nic.read(REG_TDLEN)
        );
        *NET.lock() = Some(nic);
        Ok(())
    }

    #[inline]
    fn read(&self, register: usize) -> u32 {
        unsafe { read_volatile((self.mmio + register) as *const u32) }
    }

    /// 82579 + active ME firmware arbitration workaround from Linux e1000e.
    /// While FWSM.PCIM2PCI is set, ME owns the internal PCIm->PCI path and a
    /// CSR write can be lost/corrupted. Wait up to roughly 100 ms before each
    /// MMIO write when firmware is active.
    #[inline]
    fn prepare_mmio_write(&self) {
        if self.read(REG_FWSM) & FWSM_FW_VALID == 0 {
            return;
        }

        let mut remaining = FWSM_PCIM2PCI_COUNT;
        while self.read(REG_FWSM) & FWSM_PCIM2PCI != 0 && remaining > 1 {
            for _ in 0..50 {
                crate::time::microsleep();
            }
            remaining -= 1;
        }
    }

    #[inline]
    fn write(&self, register: usize, value: u32) {
        self.prepare_mmio_write();
        unsafe { write_volatile((self.mmio + register) as *mut u32, value) }
        let _ = self.read(REG_STATUS); // flush posted PCI write
    }

    fn quiesce(&self) {
        self.write(REG_IMC, u32::MAX);
        let _ = self.read(REG_ICR);

        // Clean takeover from PXE. Do not inherit PXE's MAC/DMA/descriptor
        // state: stop both units and let pending transactions drain before
        // issuing the PCH2 global MAC reset. Keep CTRL.PHY_RST clear so the
        // integrated PHY/autonegotiated link is not intentionally reset.
        self.write(REG_RCTL, 0);
        self.write(REG_TCTL, TCTL_PSP);
        for _ in 0..10_000 {
            crate::time::microsleep();
        }

        // Match e1000e_disable_pcie_master(): block new bus-master requests
        // and wait for outstanding requests to drain before resetting MAC.
        let mut ctrl = self.read(REG_CTRL) | CTRL_GIO_MASTER_DISABLE;
        self.write(REG_CTRL, ctrl);
        let mut master_drained = false;
        for _ in 0..800 {
            if self.read(REG_STATUS) & STATUS_GIO_MASTER_ENABLE == 0 {
                master_drained = true;
                break;
            }
            for _ in 0..100 {
                crate::time::microsleep();
            }
        }
        if !master_drained {
            println!(
                "e1000e: warning: PCIe master requests still pending STATUS={:#010x}",
                self.read(REG_STATUS)
            );
        }

        println!(
            "e1000e: PXE takeover: global MAC reset CTRL={:#010x} FWSM={:#010x}",
            ctrl,
            self.read(REG_FWSM)
        );

        // Linux's ich8lan reset path explicitly does NOT flush/read immediately
        // after CTRL.RST because that can hang this hardware. Bypass write(),
        // which normally performs a STATUS read to flush posted MMIO writes.
        ctrl &= !(CTRL_FRCSPD | CTRL_FRCDPX);
        ctrl |= CTRL_RST;
        self.prepare_mmio_write();
        unsafe { write_volatile((self.mmio + REG_CTRL) as *mut u32, ctrl) };
        for _ in 0..20_000 {
            crate::time::microsleep();
        }

        // 82579/PCH2-specific post-reset setting used by e1000e.
        let mut fextnvm3 = self.read(REG_FEXTNVM3);
        fextnvm3 &= !FEXTNVM3_PHY_CFG_COUNTER_MASK;
        fextnvm3 |= FEXTNVM3_PHY_CFG_COUNTER_50MSEC;
        self.write(REG_FEXTNVM3, fextnvm3);

        self.write(REG_IMC, u32::MAX);
        let _ = self.read(REG_ICR);

        // Claim driver ownership again after reset and leave speed/duplex to
        // PHY autonegotiation. start() will program every DMA/ring register.
        self.write(REG_CTRL_EXT, self.read(REG_CTRL_EXT) | CTRL_EXT_DRV_LOAD);
        let ctrl = (self.read(REG_CTRL) | CTRL_SLU) & !(CTRL_FRCSPD | CTRL_FRCDPX);
        self.write(REG_CTRL, ctrl);
        println!(
            "e1000e: PXE takeover complete CTRL={:#010x} STATUS={:#010x} RCTL={:#010x} TCTL={:#010x}",
            self.read(REG_CTRL),
            self.read(REG_STATUS),
            self.read(REG_RCTL),
            self.read(REG_TCTL)
        );
    }

    fn read_mac(&self) -> [u8; 6] {
        let low = self.read(REG_RAL0);
        let high = self.read(REG_RAH0);
        [
            low as u8,
            (low >> 8) as u8,
            (low >> 16) as u8,
            (low >> 24) as u8,
            high as u8,
            (high >> 8) as u8,
        ]
    }

    fn program_mac(&self) {
        let low = u32::from_le_bytes([self.mac[0], self.mac[1], self.mac[2], self.mac[3]]);
        let high = u16::from_le_bytes([self.mac[4], self.mac[5]]) as u32 | (1 << 31);
        self.write(REG_RAL0, low);
        self.write(REG_RAH0, high);
    }

    fn wait_lan_init(&self) {
        // 82579 loads its receive address and PHY configuration from the PCH
        // flash after global reset. RAL/RAH are not valid before this bit.
        for _ in 0..2000 {
            if self.read(REG_STATUS) & STATUS_LAN_INIT_DONE != 0 {
                return;
            }
            crate::time::microsleep();
        }
        println!(
            "e1000e: warning: LAN_INIT_DONE timeout, STATUS={:#010x}",
            self.read(REG_STATUS)
        );
    }

    fn mdic(&self, register: u8, data: u16, operation: u32) -> Result<u16, &'static str> {
        let command = data as u32 | ((register as u32) << 16) | (MDIC_PHY_ADDR << 21) | operation;
        self.write(REG_MDIC, command);
        for _ in 0..2000 {
            crate::time::microsleep();
            let value = self.read(REG_MDIC);
            if value & MDIC_READY != 0 {
                if value & MDIC_ERROR != 0 {
                    return Err("82579LM MDIC error");
                }
                return Ok(value as u16);
            }
        }
        Err("82579LM MDIC timeout")
    }

    fn phy_read(&self, register: u8) -> Result<u16, &'static str> {
        self.mdic(register, 0, MDIC_OP_READ)
    }

    fn phy_write(&self, register: u8, value: u16) -> Result<(), &'static str> {
        self.mdic(register, value, MDIC_OP_WRITE).map(|_| ())
    }

    fn setup_copper_link(&self) -> Result<(), &'static str> {
        if self.read(REG_STATUS) & STATUS_LU != 0 {
            println!("e1000e: preserving PXE PHY link");
            return Ok(());
        }
        let status = self.read(REG_STATUS);
        if status & STATUS_PHYRA != 0 {
            self.write(REG_STATUS, status & !STATUS_PHYRA);
        }

        // Do not force speed/duplex. Power up the integrated PHY and restart
        // IEEE autonegotiation, which also drives the physical link LEDs.
        let mut control = self.phy_read(PHY_CONTROL)?;
        control &= !(PHY_CTRL_POWER_DOWN | PHY_CTRL_ISOLATE);
        control |= PHY_CTRL_AUTONEG_ENABLE | PHY_CTRL_RESTART_AUTONEG;
        self.phy_write(PHY_CONTROL, control)?;
        let phy_status = self.phy_read(PHY_STATUS)?;
        println!(
            "e1000e: PHY control={:#06x} status={:#06x}",
            control, phy_status
        );
        Ok(())
    }

    fn setup_rings(&mut self) {
        unsafe {
            for i in 0..RX_RING_SIZE {
                let desc = self.rx_ring.add(i);
                write_volatile(
                    &mut (*desc).read,
                    RxDescRead {
                        buffer_addr: (self.rx_buffers_phys as usize + i * E1000E_RX_BUF_SIZE)
                            as u64,
                        reserved: 0,
                    },
                );
                // Diagnostic marker: if RX completion arrives but this pattern is
                // unchanged, the NIC did not DMA payload into the advertised buffer.
                core::ptr::write_bytes(self.rx_buffers.add(i * E1000E_RX_BUF_SIZE), 0xA5, 64);
            }
            for i in 0..E1000E_TX_RING_SIZE {
                let desc = self.tx_ring.add(i);
                write_volatile(
                    &mut (*desc).address,
                    (self.tx_buffers_phys as usize + i * TX_BUF_SIZE) as u64,
                );
                write_volatile(&mut (*desc).length, 0);
                write_volatile(&mut (*desc).checksum_offset, 0);
                write_volatile(&mut (*desc).command, 0);
                write_volatile(&mut (*desc).status, TXD_STAT_DD);
                write_volatile(&mut (*desc).checksum_start, 0);
                write_volatile(&mut (*desc).special, 0);
            }
        }
        dma_sync();
    }

    fn start(&self) {
        // Required PCH-generation initialization bits from Intel's ich8lan
        // path. In particular, disable relaxed ordering for DMA descriptors.
        self.write(
            REG_CTRL_EXT,
            self.read(REG_CTRL_EXT) | CTRL_EXT_DRV_LOAD | CTRL_EXT_RO_DIS | CTRL_EXT_PCH_REQUIRED,
        );
        // 82579/e1000e uses extended 16-byte RX descriptors for the normal
        // non-jumbo receive path. Packet split is a separate RCTL.DTYP mode;
        // keep DTYP=0 below and enable only extended descriptor writeback.
        self.write(
            REG_RFCTL,
            self.read(REG_RFCTL) | RFCTL_NFSW_DIS | RFCTL_NFSR_DIS | RFCTL_EXTEN,
        );
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

        self.write(REG_RDBAL, self.rx_ring_phys);
        self.write(REG_RDBAH, 0);
        self.write(
            REG_RDLEN,
            (core::mem::size_of::<RxDesc>() * RX_RING_SIZE) as u32,
        );
        self.write(REG_RDH, 0);
        self.write(REG_RDTR, 0);
        self.write(REG_RADV, 0);

        // PCH2 does not use Linux's FLAG2_DMA_BURST policy. In particular,
        // bit 25 is reserved here, not a queue-enable bit.
        self.write(REG_RXDCTL, RXDCTL_POLLING);

        // The PCIm2PCI arbiter erratum can also lose tail writes. Verify the
        // initial RDT while ME firmware is active instead of silently leaving
        // the receive ring empty.
        let wanted_rdt = (RX_RING_SIZE - 1) as u32;
        self.write(REG_RDT, wanted_rdt);
        let actual_rdt = self.read(REG_RDT);
        if actual_rdt != wanted_rdt {
            println!(
                "e1000e: RDT INIT WRITE LOST wanted={} actual={} FWSM={:#010x}",
                wanted_rdt,
                actual_rdt,
                self.read(REG_FWSM)
            );
        }

        self.write(REG_TDBAL, self.tx_ring_phys);
        self.write(REG_TDBAH, 0);
        self.write(
            REG_TDLEN,
            (core::mem::size_of::<TxDesc>() * E1000E_TX_RING_SIZE) as u32,
        );
        self.write(REG_TDH, 0);
        self.write(REG_TDT, 0);

        // Mirror Linux e1000e's ICH/PCH initialization.  The PCH2 hardware
        // wants full-descriptor writeback, descriptor-granularity prefetch,
        // COUNT_DESC, and the same TXDCTL policy on both queues.  Clear the
        // legacy low-water field as well so a stale bit 25 from the old Felix
        // QUEUE_ENABLE code cannot survive a warm reinitialization.
        let mut txdctl = self.read(REG_TXDCTL0);
        txdctl &= !(TXDCTL_PTHRESH | TXDCTL_WTHRESH | TXDCTL_LWTHRESH);
        txdctl |= TXDCTL_FULL_TX_DESC_WB | TXDCTL_MAX_TX_DESC_PREFETCH | TXDCTL_COUNT_DESC;
        self.write(REG_TXDCTL0, txdctl);
        self.write(REG_TXDCTL1, txdctl);

        // Preserve PCH/firmware-owned TCTL bits. Linux e1000e only replaces
        // the collision fields and adds PSP/RTLC here; Felix also has to set EN
        // because quiesce() explicitly disabled the transmitter above.
        let mut tctl = self.read(REG_TCTL);
        tctl &= !(TCTL_CT_MASK | TCTL_COLD_MASK);
        tctl |= TCTL_EN | TCTL_PSP | TCTL_RTLC | TCTL_CT | TCTL_COLD;
        self.write(REG_TCTL, tctl);
        self.write(
            REG_RCTL,
            RCTL_EN | RCTL_UPE | RCTL_MPE | RCTL_BAM | RCTL_SECRC,
        );
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    pub fn send(&self, data: &[u8]) -> Result<(), &'static str> {
        if !self.initialized.load(Ordering::Acquire) {
            return Err("not initialized");
        }
        if data.is_empty() || data.len() > TX_BUF_SIZE {
            return Err("frame too large");
        }
        let slot = self.tx_head.load(Ordering::Relaxed);
        let packet = self.tx_packets.fetch_add(1, Ordering::Relaxed);
        dma_sync();
        unsafe {
            let desc = &mut *self.tx_ring.add(slot);
            if read_volatile(&desc.status) & TXD_STAT_DD == 0 {
                if !self.tx_stall_reported.swap(true, Ordering::AcqRel) {
                    println!(
                        "e1000e: TX STALL packet={} slot={} TDH={} TDT={} desc_status={:#04x} STATUS={:#010x}",
                        packet,
                        slot,
                        self.read(REG_TDH),
                        self.read(REG_TDT),
                        read_volatile(&desc.status),
                        self.read(REG_STATUS)
                    );
                    println!(
                        "e1000e: TX STALL TDBA={:#010x}:{:#010x} TDLEN={} TXDCTL={:#010x} TCTL={:#010x} FWSM={:#010x}",
                        self.read(REG_TDBAH),
                        self.read(REG_TDBAL),
                        self.read(REG_TDLEN),
                        self.read(REG_TXDCTL0),
                        self.read(REG_TCTL),
                        self.read(REG_FWSM)
                    );
                    println!(
                        "e1000e: TX FIFO TDFH={} TDFT={} TDFHS={} TDFTS={} TDFPC={} TARC0={:#010x} TARC1={:#010x}",
                        self.read(REG_TDFH),
                        self.read(REG_TDFT),
                        self.read(REG_TDFHS),
                        self.read(REG_TDFTS),
                        self.read(REG_TDFPC),
                        self.read(REG_TARC0),
                        self.read(REG_TARC1)
                    );
                }
                return Err("TX ring full");
            }
            let len = data.len().max(60);
            let buffer = self.tx_buffers.add(slot * TX_BUF_SIZE);
            core::ptr::copy_nonoverlapping(data.as_ptr(), buffer, data.len());
            if data.len() < len {
                core::ptr::write_bytes(buffer.add(data.len()), 0, len - data.len());
            }
            write_volatile(&mut desc.length, len as u16);
            write_volatile(&mut desc.status, 0);
            write_volatile(&mut desc.command, TXD_CMD_EOP | TXD_CMD_IFCS | TXD_CMD_RS);
        }
        dma_sync();
        let next = (slot + 1) % E1000E_TX_RING_SIZE;
        self.tx_head.store(next, Ordering::Release);
        self.write(REG_TDT, next as u32);
        let actual_tdt = self.read(REG_TDT);
        if actual_tdt != next as u32 {
            println!(
                "e1000e: TDT WRITE LOST wanted={} actual={} FWSM={:#010x}",
                next,
                actual_tdt,
                self.read(REG_FWSM)
            );
            return Err("TDT write rejected by ME arbitration");
        }
        if packet < 8 {
            for _ in 0..200 {
                crate::time::microsleep();
            }
            dma_sync();
            let desc_status = unsafe { read_volatile(&(*self.tx_ring.add(slot)).status) };
            log_frame("TX", packet, data);
            let status = self.read(REG_STATUS);
            println!(
                "e1000e: TX ring slot={} DD={} TDH={} TDT={} link={} STATUS={:#010x} GPTC={} TPT={} TNCRS={} ECOL={} LATECOL={} TDFH={} TDFT={} TDFPC={}",
                slot,
                desc_status & TXD_STAT_DD != 0,
                self.read(REG_TDH),
                self.read(REG_TDT),
                status & STATUS_LU != 0,
                status,
                self.read(REG_GPTC),
                self.read(REG_TPT),
                self.read(REG_TNCRS),
                self.read(REG_ECOL),
                self.read(REG_LATECOL),
                self.read(REG_TDFH),
                self.read(REG_TDFT),
                self.read(REG_TDFPC)
            );
            self.log_rx_state();
        }
        Ok(())
    }

    fn log_rx_state(&self) {
        // Called only for the first eight transmissions. In the failing DHCP
        // case this captures RX progress across Discover retries without
        // flooding every empty poll. Counters below clear on read, so each
        // sample is an interval, not a lifetime total.
        let slot = self.rx_head.load(Ordering::Relaxed);
        let words = unsafe {
            let desc = self.rx_ring.add(slot).cast::<u32>();
            [
                read_volatile(desc),
                read_volatile(desc.add(1)),
                read_volatile(desc.add(2)),
                read_volatile(desc.add(3)),
            ]
        };
        println!(
            "e1000e: RX state sw={} RDH={} RDT={} completed={} desc={:08x}/{:08x}/{:08x}/{:08x}",
            slot,
            self.read(REG_RDH),
            self.read(REG_RDT),
            self.rx_packets.load(Ordering::Relaxed),
            words[0],
            words[1],
            words[2],
            words[3]
        );
        println!(
            "e1000e: RX counters (clear-on-read) GPRC={} TPR={} MPC={} RNBC={} CRC={} RXERR={}",
            self.read(REG_GPRC),
            self.read(REG_TPR),
            self.read(REG_MPC),
            self.read(REG_RNBC),
            self.read(REG_CRCERRS),
            self.read(REG_RXERRC)
        );
        println!(
            "e1000e: RX config RCTL={:#010x} RFCTL={:#010x} RXDCTL={:#010x} MRQC={:#010x} PBA={:#010x} MANC={:#010x} FWSM={:#010x}",
            self.read(REG_RCTL),
            self.read(REG_RFCTL),
            self.read(REG_RXDCTL),
            self.read(REG_MRQC),
            self.read(REG_PBA),
            self.read(REG_MANC),
            self.read(REG_FWSM)
        );
    }

    pub fn recv(&self, output: &mut [u8]) -> Option<usize> {
        if !self.initialized.load(Ordering::Acquire) {
            return None;
        }
        let status_reg = self.read(REG_STATUS);
        let link = status_reg & STATUS_LU != 0;
        if self.link_up.swap(link, Ordering::AcqRel) != link {
            println!(
                "e1000e: link changed: {} STATUS={:#010x}",
                if link { "up" } else { "down" },
                status_reg
            );
        }
        let slot = self.rx_head.load(Ordering::Relaxed);
        dma_sync();
        unsafe {
            let desc = self.rx_ring.add(slot);
            // DD publishes the rest of the descriptor and payload. Do not
            // snapshot the whole writeback before checking ownership: an
            // aggregate volatile load need not read its fields in DD order.
            let status_error = read_volatile(&raw const (*desc).wb.status_error);
            if status_error & RXDEXT_STATERR_DD == 0 {
                return None;
            }
            dma_sync();
            let length = read_volatile(&raw const (*desc).wb.length) as usize;
            let packet = self.rx_packets.fetch_add(1, Ordering::Relaxed);
            let valid = status_error & RXDEXT_STATERR_EOP != 0
                && status_error & RXDEXT_ERR_FRAME_ERR_MASK == 0
                && length >= 14
                && length <= E1000E_RX_BUF_SIZE
                && length <= output.len();

            if valid {
                core::ptr::copy_nonoverlapping(
                    self.rx_buffers.add(slot * E1000E_RX_BUF_SIZE),
                    output.as_mut_ptr(),
                    length,
                );
            }

            if packet < 8 {
                if valid {
                    log_frame("RX", packet, &output[..length]);
                    if output.len() >= 16 && output[12] == 0 && output[13] == 0 {
                        println!(
                            "e1000e: RX raw {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} expected={:#010x} stride={} staterr={:#010x} RXDCTL={:#010x} RCTL={:#010x} RFCTL={:#010x}",
                            output[0],
                            output[1],
                            output[2],
                            output[3],
                            output[4],
                            output[5],
                            output[6],
                            output[7],
                            output[8],
                            output[9],
                            output[10],
                            output[11],
                            output[12],
                            output[13],
                            output[14],
                            output[15],
                            self.rx_buffers_phys + (slot * E1000E_RX_BUF_SIZE) as u32,
                            E1000E_RX_BUF_SIZE,
                            status_error,
                            self.read(REG_RXDCTL),
                            self.read(REG_RCTL),
                            self.read(REG_RFCTL)
                        );
                    }
                }
                println!(
                    "e1000e: RX ring slot={} valid={} staterr={:#010x} len={} RDH={} RDT={}",
                    slot,
                    valid,
                    status_error,
                    length,
                    self.read(REG_RDH),
                    self.read(REG_RDT)
                );
            }

            // Extended RX writeback destroys the original buffer address.
            // Re-arm the slot with its DMA address before returning it to HW.
            write_volatile(
                &raw mut (*desc).read,
                RxDescRead {
                    buffer_addr: (self.rx_buffers_phys as usize + slot * E1000E_RX_BUF_SIZE) as u64,
                    reserved: 0,
                },
            );
            dma_sync();

            let next = (slot + 1) % RX_RING_SIZE;
            self.rx_head.store(next, Ordering::Release);

            // Return RX descriptors to hardware in batches. Linux e1000e does
            // the same (E1000_RX_BUFFER_WRITE == 16), and on 82579/PCH2 it
            // verifies RDT writes because ME/PCIm2PCI arbitration can corrupt
            // tail updates. Do not wrap RDT 127 -> 0 after every single packet.
            if next % 16 == 0 {
                let wanted_rdt = slot as u32;
                self.write(REG_RDT, wanted_rdt);
                let actual_rdt = self.read(REG_RDT);
                if actual_rdt != wanted_rdt {
                    println!(
                        "e1000e: RDT WRITE LOST wanted={} actual={} RDH={} FWSM={:#010x}",
                        wanted_rdt,
                        actual_rdt,
                        self.read(REG_RDH),
                        self.read(REG_FWSM)
                    );
                } else if packet < 32 {
                    println!(
                        "e1000e: RX returned batch tail={} RDH={}",
                        actual_rdt,
                        self.read(REG_RDH)
                    );
                }
            }
            valid.then_some(length)
        }
    }
}

fn valid_mac(mac: [u8; 6]) -> bool {
    mac != [0; 6] && mac != [0xff; 6] && mac[0] & 1 == 0
}

fn log_frame(direction: &str, number: usize, frame: &[u8]) {
    if frame.len() < 14 {
        println!("e1000e: {}#{} len={} runt", direction, number, frame.len());
        return;
    }
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    if ethertype == 0x0800 && frame.len() >= 42 && frame[23] == 17 {
        let ihl = ((frame[14] & 0x0f) as usize) * 4;
        let udp = 14 + ihl;
        if frame.len() >= udp + 4 {
            let src = u16::from_be_bytes([frame[udp], frame[udp + 1]]);
            let dst = u16::from_be_bytes([frame[udp + 2], frame[udp + 3]]);
            println!(
                "e1000e: {}#{} len={} eth=IPv4 UDP {}->{}",
                direction,
                number,
                frame.len(),
                src,
                dst
            );
            return;
        }
    }
    println!(
        "e1000e: {}#{} len={} eth={:#06x}",
        direction,
        number,
        frame.len(),
        ethertype
    );
}
