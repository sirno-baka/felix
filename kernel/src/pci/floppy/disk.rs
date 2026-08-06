// kernel/src/pci/floppy/disk.rs

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicBool, Ordering};
use crate::io::{inb, outb};
use crate::time::sleep;
use crate::disk::interface::BlockDevice;
use crate::{print, println};
use crate::memory::allocator::ALLOCATOR;
use crate::memory::paging::PAGING;

const FDC_DMA_BUF_SIZE: usize = 4096;
// Fixed bounce buffer address below 16 MB (and below 1 MB), identity-mapped.
// 128 KB (0x20000) is typically safe in conventional memory.
const FDC_BOUNCE_BUF_ADDR: u32 = 0x00020000;

/// Получает физический адрес bounce-буфера для DMA
fn fdc_dma_buffer_phys_addr() -> u32 {
    FDC_BOUNCE_BUF_ADDR
}

/// Проверяет, что bounce-буфер пригоден для ISA DMA
fn validate_fdc_dma(len: u32) -> Result<u32, u8> {
    if len == 0 || len > (FDC_DMA_BUF_SIZE as u32) {
        return Err(0x50);
    }
    let pa = fdc_dma_buffer_phys_addr();
    // ISA DMA не может адресовать >= 16 MiB.
    if pa >= 0x1000000 {
        return Err(0x54);
    }
    // Буфер не должен пересекать границу 64 KiB.
    let pa_end = pa.wrapping_add(len).wrapping_sub(1);
    if (pa & 0xFFFF0000) != (pa_end & 0xFFFF0000) {
        return Err(0x54);
    }
    Ok(pa)
}

// --- FDC Порты ---
const FDC_DOR:  u16 = 0x3F2;
const FDC_MSR:  u16 = 0x3F4;
const FDC_FIFO: u16 = 0x3F5;
const FDC_CCR:  u16 = 0x3F7;

// --- FDC MSR Биты ---
const MSR_BUSY:  u8 = 0x10; // Command Busy
const MSR_NDMA:  u8 = 0x20; // Non-DMA Execution Phase
const MSR_DIO:   u8 = 0x40; // 0 = CPU->FDC, 1 = CPU<-FDC
const MSR_RQM:   u8 = 0x80; // Request for Master

// --- FDC Команды ---
const CMD_SPECIFY:       u8 = 0x03;
const CMD_RECALIBRATE:   u8 = 0x07;
const CMD_SENSE_INT:     u8 = 0x08;
const CMD_SEEK:          u8 = 0x0F;
const CMD_READ_DATA:     u8 = 0x66; // MFM + Skip deleted data
const CMD_WRITE_DATA:    u8 = 0xC5; // MT + MFM

// --- DMA Порты (Канал 2 для FDC) ---
const DMA_CH2_ADDR_LO: u16 = 0x04;
const DMA_CH2_COUNT_LO: u16 = 0x05;
const DMA_CHAN_MASK: u16 = 0x0A;
const DMA_MODE_REG: u16 = 0x0B;
const DMA_CLEAR_FF: u16 = 0x0C;
const DMA_PAGE_CH2: u16 = 0x81;

const SECTOR_SIZE: u32 = 512;
const SPT: u8 = 18;
const HEADS: u8 = 2;
const TRACKS: u8 = 80;

/// Получает физический адрес из виртуального
fn virt_to_phys(virt: u32) -> u32 {
    unsafe { PAGING.lock().dir.translate(virt).unwrap() }
}

/// Выделяет bounce-буфер (желательно в первых 16 МБ)
fn alloc_bounce_buffer(size: u32) -> Option<u32> {
    let layout = Layout::from_size_align(size as usize, 4096).ok()?;
    let ptr = unsafe { ALLOCATOR.alloc(layout) };
    if ptr.is_null() {
        return None;
    }
    Some(ptr as u32)
}

/// Освобождает bounce-буфер
fn free_bounce_buffer(ptr: u32, size: u32) {
    let layout = Layout::from_size_align(size as usize, 4096).unwrap();
    unsafe { ALLOCATOR.dealloc(ptr as *mut u8, layout); }
}

/// Настраивает DMA контроллер (8237A) для передачи данных.
/// `read` = true: Device -> Memory (чтение с флоппи в RAM).
/// `read` = false: Memory -> Device (запись из RAM на флоппи).
fn setup_dma(addr: u32, count: u32, read: bool) -> Result<(), u8> {
    // Ограничения ISA DMA: память до 16 МБ и запрет на переход через границу 64 КБ.
    if addr >= 0x1000000 {
        return Err(0x50); // Адрес вне первых 16 МБ
    }
    let offset = (addr & 0xFFFF) as u32;
    if offset + count > 0x10000 {
        return Err(0x51); // Переход через границу 64 КБ (запрещено 8237A)
    }

    // 1. Маскируем канал 2
    outb(DMA_CHAN_MASK, 0x06);

    // 2. Сбрасываем flip-flop
    outb(DMA_CLEAR_FF, 0x00);

    // 3. Режим:
    // 0x46 = Single + Increment + Read (Device->Mem) + Channel 2
    // 0x4A = Single + Increment + Write (Mem->Device) + Channel 2
    let mode = if read { 0x46 } else { 0x4A };
    outb(DMA_MODE_REG, mode);

    // 4. Страница + адрес
    let page = (addr >> 16) as u8;
    let offset16 = (addr & 0xFFFF) as u16;

    outb(DMA_PAGE_CH2, page);

    // Снова сбрасываем flip-flop перед адресом (более надёжно)
    outb(DMA_CLEAR_FF, 0x00);
    outb(DMA_CH2_ADDR_LO, (offset16 & 0xFF) as u8);
    outb(DMA_CH2_ADDR_LO, ((offset16 >> 8) & 0xFF) as u8);

    // 5. Счётчик (count - 1)
    outb(DMA_CLEAR_FF, 0x00); // ещё раз перед count
    let count_val = (count - 1) as u16;
    outb(DMA_CH2_COUNT_LO, (count_val & 0xFF) as u8);
    outb(DMA_CH2_COUNT_LO, ((count_val >> 8) & 0xFF) as u8);

    // 6. Снимаем маску
    outb(DMA_CHAN_MASK, 0x02);

    Ok(())
}

pub struct Floppy {
    drive: u8,
    motor_on: AtomicBool,
    initialized: AtomicBool,
}

impl Floppy {
    pub const fn new(drive: u8) -> Self {
        Self {
            drive,
            motor_on: AtomicBool::new(false),
            initialized: AtomicBool::new(false),
        }
    }

    pub fn init(&self) -> Result<(), u8> {
        // Маскируем IRQ6 (floppy) в master PIC
        unsafe {
            let mask = inb(0x21);
            outb(0x21, mask | (1 << 6));
        }
        println!("[fdc] reset start");
        outb(FDC_DOR, 0x00);
        sleep(20);
        outb(FDC_DOR, 0x0C);
        sleep(20);
        println!("[fdc] after reset");

        for i in 0..4 {
            println!("[fdc] sense {}", i);
            let _ = self.sense_interrupt();
        }
        println!("[fdc] after senses");

        outb(FDC_CCR, 0x00);
        println!("[fdc] CCR set");

        self.specify()?;
        println!("[fdc] specify ok");

        self.motor_on();
        println!("[fdc] motor on");

        match self.recalibrate() {
            Ok(_) => {println!("[fdc] recalibrate ok");}
            Err(_) => {println!("[fdc] recalibrate fail");}
        };
        println!("[fdc] recalibrate ok");

        self.initialized.store(true, Ordering::SeqCst);
        println!("[fdc] init done");
        Ok(())
    }

    fn wait_rqm(&self) -> Result<(), u8> {
        for _ in 0..1_000_000 {
            let msr = inb(FDC_MSR);
            if (msr & MSR_RQM) != 0 {
                return Ok(());
            }
        }
        Err(0x10)
    }

    fn wait_for_command_phase(&self) -> Result<(), u8> {
        for _ in 0..1_000_000 {
            let msr = inb(FDC_MSR);
            if (msr & MSR_RQM) != 0 && (msr & MSR_DIO) == 0 {
                return Ok(());
            }
        }
        Err(0x10)
    }

    fn wait_for_result_phase(&self) -> Result<(), u8> {
        for _ in 0..1_000_000 {
            let msr = inb(FDC_MSR);
            if (msr & (MSR_RQM | MSR_DIO)) == (MSR_RQM | MSR_DIO) {
                return Ok(());
            }
        }
        Err(0x40)
    }

    fn send_byte(&self, val: u8) -> Result<(), u8> {
        self.wait_rqm()?;
        if (inb(FDC_MSR) & MSR_DIO) != 0 {
            return Err(0x11);
        }
        outb(FDC_FIFO, val);
        Ok(())
    }

    fn recv_byte(&self) -> Result<u8, u8> {
        self.wait_rqm()?;
        if (inb(FDC_MSR) & MSR_DIO) == 0 {
            return Err(0x12);
        }
        Ok(inb(FDC_FIFO))
    }

    fn motor_on(&self) {
        if self.motor_on.swap(true, Ordering::SeqCst) {
            return;
        }
        // bit3 = DMA/IRQ enable, bit2 = reset off, bits 0-1 = drive select,
        // bit 4+drive = motor enable
        let dor = (0x10 << self.drive) | 0x0C | self.drive;
        outb(FDC_DOR, dor);
        sleep(500); // время раскрутки мотора
    }

    fn motor_off(&self) {
        if !self.motor_on.swap(false, Ordering::SeqCst) {
            return;
        }
        outb(FDC_DOR, 0x0C); // motors off, DMA/IRQ still enabled, reset off
    }

    fn sense_interrupt(&self) -> Result<(u8, u8), u8> {
        self.send_byte(CMD_SENSE_INT)?;
        let st0 = self.recv_byte()?;
        let cyl = self.recv_byte()?;
        Ok((st0, cyl))
    }

    fn specify(&self) -> Result<(), u8> {
        // Более консервативные и проверенные значения для 500 kbps:
        // SRT = 3 ms (0xD), HUT = 240 ms (0xF) → 0xDF
        // HLT = 16 ms (0x01 << 1), ND = 0 (DMA) → 0x02
        self.send_byte(CMD_SPECIFY)?;
        self.send_byte(0xDF)?;
        self.send_byte(0x02)?;
        sleep(20);
        Ok(())
    }

    fn recalibrate(&self) -> Result<(), u8> {
        self.motor_on();
        self.send_byte(CMD_RECALIBRATE)?;
        self.send_byte(self.drive)?;

        // Даём механике время (на реальном железе нужно больше)
        // sleep(1500);

        self.wait_for_command_phase()?;

        let (st0, _) = self.sense_interrupt()?;
        // Bit 5 (0x20) = Seek End / Recalibrate successful
        if (st0 & 0x20) == 0 {
            // Повторная попытка
            sleep(500);
            self.send_byte(CMD_RECALIBRATE)?;
            self.send_byte(self.drive)?;
            sleep(1500);
            self.wait_for_command_phase()?;
            let (st0, _) = self.sense_interrupt()?;
            if (st0 & 0x20) == 0 {
                return Err(0x20);
            }
        }
        Ok(())
    }

    fn seek(&self, cylinder: u8, head: u8) -> Result<(), u8> {
        self.motor_on();
        self.send_byte(CMD_SEEK)?;
        self.send_byte((head << 2) | self.drive)?;
        self.send_byte(cylinder)?;

        // Время seek зависит от расстояния, берём с запасом
        // sleep((100 + (cylinder * 3)) as usize); // грубая оценка

        self.wait_for_command_phase()?;

        let (st0, _) = self.sense_interrupt()?;
        if (st0 & 0x20) == 0 {
            return Err(0x21);
        }
        Ok(())
    }

    fn lba_to_chs(&self, lba: u32) -> (u8, u8, u8) {
        let sector = ((lba % SPT as u32) + 1) as u8;
        let temp = lba / SPT as u32;
        let head = (temp % HEADS as u32) as u8;
        let cylinder = (temp / HEADS as u32) as u8;
        (cylinder, head, sector)
    }

    fn transfer(&self, lba: u32, numsects: u8, buf: u32, write: bool) -> Result<(), u8> {
        if !self.initialized.load(Ordering::SeqCst) {
            self.init()?;
        }

        if numsects == 0 || numsects > SPT {
            return Err(0x01);
        }
        let total_bytes = numsects as u32 * SECTOR_SIZE;
        let (cyl, head, sector) = self.lba_to_chs(lba);
        if cyl >= TRACKS {
            return Err(0x02);
        }

        // Проверяем, что не вылезаем за конец трека
        // if sector as u16 + numsects as u16 - 1 > SPT as u16 {
        //     println!("{:?} {:?}", sector as u16 + numsects as u16, SPT);
        //     return Err(0x03);
        // }

        self.motor_on();
        // Specify можно не вызывать каждый раз, но дёшево и безопасно
        self.specify()?;
        self.seek(cyl, head)?;

        // Используем статический bounce-буфер для DMA
        let dma_buf = validate_fdc_dma(total_bytes)?;

        // Копируем данные в bounce перед записью
        if write {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    buf as *const u8,
                    dma_buf as *mut u8,
                    total_bytes as usize,
                );
            }
        }

        // Настраиваем DMA
        setup_dma(dma_buf, total_bytes, !write)?;

        let cmd = if write { CMD_WRITE_DATA } else { CMD_READ_DATA };

        self.send_byte(cmd)?;
        self.send_byte((head << 2) | self.drive)?;
        self.send_byte(cyl)?;
        self.send_byte(head)?;
        self.send_byte(sector)?;
        self.send_byte(2)?;        // N = 2 → 512 байт
        self.send_byte(SPT)?;      // EOT
        self.send_byte(0x1B)?;     // GPL
        self.send_byte(0xFF)?;     // DTL (не используется при N≠0)

        // Ждём окончания выполнения (result phase)
        self.wait_for_result_phase()?;

        // Маскируем DMA-канал
        outb(DMA_CHAN_MASK, 0x06);

        // Читаем 7 байт результата
        let st0 = self.recv_byte()?;
        let st1 = self.recv_byte()?;
        let st2 = self.recv_byte()?;
        let _c  = self.recv_byte()?;
        let _h  = self.recv_byte()?;
        let _r  = self.recv_byte()?;
        let _n  = self.recv_byte()?;

        // IC (Interrupt Code) в ST0 bits 7:6
        // 00 = Normal Termination
        if (st0 & 0xC0) != 0 {
            return Err(0x30);
        }
        // Ошибки в ST1/ST2
        if (st1 & 0x37) != 0 || (st2 & 0x73) != 0 {
            return Err(0x31);
        }

        // Копируем данные из bounce после чтения
        if !write {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    dma_buf as *const u8,
                    buf as *mut u8,
                    total_bytes as usize,
                );
            }
        }

        println!("dma_buf {:02x?}", (dma_buf as *const u8));
        println!("buf {:02x?}", buf as *mut u8);

        Ok(())
    }
}

impl BlockDevice for Floppy {
    fn read_sectors(&self, numsects: u8, lba: u32, buf: u32) -> Result<(), u8> {
        self.transfer(lba, numsects, buf, false)
    }

    fn write_sectors(&mut self, numsects: u8, lba: u32, buf: u32) -> Result<(), u8> {
        self.transfer(lba, numsects, buf, true)
    }

    fn sector_size(&self) -> u32 {
        SECTOR_SIZE
    }
}