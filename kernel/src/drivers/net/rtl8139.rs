//! Realtek RTL8139 Fast Ethernet
//! I/O BAR + 8K WRAP RX ring + 4 TX descriptors

use crate::drivers::net::{RX_BUF_SIZE, TX_BUF_SIZE};
use crate::io::{inb, inl, inw, outb, outl, outw};
use crate::memory::paging::{KERNEL_OFFSET, PAGE_SIZE, PAGING};
use crate::pci;
use crate::println;
use crate::sync::mutex::Mutex;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const VENDOR: u16 = 0x10EC;
const DEVICE: u16 = 0x8139;

const REG_MAC: u16 = 0x00;
const REG_TSD0: u16 = 0x10;
const REG_TSAD0: u16 = 0x20;
const REG_RBSTART: u16 = 0x30;
const REG_CMD: u16 = 0x37;
const REG_CAPR: u16 = 0x38;
const REG_IMR: u16 = 0x3C;
const REG_ISR: u16 = 0x3E;
const REG_TCR: u16 = 0x40;
const REG_RCR: u16 = 0x44;
const REG_CBR: u16 = 0x3A;
const REG_MPC: u16 = 0x4C;
const REG_9346CR: u16 = 0x50;
const REG_CONFIG1: u16 = 0x52;
const REG_MSR: u16 = 0x58;
const REG_MULINT: u16 = 0x5C;

const CMD_RST: u8 = 0x10;
const CMD_RE: u8 = 0x08;
const CMD_TE: u8 = 0x04;
const CMD_BUFE: u8 = 0x01;

const ISR_ROK: u16 = 1 << 0;
const ISR_RER: u16 = 1 << 1;
const ISR_TOK: u16 = 1 << 2;
const ISR_TER: u16 = 1 << 3;

const TSD_OWN: u32 = 1 << 13;
const TSD_TOK: u32 = 1 << 15;

const RCR_AAP: u32 = 1 << 0;
const RCR_APM: u32 = 1 << 1;
const RCR_AM: u32 = 1 << 2;
const RCR_AB: u32 = 1 << 3;
const RCR_WRAP: u32 = 1 << 7; // no-wrap: overflow into pad after 8K
const RCR_MXDMA: u32 = 0x7 << 8;
const RCR_RXFTH_NONE: u32 = 0x7 << 13;
const RX_EARLY: usize = 0xFFF0;

const RX_RING: usize = 8192;
const RX_PAD: usize = 16 + 1518;
const RX_ALLOC: usize = RX_RING + RX_PAD;
const TX_SLOTS: usize = 4;

const TCR_IFG: u32 = 0x0300_0000;
const TCR_DMA: u32 = 0x700;

pub struct Rtl8139 {
    io: u16,
    irq: u8,
    pub(crate) mac: [u8; 6],
    rx_phys: u32,
    rx_buf: *mut u8,
    tx_phys: [u32; TX_SLOTS],
    tx_buf: [*mut u8; TX_SLOTS],
    tx_cur: AtomicUsize,
    rx_off: AtomicUsize,
    initialized: AtomicBool,
}

unsafe impl Send for Rtl8139 {}
unsafe impl Sync for Rtl8139 {}

pub static NET: Mutex<Option<Rtl8139>> = Mutex::new(None);
static RX_LOGS: AtomicUsize = AtomicUsize::new(12);

fn dma_wbinvd() {
    unsafe {
        core::arch::asm!("wbinvd", options(nostack, preserves_flags));
    }
}

fn alloc_contig(bytes: usize) -> Result<(u32, *mut u8), &'static str> {
    let pages = (bytes + PAGE_SIZE - 1) / PAGE_SIZE;
    let mut paging = unsafe { PAGING.lock() };
    let frame = paging.alloc_frame();
    for _ in 1..pages {
        let _ = paging.alloc_frame();
    }
    let phys = frame << 12;
    let virt = (phys + KERNEL_OFFSET) as *mut u8;
    unsafe {
        core::ptr::write_bytes(virt, 0, pages * PAGE_SIZE);
    }
    Ok((phys, virt))
}

impl Rtl8139 {
    pub fn init() -> Result<(), &'static str> {
        let dev = pci::find_device(VENDOR, DEVICE).ok_or("RTL8139 not found")?;

        println!(
            "rtl8139: found {:02x}:{:02x}.{} IRQ {}",
            dev.bus, dev.device, dev.function, dev.interrupt_line
        );

        dev.enable_bus_mastering();

        let io = match dev.get_bar(0) {
            Some(crate::pci::bar::Bar::Io { address, .. }) => *address as u16,
            Some(crate::pci::bar::Bar::Memory { address, .. }) => {
                // QEMU may expose MMIO on BAR1; prefer I/O on BAR0
                match dev.get_bar(1) {
                    Some(crate::pci::bar::Bar::Io { address, .. }) => *address as u16,
                    _ => {
                        return Err("RTL8139 has no I/O BAR");
                    }
                }
            }
            _ => {
                // BAR1 is I/O on some boards
                match dev.get_bar(1) {
                    Some(crate::pci::bar::Bar::Io { address, .. }) => *address as u16,
                    _ => return Err("RTL8139 has no I/O BAR"),
                }
            }
        };

        println!("rtl8139: I/O base {:#x}", io);

        let (rx_phys, rx_buf) = alloc_contig(RX_ALLOC)?;
        let mut tx_phys = [0u32; TX_SLOTS];
        let mut tx_buf = [core::ptr::null_mut(); TX_SLOTS];
        for i in 0..TX_SLOTS {
            let (p, v) = alloc_contig(TX_BUF_SIZE)?;
            tx_phys[i] = p;
            tx_buf[i] = v;
        }

        let mut nic = Rtl8139 {
            io,
            irq: dev.interrupt_line,
            mac: [0; 6],
            rx_phys,
            rx_buf,
            tx_phys,
            tx_buf,
            tx_cur: AtomicUsize::new(0),
            rx_off: AtomicUsize::new(0),
            initialized: AtomicBool::new(false),
        };

        // old 8139: decent PCI latency or TX/RX DMA stalls
        dev.write_u8(0x0D, 0x40);

        nic.power_on();
        nic.reset()?;
        nic.read_mac();
        nic.setup()?;
        nic.initialized.store(true, Ordering::SeqCst);

        println!(
            "rtl8139: ready  MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            nic.mac[0], nic.mac[1], nic.mac[2], nic.mac[3], nic.mac[4], nic.mac[5]
        );

        *NET.lock() = Some(nic);
        Ok(())
    }

    fn power_on(&self) {
        outb(self.io + REG_9346CR, 0xC0);
        outb(self.io + REG_CONFIG1, 0x00);
        outb(self.io + REG_9346CR, 0x00);
    }

    fn reset(&self) -> Result<(), &'static str> {
        outb(self.io + REG_CMD, CMD_RST);
        for _ in 0..100_000 {
            if inb(self.io + REG_CMD) & CMD_RST == 0 {
                for _ in 0..50_000 {
                    core::hint::spin_loop();
                }
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err("RTL8139 reset timeout")
    }

    fn read_mac(&mut self) {
        for i in 0..6 {
            self.mac[i] = inb(self.io + REG_MAC + i as u16);
        }
    }

    fn setup(&self) -> Result<(), &'static str> {
        outl(self.io + REG_RBSTART, self.rx_phys);
        outw(self.io + REG_IMR, 0x0000);
        outw(self.io + REG_ISR, 0xFFFF);
        outw(self.io + REG_MULINT, 0);
        outl(self.io + REG_MPC, 0);

        let rcr = RCR_AAP | RCR_AB | RCR_AM | RCR_APM | RCR_WRAP | RCR_MXDMA | RCR_RXFTH_NONE;
        outl(self.io + REG_RCR, rcr);
        outl(self.io + REG_TCR, TCR_IFG | TCR_DMA);

        outb(self.io + REG_CMD, CMD_TE | CMD_RE);
        outl(self.io + REG_RCR, rcr);
        outw(self.io + REG_CAPR, 0xFFF0);
        dma_wbinvd();
        Ok(())
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    pub fn send(&self, data: &[u8]) -> Result<(), &'static str> {
        if !self.initialized.load(Ordering::Acquire) {
            return Err("not initialized");
        }
        if data.is_empty() || data.len() > TX_BUF_SIZE {
            return Err("bad frame size");
        }

        let slot = self.tx_cur.load(Ordering::Relaxed);
        let tsd = self.io + REG_TSD0 + (slot as u16) * 4;

        // Free when driver owns the slot: reset value is OWN=1 (QEMU/hw),
        // or TSD==0, or previous TX set TOK/error.
        let done = TSD_OWN | TSD_TOK | (1 << 14) | (1 << 30);
        let mut ready = false;
        for _ in 0..1_000_000 {
            let st = inl(tsd);
            if st == 0 || (st & done) != 0 {
                ready = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !ready {
            return Err("TX busy");
        }

        let len = core::cmp::max(data.len(), 60);
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), self.tx_buf[slot], data.len());
            if data.len() < 60 {
                core::ptr::write_bytes(self.tx_buf[slot].add(data.len()), 0, 60 - data.len());
            }
        }

        dma_wbinvd();
        outl(self.io + REG_TSAD0 + (slot as u16) * 4, self.tx_phys[slot]);
        outl(tsd, len as u32);

        self.tx_cur.store((slot + 1) % TX_SLOTS, Ordering::Release);
        Ok(())
    }

    pub fn recv(&self, buf: &mut [u8]) -> Option<usize> {
        if !self.initialized.load(Ordering::Acquire) {
            return None;
        }

        let isr = inw(self.io + REG_ISR);
        if isr != 0 {
            outw(self.io + REG_ISR, isr);
        }

        let cmd = inb(self.io + REG_CMD);
        let cbr = inw(self.io + REG_CBR);
        let capr = inw(self.io + REG_CAPR);
        if cmd & CMD_BUFE != 0 {
            return None;
        }

        dma_wbinvd();

        let off = self.rx_off.load(Ordering::Relaxed);
        unsafe {
            let hdr = self.rx_buf.add(off);
            let word = core::ptr::read_volatile(hdr as *const u32);
            let status = word as u16;
            let size = (word >> 16) as usize;

            let n = RX_LOGS.load(Ordering::Relaxed);
            if n > 0 {
                RX_LOGS.store(n - 1, Ordering::Relaxed);
                log::debug!(
                    "8139 rx cmd={:#x} isr={:#x} cbr={:#x} capr={:#x} off={} hdr={:#x} msr={:#x}",
                    cmd,
                    isr,
                    cbr,
                    capr,
                    off,
                    word,
                    inb(self.io + REG_MSR)
                );
            }

            // Real 8139: 0xFFF0 = FIFO still copying. Do NOT reset RX.
            if size == RX_EARLY || size == 0 {
                return None;
            }

            if status & 1 == 0 || size < 18 || size > 1518 + 4 {
                let next = (off + 4 + 3) & !3;
                let next = next % RX_RING;
                self.rx_off.store(next, Ordering::Release);
                outw(self.io + REG_CAPR, next.wrapping_sub(16) as u16);
                return None;
            }

            let payload = size - 4;
            let copy = core::cmp::min(payload, buf.len());
            core::ptr::copy_nonoverlapping(hdr.add(4), buf.as_mut_ptr(), copy);

            let next = (off + size + 4 + 3) & !3;
            let next = next % RX_RING;
            self.rx_off.store(next, Ordering::Release);
            outw(self.io + REG_CAPR, next.wrapping_sub(16) as u16);

            Some(copy)
        }
    }

    pub fn handle_interrupt(&self) {
        let isr = inw(self.io + REG_ISR);
        outw(self.io + REG_ISR, isr);
        let _ = isr & (ISR_ROK | ISR_TOK | ISR_RER | ISR_TER);
    }
}
