//! Intel 8255x (82557/82558/82559) driver with proper Rx/Tx rings
//! Uses Felix PCI subsystem + PageManager for DMA buffers

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
use crate::drivers::net::{map_mmio, RX_BUF_SIZE, RX_RING_SIZE, TX_BUF_SIZE, TX_RING_SIZE};
use crate::memory::paging::{PAGING, PAGE_SIZE, PhysAddr, KERNEL_OFFSET};
use crate::pci::{self, device::PciDevice};
use crate::println;
use crate::sync::mutex::Mutex;

// ===================== Константы =====================
const EE_SHIFT_CLK: u16 = 0x01;
const EE_CS:        u16 = 0x02;
const EE_DATA_WRITE:u16 = 0x04;
const EE_DATA_READ: u16 = 0x08;
const EE_ENB:       u16 = 0x4800 | EE_CS;   // 0x4802


// SCB offsets
pub const SCB_STATUS: usize = 0x00;
const SCB_CMD: usize = 0x02;
const SCB_POINTER: usize = 0x04;
const SCB_PORT: usize = 0x08;
const SCB_EEPROM: usize = 0x0E;

// SCB Status
const STAT_CX:  u16 = 1 << 15;  // было << 7
const STAT_FR:  u16 = 1 << 14;  // было << 6
const STAT_CNA: u16 = 1 << 13;  // было << 5
const STAT_RNR: u16 = 1 << 12;  // было << 4

// SCB Commands
const CU_START: u16 = 0x10;
const CU_RESUME: u16 = 0x20;
const CU_LOAD_BASE: u16 = 0x60;
const RU_START: u16 = 0x01;
const RU_RESUME: u16 = 0x02;
const RU_ABORT: u16 = 0x04;
const RU_LOAD_BASE: u16 = 0x06;

// Command bits
const CMD_EL: u16 = 1 << 15;
const CMD_S: u16 = 1 << 14;
const CMD_I: u16 = 1 << 13;
const CMD_C: u16 = 1 << 15; // Completed bit in status
const CMD_OK: u16 = 1 << 13;

const CMD_NOP: u16 = 0x0000;
const CMD_IA_SETUP: u16 = 0x0001;
const CMD_CONFIGURE: u16 = 0x0002;
const CMD_TX: u16 = 0x0004;

const PORT_SOFT_RESET: u32 = 0x0;

// ===================== Дескрипторы =====================

/// Transmit Command Block (TCB) + inline TBD for simplicity
#[repr(C, align(16))]
struct TxDesc {
    status: u16,
    command: u16,
    link: u32,          // physical address of next TCB
    tbd_addr: u32,      // 0xFFFFFFFF = immediate data / simplified
    tcb_byte_count: u16,
    tx_threshold: u8,
    tbd_number: u8,
    // Immediate data area (we put packet here for simplicity)
    data: [u8; TX_BUF_SIZE],
}

/// Receive Frame Descriptor (RFD) — simplified, data follows header
#[repr(C, align(16))]
struct RxDesc {
    status: u16,
    command: u16,
    link: u32,          // physical of next RFD
    reserved: u32,
    count: u16,         // actual size in low 14 bits when complete
    size: u16,          // buffer size
    data: [u8; RX_BUF_SIZE],
}

// ===================== Драйвер =====================

pub struct I8255x {
    pub(crate) mmio: usize,
    irq: u8,
    pub(crate) mac: [u8; 6],

    // Физические адреса колец (для NIC)
    tx_ring_phys: u32,
    rx_ring_phys: u32,

    // Виртуальные указатели (identity map → можем использовать как phys)
    tx_ring: *mut TxDesc,
    rx_ring: *mut RxDesc,

    tx_head: AtomicUsize, // next free slot for TX
    tx_tail: AtomicUsize, // last completed
    rx_idx: AtomicUsize,  // next expected RX

    initialized: AtomicBool,
}

unsafe impl Send for I8255x {}
unsafe impl Sync for I8255x {}

pub static NET: Mutex<Option<I8255x>> = Mutex::new(None);
/// Проверенный Configure-блок для 82557/82558/82559
/// (на основе Linux eepro100 + Intel рекомендаций)
const CONFIG_DATA: [u8; 22] = [
    0x16, 0x08, 0x00, 0x00, 0x00, 0x00, 0x32, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf2, 0x49, // promisc
    0x00, 0x40, 0xf2, 0x80, 0x3f, 0x05,
];

fn eeprom_delay() {
    for _ in 0..200 {
        core::hint::spin_loop();
    }
}

impl I8255x {
    pub fn init() -> Result<(), &'static str> {
        let dev = pci::find_device(0x8086, 0x1229)
            .or_else(|| pci::find_device(0x8086, 0x1209))
            .ok_or("Intel 8255x not found")?;

        println!(
            "i8255x: found {:02x}:{:02x}.{} IRQ {}",
            dev.bus, dev.device, dev.function, dev.interrupt_line
        );

        dev.enable_bus_mastering();

        let bar = dev.get_bar(0).ok_or("No BAR0")?;
        let (mmio_phys, bar_size) = match bar {
            crate::pci::bar::Bar::Memory { address, size, .. } => (*address, *size),
            _ => return Err("BAR0 is not Memory"),
        };

        println!("i8255x: BAR0 phys={:#x} size={:#x}", mmio_phys, bar_size);

        let mmio = map_mmio(mmio_phys, bar_size)?;
        println!("i8255x: MMIO mapped at virt {:#x}", mmio);

        // ---- Выделяем страницы под кольца ----
        let tx_pages = ((core::mem::size_of::<TxDesc>() * TX_RING_SIZE) + PAGE_SIZE - 1) / PAGE_SIZE;
        let rx_pages = ((core::mem::size_of::<RxDesc>() * RX_RING_SIZE) + PAGE_SIZE - 1) / PAGE_SIZE;

        let mut tx_phys = 0u32;
        let mut rx_phys = 0u32;

        unsafe {
            let mut paging = PAGING.lock();

            // TX ring — берём подряд идущие фреймы (bump-аллокатор даёт consecutive)
            let tx_frame = paging.alloc_frame();
            for _ in 1..tx_pages {
                let _ = paging.alloc_frame();
            }
            tx_phys = tx_frame << 12;          // ← важно!

            // RX ring
            let rx_frame = paging.alloc_frame();
            for _ in 1..rx_pages {
                let _ = paging.alloc_frame();
            }
            rx_phys = rx_frame << 12;          // ← важно!
        }
        // Благодаря identity-mapping низкой памяти можем использовать phys как virt
        use crate::memory::paging::KERNEL_OFFSET;
        let tx_ring = (tx_phys + KERNEL_OFFSET) as *mut TxDesc;
        let rx_ring = (rx_phys + KERNEL_OFFSET) as *mut RxDesc;

        let mut nic = I8255x {
            mmio,
            irq: dev.interrupt_line,
            mac: [0; 6],
            tx_ring_phys: tx_phys,
            rx_ring_phys: rx_phys,
            tx_ring,
            rx_ring,
            tx_head: AtomicUsize::new(0),
            tx_tail: AtomicUsize::new(0),
            rx_idx: AtomicUsize::new(0),
            initialized: AtomicBool::new(false),
        };

        nic.reset()?;
        nic.read_eeprom_mac()?;
        nic.setup_rings()?;

        nic.configure()?;
        nic.start_ru()?;
        unsafe {
            // ACK все статусные биты
            let st = read_volatile((nic.mmio + SCB_STATUS) as *const u16);
            write_volatile((nic.mmio + SCB_STATUS) as *mut u16, st & 0xFF00);
        }

        nic.initialized.store(true, Ordering::SeqCst);

        println!(
            "i8255x: ready  MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            nic.mac[0], nic.mac[1], nic.mac[2], nic.mac[3], nic.mac[4], nic.mac[5]
        );

        *NET.lock() = Some(nic);
        Ok(())
    }

    // -------------------- Низкоуровневые helpers --------------------

    fn scb_cmd(&self, cmd: u16) {
        unsafe {
            while (read_volatile((self.mmio + SCB_CMD) as *const u16) & 0xFF) != 0 {}
            write_volatile((self.mmio + SCB_CMD) as *mut u16, cmd);
        }
    }

    fn wait_scb(&self) {
        unsafe {
            while (read_volatile((self.mmio + SCB_CMD) as *const u16) & 0xFF) != 0 {}
        }
    }

    fn write_pointer(&self, ptr: u32) {
        unsafe {
            write_volatile((self.mmio + SCB_POINTER) as *mut u32, ptr);
        }
    }

    fn reset(&self) -> Result<(), &'static str> {
        unsafe {
            write_volatile((self.mmio + SCB_PORT) as *mut u32, PORT_SOFT_RESET);
        }
        for _ in 0..50_000 {
            core::hint::spin_loop();
        }
        self.wait_scb();
        Ok(())
    }

    // -------------------- EEPROM / MAC --------------------

    // fn read_eeprom_mac(&mut self) -> Result<(), &'static str> {
    //     for i in 0..3u8 {
    //         let word = self.eeprom_read(i)?;
    //         self.mac[i as usize * 2] = (word & 0xFF) as u8;
    //         self.mac[i as usize * 2 + 1] = (word >> 8) as u8;
    //     }
    //     Ok(())
    // }

    fn eeprom_delay() {
        for _ in 0..100 {
            core::hint::spin_loop();
        }
    }

    fn eeprom_read(&self, addr: u8) -> u16 {
        unsafe {
            let reg = (self.mmio + SCB_EEPROM) as *mut u16;

            // CS = 1, остальные биты 0
            write_volatile(reg, EE_ENB);
            eeprom_delay();

            // Команда: READ (110) + 6-bit address  → всего 9 бит команды
            // Потом 16 бит данных. Итого 25 бит.
            let cmd = ((0b110u32 << 6) | (addr as u32 & 0x3F)) << 16;

            let mut retval = 0u32;

            for i in (0..25).rev() {
                let dataval = if (cmd & (1u32 << i)) != 0 {
                    EE_DATA_WRITE
                } else {
                    0
                };

                // выставляем данные
                write_volatile(reg, EE_ENB | dataval);
                eeprom_delay();

                // clock high
                write_volatile(reg, EE_ENB | dataval | EE_SHIFT_CLK);
                eeprom_delay();

                // читаем DO
                let v = read_volatile(reg);
                retval = (retval << 1) | if (v & EE_DATA_READ) != 0 { 1 } else { 0 };

                // clock low
                write_volatile(reg, EE_ENB | dataval);
                eeprom_delay();
            }

            // CS = 0
            write_volatile(reg, EE_ENB & !EE_CS);
            eeprom_delay();

            (retval & 0xFFFF) as u16
        }
    }

    fn read_eeprom_mac(&mut self) -> Result<(), &'static str> {
        // Временно для QEMU — тот MAC, который указан в -device
        self.mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        Ok(())
    }

    // -------------------- Кольца --------------------

    fn setup_rings(&mut self) -> Result<(), &'static str> {
        unsafe {
            // ---- TX ring ----
            for i in 0..TX_RING_SIZE {
                let desc = &mut *self.tx_ring.add(i);
                write_volatile(&mut desc.status, 0);
                write_volatile(&mut desc.command, 0);
                let next = if i + 1 == TX_RING_SIZE {
                    self.tx_ring_phys
                } else {
                    self.tx_ring_phys + ((i + 1) * core::mem::size_of::<TxDesc>()) as u32
                };
                write_volatile(&mut desc.link, next);
                write_volatile(&mut desc.tbd_addr, 0xFFFFFFFF);
                write_volatile(&mut desc.tcb_byte_count, 0);
                write_volatile(&mut desc.tx_threshold, 0xE0);
                write_volatile(&mut desc.tbd_number, 0);
            }

            for i in 0..RX_RING_SIZE {
                let desc = &mut *self.rx_ring.add(i);
                write_volatile(&mut desc.status, 0);
                write_volatile(&mut desc.command, if i + 1 == RX_RING_SIZE { CMD_EL } else { 0 });

                let next = if i + 1 == RX_RING_SIZE {
                    self.rx_ring_phys
                } else {
                    self.rx_ring_phys + ((i + 1) * core::mem::size_of::<RxDesc>()) as u32
                };
                write_volatile(&mut desc.link, next);
                write_volatile(&mut desc.reserved, 0);
                write_volatile(&mut desc.count, 0);
                write_volatile(&mut desc.size, RX_BUF_SIZE as u16);
            }
        }

        Ok(())
    }

    // -------------------- Configure + IA Setup --------------------

    fn configure(&mut self) -> Result<(), &'static str> {
        unsafe {
            let cmd = &mut *self.tx_ring; // используем первый дескриптор

            // ---------- 1. Configure command ----------
            write_volatile(&mut cmd.status, 0);
            write_volatile(&mut cmd.command, CMD_CONFIGURE | CMD_EL | CMD_I);
            write_volatile(&mut cmd.link, 0xFFFFFFFF);

            // Данные Configure идут сразу после заголовка CB (offset 8)
            let cfg_ptr = (self.tx_ring as usize + 8) as *mut u8;
            for (i, &b) in CONFIG_DATA.iter().enumerate() {
                write_volatile(cfg_ptr.add(i), b);
            }
        }
        self.write_pointer(0);
        self.scb_cmd(CU_LOAD_BASE);
        self.wait_scb();

        // 2. Указать на первый RFD и запустить
        self.write_pointer(self.tx_ring_phys);
        self.scb_cmd(CU_START);
        self.wait_scb();


        // Ждём завершения Configure
        for _ in 0..300_000 {
            unsafe {
                if read_volatile(&(*self.tx_ring).status) & CMD_C != 0 {
                    break;
                }
            }
            core::hint::spin_loop();
        }

        // ---------- 2. Individual Address Setup (MAC) ----------
        unsafe {
            let cmd = &mut *self.tx_ring;
            write_volatile(&mut cmd.status, 0);
            write_volatile(&mut cmd.command, CMD_IA_SETUP | CMD_EL | CMD_I);
            write_volatile(&mut cmd.link, 0xFFFFFFFF);

            let mac_ptr = (self.tx_ring as usize + 8) as *mut u8;
            for i in 0..6 {
                write_volatile(mac_ptr.add(i), self.mac[i]);
            }
        }

        self.write_pointer(self.tx_ring_phys);
        self.scb_cmd(CU_START);
        self.wait_scb();

        for _ in 0..200_000 {
            unsafe {
                if read_volatile(&(*self.tx_ring).status) & CMD_C != 0 {
                    return Ok(());
                }
            }
            core::hint::spin_loop();
        }

        Err("Configure/IA-Setup timeout")
    }

    fn start_ru(&self) -> Result<(), &'static str> {
        // 1. Установить RU Base = 0
        self.write_pointer(0);
        self.scb_cmd(RU_LOAD_BASE);
        self.wait_scb();

        // 2. Указать на первый RFD и запустить
        self.write_pointer(self.rx_ring_phys);
        self.scb_cmd(RU_START);
        self.wait_scb();

        Ok(())
    }

    pub fn dump_scb(&self) {
        unsafe {
            let status = read_volatile((self.mmio + SCB_STATUS) as *const u16);
            let cmd    = read_volatile((self.mmio + SCB_CMD) as *const u16);
            let rus = (status >> 2) & 0xF;
            let rus_str = match rus {
                0 => "Idle",
                1 => "Suspended",
                2 => "No resources",
                4 => "Ready",
                _ => "???",
            };
            println!("SCB status={:04x} cmd={:04x}  RUS={} ({})", status, cmd, rus, rus_str);
        }
    }

    pub fn dump_rfds(&self) {
        unsafe {
            println!("--- RFDs ---");
            for i in 0..RX_RING_SIZE {
                let desc = &*self.rx_ring.add(i);
                let st  = read_volatile(&desc.status);
                let cmd = read_volatile(&desc.command);
                let cnt = read_volatile(&desc.count);
                let sz  = read_volatile(&desc.size);
                println!("RFD[{:02}] st={:04x} cmd={:04x} cnt={:04x} sz={:04x}", i, st, cmd, cnt, sz);
            }
        }
    }
    // -------------------- Отправка --------------------

    pub fn send(&self, data: &[u8]) -> Result<(), &'static str> {
        if !self.initialized.load(Ordering::Acquire) {
            return Err("not initialized");
        }
        if data.len() > TX_BUF_SIZE {
            return Err("frame too large");
        }

        let head = self.tx_head.load(Ordering::Relaxed);
        let next = (head + 1) % TX_RING_SIZE;

        // Проверяем, свободен ли слот
        unsafe {
            let desc = &mut *self.tx_ring.add(head);
            if read_volatile(&desc.status) & CMD_C == 0 && read_volatile(&desc.command) != 0 {
                // Ещё не завершён предыдущий — можно сделать очередь или вернуть Busy
                return Err("TX ring full");
            }

            // Заполняем дескриптор
            write_volatile(&mut desc.status, 0);
            write_volatile(&mut desc.command, CMD_TX | CMD_EL | CMD_I);
            write_volatile(&mut desc.tbd_addr, 0xFFFFFFFF); // immediate
            write_volatile(&mut desc.tcb_byte_count, data.len() as u16);
            write_volatile(&mut desc.tx_threshold, 0xE0);
            write_volatile(&mut desc.tbd_number, 0);

            // Копируем данные
            core::ptr::copy_nonoverlapping(data.as_ptr(), desc.data.as_mut_ptr(), data.len());
        }

        self.wait_scb();
        self.write_pointer(self.tx_ring_phys + (head * core::mem::size_of::<TxDesc>()) as u32);
        self.scb_cmd(CU_START);

        self.tx_head.store(next, Ordering::Release);
        Ok(())
    }

    // -------------------- Приём --------------------

    pub fn recv(&self, buf: &mut [u8]) -> Option<usize> {
        if !self.initialized.load(Ordering::Acquire) {
            return None;
        }

        // Идём по кольцу последовательно (не сканируем все RFD каждый раз)
        let start = self.rx_idx.load(Ordering::Relaxed);
        for offset in 0..RX_RING_SIZE {
            let i = (start + offset) % RX_RING_SIZE;
            unsafe {
                let desc = &mut *self.rx_ring.add(i);
                let status = read_volatile(&desc.status);

                // Нужен Complete; OK желателен (без OK — часто мусор/ошибка)
                if status & CMD_C == 0 {
                    continue;
                }

                let count = (read_volatile(&desc.count) & 0x3FFF) as usize;

                // Минимальный Ethernet-frame = 14 (заголовок).
                // Мельче — мусор; его нельзя отдавать в smoltcp.
                let valid = count >= 14
                    && count <= buf.len()
                    && count <= RX_BUF_SIZE
                    && (status & CMD_OK) != 0;

                if valid {
                    core::ptr::copy_nonoverlapping(
                        desc.data.as_ptr(),
                        buf.as_mut_ptr(),
                        count,
                    );
                }

                // === Важно: правильно вернуть RFD в кольцо ===
                // Раньше ставили command=0 и сбрасывали EL у последнего
                // дескриптора → RU ломался и при RNR/рестарте
                // отдавал старые/пустые кадры (n=35 нулей).
                write_volatile(&mut desc.status, 0);
                write_volatile(&mut desc.count, 0);
                write_volatile(&mut desc.size, RX_BUF_SIZE as u16);
                write_volatile(
                    &mut desc.command,
                    if i + 1 == RX_RING_SIZE { CMD_EL } else { 0 },
                );

                self.rx_idx.store((i + 1) % RX_RING_SIZE, Ordering::Release);

                let scb = read_volatile((self.mmio + SCB_STATUS) as *const u16);
                if (scb & STAT_RNR) != 0 {
                    let _ = self.start_ru();
                }

                if valid {
                    return Some(count);
                }
                // невалидный кадр — RFD уже освобождён, ищем дальше
            }
        }
        None
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    // -------------------- Interrupt handler --------------------

    pub fn handle_interrupt(&self) {
        unsafe {
            let status = read_volatile((self.mmio + SCB_STATUS) as *const u16);

            // Acknowledge
            write_volatile((self.mmio + SCB_STATUS) as *mut u16, status & 0xFF00);

            if status & STAT_FR != 0 {
                // Frame received — можно уведомить сетевой стек / задачу
            }

            if status & STAT_CX != 0 {
                // Command completed
            }

            if status & STAT_RNR != 0 {
                // Receiver Not Ready — перезапускаем
                let _ = self.start_ru();
            }
        }
    }
}