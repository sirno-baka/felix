//! Minimal Intel 82579LM (PCH2 / e1000e family) Ethernet driver.
//!
//! The first implementation intentionally uses polling,
//! extended RX / legacy TX descriptors and no checksum/VLAN offload.
//! The 82579 PHY and firmware retain responsibility
//! for copper autonegotiation; Felix owns the MAC DMA rings.

use core::arch::asm;
use core::fmt;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{compiler_fence, AtomicBool, AtomicU32, AtomicU8, AtomicUsize, Ordering};

use crate::drivers::net::{map_mmio, RX_RING_SIZE, TX_BUF_SIZE};
use crate::memory::resources::{DmaBuffer as DmaAllocation, MmioMapping, dma_alloc_for};
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
const REG_EXTCNF_CTRL: usize = 0x0f00;
const REG_ICR: usize = 0x00c0;
const REG_IMS: usize = 0x00d0;
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

const ICR_TXDW: u32 = 1 << 0;
const ICR_LSC: u32 = 1 << 2;
const ICR_RXDMT0: u32 = 1 << 4;
const ICR_RXO: u32 = 1 << 6;
const ICR_RXT0: u32 = 1 << 7;
const IRQ_MASK: u32 = ICR_TXDW | ICR_LSC | ICR_RXDMT0 | ICR_RXO | ICR_RXT0;

const MDIC_PHY_ADDR: u32 = 1;
const MDIC_OP_WRITE: u32 = 1 << 26;
const MDIC_OP_READ: u32 = 2 << 26;
const MDIC_READY: u32 = 1 << 28;
const MDIC_ERROR: u32 = 1 << 30;
const PHY_CONTROL: u8 = 0;
const PHY_STATUS: u8 = 1;
const PHY_PAGE_SELECT: u8 = 0x1f;
const PHY_CTRL_RESTART_AUTONEG: u16 = 1 << 9;
const PHY_CTRL_ISOLATE: u16 = 1 << 10;
const PHY_CTRL_POWER_DOWN: u16 = 1 << 11;
const PHY_CTRL_AUTONEG_ENABLE: u16 = 1 << 12;

// 82579/PCH2 PHY workarounds mirrored from Linux e1000e's pch2lan path.
const HV_KMRN_MODE_CTRL_PAGE: u16 = 769;
const HV_KMRN_MODE_CTRL_REG: u8 = 16;
const HV_KMRN_MDIO_SLOW: u16 = 0x0400;
const HV_PM_CTRL_PAGE: u16 = 770;
const HV_PM_CTRL_REG: u8 = 17;
const HV_PM_CTRL_K1_ENABLE: u16 = 0x4000;
const HV_M_STATUS: u8 = 26;
const HV_M_STATUS_LINK_UP: u16 = 0x0040;
const HV_M_STATUS_AUTONEG_COMPLETE: u16 = 0x1000;
const HV_M_STATUS_SPEED_MASK: u16 = 0x0300;
const HV_M_STATUS_SPEED_100: u16 = 0x0100;
const HV_M_STATUS_SPEED_1000: u16 = 0x0200;

const I82579_LPI_CTRL_PAGE: u16 = 772;
const I82579_LPI_CTRL_REG: u8 = 20;
const I82579_LPI_CTRL_ENABLE_MASK: u16 = 0x6000;
const I82579_EMI_ADDR: u8 = 0x10;
const I82579_EMI_DATA: u8 = 0x11;
const I82579_MSE_THRESHOLD: u16 = 0x084f;
const I82579_MSE_LINK_DOWN: u16 = 0x2411;
const I82579_LPI_PLL_SHUT: u16 = 0x4412;
const I82579_EEE_PCS_STATUS: u16 = 0x182e;
const I82579_LPI_100_PLL_SHUT: u16 = 1 << 2;

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
const EXTCNF_CTRL_SWFLAG: u32 = 0x0000_0020;
const EXTCNF_CTRL_GATE_PHY_CFG: u32 = 0x0000_0080;
const PHY_CFG_TIMEOUT_MS: usize = 100;
const PHY_SWFLAG_TIMEOUT_MS: usize = 1000;
const MDIC_POLL_COUNT: usize = 640 * 3;
const LAN_INIT_POLL_COUNT: usize = 1500;
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
    _mmio_mapping: MmioMapping,
    irq: u8,
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
    tx_packets: AtomicUsize,
    rx_packets: AtomicUsize,
    tcp_last_rx_ms: AtomicU32,
    tcp_last_rx_seq: AtomicU32,
    tcp_last_rx_ack: AtomicU32,
    tcp_last_rx_meta: AtomicU32,
    tcp_last_rx_ports: AtomicU32,
    tcp_last_tx_ack_ms: AtomicU32,
    tcp_last_tx_ack: AtomicU32,
    tcp_last_tx_ack_meta: AtomicU32,
    tcp_last_tx_ack_ports: AtomicU32,
    tx_stall_reported: AtomicBool,
    link_up: AtomicBool,
    initialized: AtomicBool,
}

unsafe impl Send for E1000e {}
unsafe impl Sync for E1000e {}

impl Drop for E1000e {
    fn drop(&mut self) {
        self.initialized.store(false, Ordering::Release);
        self.shutdown_dma();
    }
}

pub struct DebugSnapshot;

impl fmt::Display for DebugSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // During bring-up the device temporarily lives in NET, but normal
        // operation moves it into NetStack.  Looking only at NET made the F12
        // dump claim "not initialized" precisely while the NIC was active.
        if let Some(guard) = NET.try_lock() {
            if let Some(nic) = guard.as_ref() {
                return nic.fmt_debug(f);
            }
        }

        let Some(stack_guard) = crate::net::stack::NET_STACK.try_lock() else {
            return writeln!(f, "e1000e: network stack lock busy");
        };
        let Some(stack) = stack_guard.as_ref() else {
            return writeln!(f, "e1000e: not initialized");
        };
        let crate::drivers::net::AnyNic::E1000e(nic) = &stack.device else {
            return writeln!(f, "e1000e: not active");
        };

        nic.fmt_debug(f)?;

        // F12 also shows the smoltcp queues behind each Felix socket id. This
        // distinguishes "packet reached TCP and is waiting for userspace" from
        // a driver/RX loss without adding hot-path logging that changes timing.
        for (index, mapping) in stack.handles.iter().enumerate() {
            let Some((handle, is_tcp)) = mapping else {
                continue;
            };
            if !*is_tcp {
                continue;
            }
            let socket = stack.sockets.get::<smoltcp::socket::tcp::Socket>(*handle);
            writeln!(
                f,
                "tcp[{}]: state={:?} local={:?} remote={:?} can_recv={} may_recv={} rxq={}/{} can_send={} may_send={} txq={}/{}",
                index + 1,
                socket.state(),
                socket.local_endpoint(),
                socket.remote_endpoint(),
                socket.can_recv(),
                socket.may_recv(),
                socket.recv_queue(),
                socket.recv_capacity(),
                socket.can_send(),
                socket.may_send(),
                socket.send_queue(),
                socket.send_capacity(),
            )?;
        }
        Ok(())
    }
}

impl E1000e {
    fn track_tcp_frame(&self, tx: bool, frame: &[u8]) {
        if frame.len() < 14 + 20 || u16::from_be_bytes([frame[12], frame[13]]) != 0x0800 {
            return;
        }
        let ip = 14usize;
        let ihl = ((frame[ip] & 0x0f) as usize) * 4;
        if ihl < 20 || frame.len() < ip + ihl || frame[ip + 9] != 6 {
            return;
        }
        let tcp = ip + ihl;
        if frame.len() < tcp + 20 {
            return;
        }
        let tcp_header_len = ((frame[tcp + 12] >> 4) as usize) * 4;
        if tcp_header_len < 20 || frame.len() < tcp + tcp_header_len {
            return;
        }
        let total_len = u16::from_be_bytes([frame[ip + 2], frame[ip + 3]]) as usize;
        let payload_len = total_len.saturating_sub(ihl + tcp_header_len).min(u16::MAX as usize);
        let flags = frame[tcp + 13];
        let seq = u32::from_be_bytes([frame[tcp + 4], frame[tcp + 5], frame[tcp + 6], frame[tcp + 7]]);
        let ack = u32::from_be_bytes([frame[tcp + 8], frame[tcp + 9], frame[tcp + 10], frame[tcp + 11]]);
        let window = u16::from_be_bytes([frame[tcp + 14], frame[tcp + 15]]);
        let src_port = u16::from_be_bytes([frame[tcp], frame[tcp + 1]]);
        let dst_port = u16::from_be_bytes([frame[tcp + 2], frame[tcp + 3]]);
        let ports = ((src_port as u32) << 16) | dst_port as u32;
        let now = crate::time::uptime_ms() as u32;

        if !tx && payload_len != 0 {
            self.tcp_last_rx_ms.store(now, Ordering::Relaxed);
            self.tcp_last_rx_seq.store(seq, Ordering::Relaxed);
            self.tcp_last_rx_ack.store(ack, Ordering::Relaxed);
            self.tcp_last_rx_meta.store(((payload_len as u32) << 16) | window as u32, Ordering::Relaxed);
            self.tcp_last_rx_ports.store(ports, Ordering::Relaxed);
        } else if tx && payload_len == 0 && flags & 0x10 != 0 && flags & 0x07 == 0 {
            // Pure ACK/window update. These are intentionally omitted from the
            // verbose packet log, but are exactly what matters when a remote
            // sender pauses for tens of seconds.
            self.tcp_last_tx_ack_ms.store(now, Ordering::Relaxed);
            self.tcp_last_tx_ack.store(ack, Ordering::Relaxed);
            self.tcp_last_tx_ack_meta.store(window as u32, Ordering::Relaxed);
            self.tcp_last_tx_ack_ports.store(ports, Ordering::Relaxed);
        }
    }

    fn fmt_debug(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let nic = self;

        let slot = nic.rx_head.load(Ordering::Relaxed) % RX_RING_SIZE;
        let tx_head = nic.tx_head.load(Ordering::Relaxed);
        let tx_next = (tx_head + 1) % E1000E_TX_RING_SIZE;
        let tx_status = unsafe { read_volatile(&(*nic.tx_ring.add(tx_head)).status) };
        let tx_next_status = unsafe { read_volatile(&(*nic.tx_ring.add(tx_next)).status) };
        let (d0, d1, d2, d3) = unsafe {
            let desc = nic.rx_ring.add(slot).cast::<u32>();
            (
                read_volatile(desc),
                read_volatile(desc.add(1)),
                read_volatile(desc.add(2)),
                read_volatile(desc.add(3)),
            )
        };
        writeln!(
            f,
            "e1000e: STATUS={:#010x} link={} init={} RX sw={} RDH={} RDT={} TX sw={} ready={} desc={:#04x} next={:#04x} TDH={} TDT={}",
            nic.read(REG_STATUS),
            nic.link_up.load(Ordering::Relaxed),
            nic.initialized.load(Ordering::Relaxed),
            slot,
            nic.read(REG_RDH),
            nic.read(REG_RDT),
            tx_head,
            tx_status & TXD_STAT_DD != 0 && tx_next_status & TXD_STAT_DD != 0,
            tx_status,
            tx_next_status,
            nic.read(REG_TDH),
            nic.read(REG_TDT),
        )?;
        writeln!(
            f,
            "e1000e: RX desc[{}]={:08x}/{:08x}/{:08x}/{:08x} sw_rx={} sw_tx={}",
            slot,
            d0,
            d1,
            d2,
            d3,
            nic.rx_packets.load(Ordering::Relaxed),
            nic.tx_packets.load(Ordering::Relaxed),
        )?;
        writeln!(
            f,
            "e1000e: RX hw GPRC={} TPR={} MPC={} RNBC={} CRC={} RXERR={}",
            nic.read(REG_GPRC),
            nic.read(REG_TPR),
            nic.read(REG_MPC),
            nic.read(REG_RNBC),
            nic.read(REG_CRCERRS),
            nic.read(REG_RXERRC),
        )?;
        writeln!(
            f,
            "e1000e: RX cfg RCTL={:#010x} RXDCTL={:#010x} RFCTL={:#010x} PBA={:#010x} FWSM={:#010x}",
            nic.read(REG_RCTL),
            nic.read(REG_RXDCTL),
            nic.read(REG_RFCTL),
            nic.read(REG_PBA),
            nic.read(REG_FWSM),
        )?;

        let now = crate::time::uptime_ms() as u32;
        let rx_ms = nic.tcp_last_rx_ms.load(Ordering::Relaxed);
        let rx_meta = nic.tcp_last_rx_meta.load(Ordering::Relaxed);
        let rx_ports = nic.tcp_last_rx_ports.load(Ordering::Relaxed);
        let tx_ms = nic.tcp_last_tx_ack_ms.load(Ordering::Relaxed);
        let tx_meta = nic.tcp_last_tx_ack_meta.load(Ordering::Relaxed);
        let tx_ports = nic.tcp_last_tx_ack_ports.load(Ordering::Relaxed);
        writeln!(
            f,
            "tcp-flow: RX age={}ms {}->{} seq={} ack={} win={} payload={} | TXACK age={}ms {}->{} ack={} win={}",
            if rx_ms == 0 { u32::MAX } else { now.wrapping_sub(rx_ms) },
            (rx_ports >> 16) as u16,
            rx_ports as u16,
            nic.tcp_last_rx_seq.load(Ordering::Relaxed),
            nic.tcp_last_rx_ack.load(Ordering::Relaxed),
            rx_meta as u16,
            (rx_meta >> 16) as u16,
            if tx_ms == 0 { u32::MAX } else { now.wrapping_sub(tx_ms) },
            (tx_ports >> 16) as u16,
            tx_ports as u16,
            nic.tcp_last_tx_ack.load(Ordering::Relaxed),
            tx_meta as u16,
        )
    }
}

pub static NET: Mutex<Option<E1000e>> = Mutex::new(None);
static IRQ_MMIO: AtomicUsize = AtomicUsize::new(0);
static IRQ_LINE: AtomicU8 = AtomicU8::new(u8::MAX);

fn alloc_dma(owner: &'static str, bytes: usize) -> Result<DmaAllocation, &'static str> {
    dma_alloc_for(owner, bytes, 4096, u32::MAX as u64)
        .map_err(|_| "e1000e DMA allocation failed")
}

#[inline]
fn dma_sync() {
    crate::memory::resources::dma_mb();
}

// The legacy time::microsleep() helper performs 40 port-0x80 waits and is
// intentionally much longer than one hardware I/O delay. Intel's e1000e
// timings are expressed in real microseconds, so using microsleep() here can
// inflate 10-100 ms hardware waits into seconds. One port-0x80 access is
// roughly 1-4 us on the target class of x86 hardware; use 32 waits per 100 us
// as a conservative approximation without depending on PIT/timer interrupts.
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
        let mmio_mapping = map_mmio(bar_phys, bar_size)?;
        let mmio = mmio_mapping.as_usize();

        let rx_ring_dma = alloc_dma("e1000e RX ring", core::mem::size_of::<RxDesc>() * RX_RING_SIZE)?;
        let tx_ring_dma = alloc_dma("e1000e TX ring", core::mem::size_of::<TxDesc>() * E1000E_TX_RING_SIZE)?;
        let rx_buffers_dma = alloc_dma("e1000e RX buffers", E1000E_RX_BUF_SIZE * RX_RING_SIZE)?;
        let tx_buffers_dma = alloc_dma("e1000e TX buffers", TX_BUF_SIZE * E1000E_TX_RING_SIZE)?;
        let rx_ring_phys = rx_ring_dma.phys.0;
        let tx_ring_phys = tx_ring_dma.phys.0;
        let rx_buffers_phys = rx_buffers_dma.phys.0;
        let tx_buffers_phys = tx_buffers_dma.phys.0;
        let rx_ring = rx_ring_dma.as_mut_ptr();
        let tx_ring = tx_ring_dma.as_mut_ptr();
        let rx_buffers = rx_buffers_dma.as_mut_ptr();
        let tx_buffers = tx_buffers_dma.as_mut_ptr();

        let mut nic = Self {
            mmio,
            _mmio_mapping: mmio_mapping,
            irq: dev.interrupt_line,
            mac: [0; 6],
            rx_ring_phys,
            tx_ring_phys,
            rx_ring: rx_ring.cast(),
            tx_ring: tx_ring.cast(),
            rx_buffers_phys,
            tx_buffers_phys,
            rx_buffers,
            tx_buffers,
            _rx_ring_dma: rx_ring_dma,
            _tx_ring_dma: tx_ring_dma,
            _rx_buffers_dma: rx_buffers_dma,
            _tx_buffers_dma: tx_buffers_dma,
            rx_head: AtomicUsize::new(0),
            tx_head: AtomicUsize::new(0),
            tx_packets: AtomicUsize::new(0),
            rx_packets: AtomicUsize::new(0),
            tcp_last_rx_ms: AtomicU32::new(0),
            tcp_last_rx_seq: AtomicU32::new(0),
            tcp_last_rx_ack: AtomicU32::new(0),
            tcp_last_rx_meta: AtomicU32::new(0),
            tcp_last_rx_ports: AtomicU32::new(0),
            tcp_last_tx_ack_ms: AtomicU32::new(0),
            tcp_last_tx_ack: AtomicU32::new(0),
            tcp_last_tx_ack_meta: AtomicU32::new(0),
            tcp_last_tx_ack_ports: AtomicU32::new(0),
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
            // println!(
            //     "e1000e: RAL/RAH empty, using local MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            //     nic.mac[0], nic.mac[1], nic.mac[2], nic.mac[3], nic.mac[4], nic.mac[5]
            // );
        }
        nic.program_mac();
        nic.setup_copper_link()?;
        nic.setup_rings();
        nic.start();
        let status = nic.read(REG_STATUS);
        if status & STATUS_LU != 0 {
            if let Err(err) = nic.apply_pch2_k1_workaround() {
                println!("e1000e: 82579 K1 workaround failed after init: {}", err);
            }
        }
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
        // println!(
        //     "e1000e: rings RX={:#010x} TX={:#010x} buffers RX={:#010x} TX={:#010x}",
        //     nic.rx_ring_phys, nic.tx_ring_phys, nic.rx_buffers_phys, nic.tx_buffers_phys
        // );
        // println!(
        //     "e1000e: regs CTRL={:#010x} RCTL={:#010x} TCTL={:#010x} RXDCTL={:#010x} TXDCTL0={:#010x} TXDCTL1={:#010x}",
        //     nic.read(REG_CTRL),
        //     nic.read(REG_RCTL),
        //     nic.read(REG_TCTL),
        //     nic.read(REG_RXDCTL),
        //     nic.read(REG_TXDCTL0),
        //     nic.read(REG_TXDCTL1)
        // );
        // println!(
        //     "e1000e: heads RDH={} RDT={} TDH={} TDT={}",
        //     nic.read(REG_RDH),
        //     nic.read(REG_RDT),
        //     nic.read(REG_TDH),
        //     nic.read(REG_TDT)
        // );
        // println!(
        //     "e1000e: hw rings RDBA={:#010x}:{:#010x} RDLEN={} TDBA={:#010x}:{:#010x} TDLEN={}",
        //     nic.read(REG_RDBAH),
        //     nic.read(REG_RDBAL),
        //     nic.read(REG_RDLEN),
        //     nic.read(REG_TDBAH),
        //     nic.read(REG_TDBAL),
        //     nic.read(REG_TDLEN)
        // );
        *NET.lock() = Some(nic);
        IRQ_MMIO.store(mmio, Ordering::Release);
        IRQ_LINE.store(dev.interrupt_line, Ordering::Release);
        if crate::drivers::shared_irq::register_named(dev.interrupt_line, irq_entry, "e1000e").is_ok() {
            dev.write_u16(0x04, dev.read_u16(0x04) & !0x0400);
            if let Some(nic) = NET.lock().as_ref() {
                nic.set_irq_enabled(true);
            }
        } else {
            println!("e1000e: IRQ{} unavailable, polling fallback", dev.interrupt_line);
        }
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
            // Linux e1000e uses udelay(50) here.
            hw_delay_us(50);
            remaining -= 1;
        }
    }

    #[inline]
    fn write(&self, register: usize, value: u32) {
        self.prepare_mmio_write();
        unsafe { write_volatile((self.mmio + register) as *mut u32, value) }
        let _ = self.read(REG_STATUS); // flush posted PCI write
    }

    fn set_irq_enabled(&self, enabled: bool) {
        self.write(REG_IMC, u32::MAX);
        let _ = self.read(REG_ICR);
        if enabled {
            self.write(REG_IMS, IRQ_MASK);
        }
    }

    fn shutdown_dma(&self) {
        self.set_irq_enabled(false);
        self.write(REG_RCTL, 0);
        self.write(REG_TCTL, self.read(REG_TCTL) & !TCTL_EN);

        // Stop issuing new PCIe bus-master transactions and wait for any
        // outstanding DMA to drain before descriptor/buffer memory can drop.
        self.write(REG_CTRL, self.read(REG_CTRL) | CTRL_GIO_MASTER_DISABLE);
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
                "e1000e: warning: DMA shutdown with PCIe master requests pending STATUS={:#010x}",
                self.read(REG_STATUS)
            );
        }
        crate::memory::resources::dma_mb();
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
        // Linux e1000e: usleep_range(10000, 11000).
        hw_delay_ms(10);

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
            // MASTER_DISABLE_TIMEOUT is 800 iterations of 100 us in e1000e.
            hw_delay_us(100);
        }
        if !master_drained {
            println!(
                "e1000e: warning: PCIe master requests still pending STATUS={:#010x}",
                self.read(REG_STATUS)
            );
        }

        // println!(
        //     "e1000e: PXE takeover: global MAC reset CTRL={:#010x} FWSM={:#010x}",
        //     ctrl,
        //     self.read(REG_FWSM)
        // );

        // Linux's ich8lan reset path explicitly does NOT flush/read immediately
        // after CTRL.RST because that can hang this hardware. Bypass write(),
        // which normally performs a STATUS read to flush posted MMIO writes.
        ctrl &= !(CTRL_FRCSPD | CTRL_FRCDPX);
        ctrl |= CTRL_RST;
        self.prepare_mmio_write();
        unsafe { write_volatile((self.mmio + REG_CTRL) as *mut u32, ctrl) };
        // Linux ich8lan reset waits 20 ms and deliberately does not flush CTRL.RST.
        hw_delay_ms(20);

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
        // println!(
        //     "e1000e: PXE takeover complete CTRL={:#010x} STATUS={:#010x} RCTL={:#010x} TCTL={:#010x}",
        //     self.read(REG_CTRL),
        //     self.read(REG_STATUS),
        //     self.read(REG_RCTL),
        //     self.read(REG_TCTL)
        // );
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
        for _ in 0..LAN_INIT_POLL_COUNT {
            if self.read(REG_STATUS) & STATUS_LAN_INIT_DONE != 0 {
                return;
            }
            // E1000_ICH8_LAN_INIT_TIMEOUT=1500, 100-200 us per iteration.
            hw_delay_us(100);
        }
        // println!(
        //     "e1000e: warning: LAN_INIT_DONE timeout, STATUS={:#010x}",
        //     self.read(REG_STATUS)
        // );
    }

    /// Serialize PCH PHY/EMI accesses with firmware/ME. Linux e1000e uses
    /// EXTCNF_CTRL.SWFLAG for the same purpose on ICH/PCH parts. Without it,
    /// a page-select + data transaction can race firmware and hit a different
    /// PHY page, which makes link behaviour timing/order dependent.
    fn acquire_phy_swflag(&self) -> Result<(), &'static str> {
        // Match e1000_acquire_swflag_ich8lan(): first allow an existing
        // software owner up to PHY_CFG_TIMEOUT (100 ms) to release the flag.
        let mut extcnf = self.read(REG_EXTCNF_CTRL);
        for _ in 0..PHY_CFG_TIMEOUT_MS {
            if extcnf & EXTCNF_CTRL_SWFLAG == 0 {
                break;
            }
            hw_delay_ms(1);
            extcnf = self.read(REG_EXTCNF_CTRL);
        }
        if extcnf & EXTCNF_CTRL_SWFLAG != 0 {
            return Err("82579LM PHY SWFLAG already owned");
        }

        extcnf |= EXTCNF_CTRL_SWFLAG;
        self.write(REG_EXTCNF_CTRL, extcnf);

        // Firmware/hardware may arbitrate this bit. Linux allows up to 1 s,
        // polling once per millisecond after requesting ownership.
        for _ in 0..PHY_SWFLAG_TIMEOUT_MS {
            if self.read(REG_EXTCNF_CTRL) & EXTCNF_CTRL_SWFLAG != 0 {
                return Ok(());
            }
            hw_delay_ms(1);
        }

        let extcnf = self.read(REG_EXTCNF_CTRL);
        self.write(REG_EXTCNF_CTRL, extcnf & !EXTCNF_CTRL_SWFLAG);
        Err("82579LM PHY SWFLAG timeout")
    }

    fn release_phy_swflag(&self) {
        let extcnf = self.read(REG_EXTCNF_CTRL);
        if extcnf & EXTCNF_CTRL_SWFLAG != 0 {
            self.write(REG_EXTCNF_CTRL, extcnf & !EXTCNF_CTRL_SWFLAG);
        }
    }

    fn gate_hw_phy_config(&self, gate: bool) {
        let mut extcnf = self.read(REG_EXTCNF_CTRL);
        if gate {
            extcnf |= EXTCNF_CTRL_GATE_PHY_CFG;
        } else {
            extcnf &= !EXTCNF_CTRL_GATE_PHY_CFG;
        }
        self.write(REG_EXTCNF_CTRL, extcnf);
    }

    fn mdic(&self, register: u8, data: u16, operation: u32) -> Result<u16, &'static str> {
        let command = data as u32 | ((register as u32) << 16) | (MDIC_PHY_ADDR << 21) | operation;
        self.write(REG_MDIC, command);
        for _ in 0..MDIC_POLL_COUNT {
            // e1000e polls MDIC every 50 us, up to GEN_POLL_TIMEOUT * 3.
            hw_delay_us(50);
            let value = self.read(REG_MDIC);
            if value & MDIC_READY != 0 {
                // 82579/PCH2 requires 100 us after each MDIC transaction to
                // avoid returning duplicate data on the following access.
                hw_delay_us(100);
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

    fn phy_set_page(&self, page: u16) -> Result<(), &'static str> {
        // HV PHY page select expects page * 32, exactly like Linux's
        // e1000_read/write_phy_reg_hv helpers.
        self.phy_write(PHY_PAGE_SELECT, page << 5)
    }

    fn phy_read_paged(&self, page: u16, register: u8) -> Result<u16, &'static str> {
        self.phy_set_page(page)?;
        let result = self.phy_read(register & 0x1f);
        let restore = self.phy_set_page(0);
        match (result, restore) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(err), _) => Err(err),
            (Ok(_), Err(err)) => Err(err),
        }
    }

    fn phy_write_paged(
        &self,
        page: u16,
        register: u8,
        value: u16,
    ) -> Result<(), &'static str> {
        self.phy_set_page(page)?;
        let result = self.phy_write(register & 0x1f, value);
        let restore = self.phy_set_page(0);
        match (result, restore) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(err), _) => Err(err),
            (Ok(()), Err(err)) => Err(err),
        }
    }

    fn emi_read(&self, address: u16) -> Result<u16, &'static str> {
        self.phy_set_page(0)?;
        self.phy_write(I82579_EMI_ADDR, address)?;
        self.phy_read(I82579_EMI_DATA)
    }

    fn emi_write(&self, address: u16, value: u16) -> Result<(), &'static str> {
        self.phy_set_page(0)?;
        self.phy_write(I82579_EMI_ADDR, address)?;
        self.phy_write(I82579_EMI_DATA, value)
    }

    fn apply_pch2_phy_workarounds(&self) -> Result<(), &'static str> {
        // Linux applies this immediately after every 82579 PHY reset: use
        // slow MDIO before further accesses, then relax the MSE threshold so
        // transient noise does not flap the link.
        let kmrn = self.phy_read_paged(HV_KMRN_MODE_CTRL_PAGE, HV_KMRN_MODE_CTRL_REG)?;
        self.phy_write_paged(
            HV_KMRN_MODE_CTRL_PAGE,
            HV_KMRN_MODE_CTRL_REG,
            kmrn | HV_KMRN_MDIO_SLOW,
        )?;
        self.emi_write(I82579_MSE_THRESHOLD, 0x0034)?;
        self.emi_write(I82579_MSE_LINK_DOWN, 0x0005)?;

        // Felix does not yet implement Linux's delayed EEE state machine.
        // Do not inherit PXE/firmware LPI state: keep EEE disabled so 82579
        // cannot enter LPI too early after link-up.
        let lpi = self.phy_read_paged(I82579_LPI_CTRL_PAGE, I82579_LPI_CTRL_REG)?;
        self.phy_write_paged(
            I82579_LPI_CTRL_PAGE,
            I82579_LPI_CTRL_REG,
            lpi & !I82579_LPI_CTRL_ENABLE_MASK,
        )?;
        let pll = self.emi_read(I82579_LPI_PLL_SHUT)?;
        self.emi_write(I82579_LPI_PLL_SHUT, pll & !I82579_LPI_100_PLL_SHUT)?;
        let _ = self.emi_read(I82579_EEE_PCS_STATUS)?; // read/clear LPI status

        println!("e1000e: 82579 PHY workarounds: MDIO slow, MSE tuned, EEE disabled");
        Ok(())
    }

    fn apply_pch2_k1_workaround_locked(&self) -> Result<(), &'static str> {
        self.phy_set_page(0)?;
        let status = self.phy_read(HV_M_STATUS)?;
        let ready = HV_M_STATUS_LINK_UP | HV_M_STATUS_AUTONEG_COMPLETE;
        if status & ready != ready {
            return Ok(());
        }

        let speed = status & HV_M_STATUS_SPEED_MASK;
        if speed == HV_M_STATUS_SPEED_100 || speed == HV_M_STATUS_SPEED_1000 {
            let pm = self.phy_read_paged(HV_PM_CTRL_PAGE, HV_PM_CTRL_REG)?;
            if pm & HV_PM_CTRL_K1_ENABLE != 0 {
                self.phy_write_paged(
                    HV_PM_CTRL_PAGE,
                    HV_PM_CTRL_REG,
                    pm & !HV_PM_CTRL_K1_ENABLE,
                )?;
                println!(
                    "e1000e: 82579 K1 disabled at {} Mbps (packet-drop workaround)",
                    if speed == HV_M_STATUS_SPEED_1000 { 1000 } else { 100 }
                );
            }
        }
        Ok(())
    }

    fn apply_pch2_k1_workaround(&self) -> Result<(), &'static str> {
        self.acquire_phy_swflag()?;
        let result = self.apply_pch2_k1_workaround_locked();
        self.release_phy_swflag();
        result
    }

    fn setup_copper_link_locked(&self) -> Result<(), &'static str> {
        // Run the PCH2 PHY workarounds even when BIOS/PXE left link up. The
        // old early-return path inherited EEE/K1 state from firmware and made
        // first-connection behaviour depend on what traffic had run before.
        self.apply_pch2_phy_workarounds()?;

        if self.read(REG_STATUS) & STATUS_LU != 0 {
            self.apply_pch2_k1_workaround_locked()?;
            println!("e1000e: preserving PXE PHY link after PCH2 workarounds");
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

    fn setup_copper_link(&self) -> Result<(), &'static str> {
        // Linux pch2lan gates the automatic hardware PHY configuration while
        // the driver performs the 82579 MDIO/EMI sequence. Otherwise firmware
        // may modify the same PHY state concurrently during early link setup.
        let fw_valid = self.read(REG_FWSM) & FWSM_FW_VALID != 0;
        self.gate_hw_phy_config(true);

        let result = match self.acquire_phy_swflag() {
            Ok(()) => {
                let result = self.setup_copper_link_locked();
                self.release_phy_swflag();
                result
            }
            Err(err) => Err(err),
        };

        // e1000e only ungates pch2lan itself when manageability firmware is
        // not active. Give the PHY configuration cycle time to settle first.
        if !fw_valid {
            // Linux pch2lan waits 10-11 ms before ungating automatic PHY config.
            hw_delay_ms(10);
            self.gate_hw_phy_config(false);
        }

        result
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

        // Felix only owns RX queue 0. BIOS/PXE/ME may leave RSS/multiple-
        // receive-queue mode programmed, in which case the 4-tuple hash can
        // steer some TCP flows to queue 1. That makes behaviour depend on the
        // ephemeral source port (and therefore on whether another connection,
        // such as reqwest-smoke, ran first). Force legacy single-queue receive.
        let inherited_mrqc = self.read(REG_MRQC);
        if inherited_mrqc != 0 {
            println!(
                "e1000e: disabling inherited MRQC/RSS value={:#010x}",
                inherited_mrqc
            );
        }
        self.write(REG_MRQC, 0);
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

    /// Whether smoltcp may consume another packet from a socket TX queue.
    ///
    /// TxToken::consume cannot report a driver error. Advertising a token
    /// while the descriptor is still owned by hardware therefore silently
    /// discarded the packet when send() returned "TX ring full". Larger TLS
    /// exchanges expose that bug much more readily than one-shot HTTP probes.
    pub fn can_transmit(&self) -> bool {
        if !self.initialized.load(Ordering::Acquire) {
            return false;
        }
        let slot = self.tx_head.load(Ordering::Relaxed);
        let next = (slot + 1) % E1000E_TX_RING_SIZE;
        dma_sync();
        unsafe {
            // Keep one descriptor unused. TDT is the first descriptor not
            // owned by hardware, so publishing a completely full circular
            // ring would make TDT catch TDH and look empty to the device.
            read_volatile(&(*self.tx_ring.add(slot)).status) & TXD_STAT_DD != 0
                && read_volatile(&(*self.tx_ring.add(next)).status) & TXD_STAT_DD != 0
        }
    }

    pub fn send(&self, data: &[u8]) -> Result<(), &'static str> {
        if !self.initialized.load(Ordering::Acquire) {
            return Err("not initialized");
        }
        if data.is_empty() || data.len() > TX_BUF_SIZE {
            return Err("frame too large");
        }
        let slot = self.tx_head.load(Ordering::Relaxed);
        let next = (slot + 1) % E1000E_TX_RING_SIZE;
        dma_sync();
        unsafe {
            let desc = &mut *self.tx_ring.add(slot);
            // Check capacity before touching the current descriptor. If this
            // returned after clearing current.status but before advancing TDT,
            // the unpublished slot would remain permanently busy in software.
            if read_volatile(&desc.status) & TXD_STAT_DD == 0
                || read_volatile(&(*self.tx_ring.add(next)).status) & TXD_STAT_DD == 0
            {
                let packet = self.tx_packets.load(Ordering::Relaxed);
                if !self.tx_stall_reported.swap(true, Ordering::AcqRel) {
                    println!(
                        "e1000e: TX STALL packet={} slot={} TDH={} TDT={} desc_status={:#04x} next_status={:#04x} STATUS={:#010x}",
                        packet,
                        slot,
                        self.read(REG_TDH),
                        self.read(REG_TDT),
                        read_volatile(&desc.status),
                        read_volatile(&(*self.tx_ring.add(next)).status),
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
            self.tx_stall_reported.store(false, Ordering::Release);
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
        let packet = self.tx_packets.fetch_add(1, Ordering::Relaxed);
        dma_sync();
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
        self.track_tcp_frame(true, data);
        log_frame("TX", packet, data);
        if packet < 8 {
            // Diagnostic snapshot only. Do not delay the live TX/ACK path.
            dma_sync();
            let desc_status = unsafe { read_volatile(&(*self.tx_ring.add(slot)).status) };
            let status = self.read(REG_STATUS);
            // println!(
            //     "e1000e: TX ring slot={} DD={} TDH={} TDT={} link={} STATUS={:#010x} GPTC={} TPT={} TNCRS={} ECOL={} LATECOL={} TDFH={} TDFT={} TDFPC={}",
            //     slot,
            //     desc_status & TXD_STAT_DD != 0,
            //     self.read(REG_TDH),
            //     self.read(REG_TDT),
            //     status & STATUS_LU != 0,
            //     status,
            //     self.read(REG_GPTC),
            //     self.read(REG_TPT),
            //     self.read(REG_TNCRS),
            //     self.read(REG_ECOL),
            //     self.read(REG_LATECOL),
            //     self.read(REG_TDFH),
            //     self.read(REG_TDFT),
            //     self.read(REG_TDFPC)
            // );
            // self.log_rx_state();
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
        let old_link = self.link_up.swap(link, Ordering::AcqRel);
        if old_link != link {
            println!(
                "e1000e: link changed: {} STATUS={:#010x}",
                if link { "up" } else { "down" },
                status_reg
            );
            if link {
                if let Err(err) = self.apply_pch2_k1_workaround() {
                    println!("e1000e: 82579 K1 workaround failed on link-up: {}", err);
                }
            }
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
                self.track_tcp_frame(false, &output[..length]);
                log_frame("RX", packet, &output[..length]);
            } else {
                println!(
                    "e1000e: RX DROP packet={} slot={} valid=false len={} staterr={:#010x}",
                    packet,
                    slot,
                    length,
                    status_error
                );
            }

            if packet < 8 {
                if valid {
                    if output.len() >= 16 && output[12] == 0 && output[13] == 0 {
                        // println!(
                        //     "e1000e: RX raw {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} expected={:#010x} stride={} staterr={:#010x} RXDCTL={:#010x} RCTL={:#010x} RFCTL={:#010x}",
                        //     output[0],
                        //     output[1],
                        //     output[2],
                        //     output[3],
                        //     output[4],
                        //     output[5],
                        //     output[6],
                        //     output[7],
                        //     output[8],
                        //     output[9],
                        //     output[10],
                        //     output[11],
                        //     output[12],
                        //     output[13],
                        //     output[14],
                        //     output[15],
                        //     self.rx_buffers_phys + (slot * E1000E_RX_BUF_SIZE) as u32,
                        //     E1000E_RX_BUF_SIZE,
                        //     status_error,
                        //     self.read(REG_RXDCTL),
                        //     self.read(REG_RCTL),
                        //     self.read(REG_RFCTL)
                        // );
                    }
                }
                // println!(
                //     "e1000e: RX ring slot={} valid={} staterr={:#010x} len={} RDH={} RDT={}",
                //     slot,
                //     valid,
                //     status_error,
                //     length,
                //     self.read(REG_RDH),
                //     self.read(REG_RDT)
                // );
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
                    // println!(
                    //     "e1000e: RX returned batch tail={} RDH={}",
                    //     actual_rdt,
                    //     self.read(REG_RDH)
                    // );
                }
            }
            valid.then_some(length)
        }
    }
}

fn irq_entry(irq: u8) -> bool {
    if IRQ_LINE.load(Ordering::Acquire) != irq {
        return false;
    }
    let mmio = IRQ_MMIO.load(Ordering::Acquire);
    if mmio == 0 {
        return false;
    }

    // ICR is clear-on-read. Do no descriptor walking or protocol work here.
    let cause = unsafe { read_volatile((mmio + REG_ICR) as *const u32) } & IRQ_MASK;
    if cause == 0 {
        return false;
    }
    crate::drivers::net::mark_irq_pending();
    true
}

fn valid_mac(mac: [u8; 6]) -> bool {
    mac != [0; 6] && mac != [0xff; 6] && mac[0] & 1 == 0
}

fn log_frame(direction: &str, number: usize, frame: &[u8]) {
    if frame.len() < 14 {
        return;
    }
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    if ethertype != 0x0800 || frame.len() < 14 + 20 {
        return;
    }

    let ip = 14usize;
    let ihl = ((frame[ip] & 0x0f) as usize) * 4;
    if ihl < 20 || frame.len() < ip + ihl || frame[ip + 9] != 6 {
        return;
    }

    let tcp = ip + ihl;
    if frame.len() < tcp + 20 {
        return;
    }
    let flags = frame[tcp + 13];
    const FIN: u8 = 0x01;
    const SYN: u8 = 0x02;
    const RST: u8 = 0x04;
    const ACK: u8 = 0x10;
    // Never print ordinary TCP data/ACK packets from the NIC hot path.
    // Console/framebuffer output here runs inside stack.poll() and can delay
    // ACK generation by hundreds of milliseconds during a receive burst.
    // F12 keeps the detailed TCP flow snapshot; the live log only needs
    // connection-control packets.
    if flags & (FIN | SYN | RST) == 0 {
        return;
    }
    let tcp_header_len = ((frame[tcp + 12] >> 4) as usize) * 4;
    if tcp_header_len < 20 || frame.len() < tcp + tcp_header_len {
        return;
    }
    let ip_total_len = u16::from_be_bytes([frame[ip + 2], frame[ip + 3]]) as usize;
    let payload_len = ip_total_len.saturating_sub(ihl + tcp_header_len);

    let src_port = u16::from_be_bytes([frame[tcp], frame[tcp + 1]]);
    let dst_port = u16::from_be_bytes([frame[tcp + 2], frame[tcp + 3]]);
    let src = [frame[ip + 12], frame[ip + 13], frame[ip + 14], frame[ip + 15]];
    let dst = [frame[ip + 16], frame[ip + 17], frame[ip + 18], frame[ip + 19]];
    println!(
        "e1000e: TCP {}#{} {}.{}.{}.{}:{} -> {}.{}.{}.{}:{} flags={}{}{}{} frame={} payload={}",
        direction,
        number,
        src[0], src[1], src[2], src[3], src_port,
        dst[0], dst[1], dst[2], dst[3], dst_port,
        if flags & SYN != 0 { "SYN " } else { "" },
        if flags & ACK != 0 { "ACK " } else { "" },
        if flags & FIN != 0 { "FIN " } else { "" },
        if flags & RST != 0 { "RST" } else { "" },
        frame.len(),
        payload_len,
    );
}
