// DISK READER for FLOPPY — real hardware friendly
// INT 13h AH=02 (CHS), no DAP
use core::arch::asm;

pub static mut DISK: Disk = Disk { lba: 0, buffer: 0 };
const SECTOR_SIZE: u32 = 512;
const SPT: u16 = 18;
const HEADS: u16 = 2;
const MAX_RETRIES: u8 = 6;

pub struct Disk {
    lba: u32,
    buffer: u16,
}

impl Disk {
    pub fn init(&mut self, lba: u32, buffer: u16) {
        self.lba = lba;
        self.buffer = buffer;

        self.reset();
        self.delay();
    }

    fn check_ready(&self) {
        print!("T");
        let mut status: u16;
        unsafe {
            asm!(
            "mov ah, 0x10",
            "mov dl, 0x00",
            "int 0x13",
            "mov al, ah",
            "mov ah, 0",
            "jnc 1f",
            "or ax, 0x100",
            "1:",
            lateout("ax") status,
            lateout("dx") _,
            options(nostack),
            );
        }
        println!(" ready-status=0x{:x}", status);
    }

    fn reset(&self) {
        unsafe {
            asm!(
            "xor ax, ax",
            "mov dl, 0x00",
            "int 0x13",
            lateout("ax") _,
            lateout("dx") _,
            options(nostack),
            );
        }
    }

    fn delay(&self) {
        print!(".");
        unsafe {
            asm!(
            "mov cx, 0xFFFF",
            "2:",
            "loop 2b",
            "mov cx, 0xFFFF",
            "3:",
            "loop 3b",
            lateout("cx") _,
            options(nostack, nomem),
            );
        }
    }

    pub fn read_sector(&self) {
        let sector = ((self.lba % SPT as u32) + 1) as u16;
        let temp   = (self.lba / SPT as u32) as u16;
        let head   = temp % HEADS;
        let cyl    = temp / HEADS;

        let cx = (cyl << 8) | sector;
        let dx = (head << 8) | 0x00; // DL = 0 (Drive 0)

        for attempt in 0..MAX_RETRIES {
            print!(".");
            let mut err: u16;
            unsafe {
                // Прерывания должны быть включены для корректной работы BIOS
                asm!("sti", options(nostack));
                asm!(
                "mov ax, 0x0202",       // AH=02 (read), AL=1 (sector count)
                "int 0x13",
                // Если CF=0 (успех), err = 0. Иначе err = 1.
                "mov ax, 0",
                "jnc 4f",
                "inc ax",
                "4:",
                in("bx") self.buffer,
                in("cx") cx,
                in("dx") dx,
                lateout("ax") err,
                options(nostack),
                );
            }
            self.delay();

            if err == 0 {
                return; // Успешно прочитано
            }

            println!("retry");
            // При ошибке: сброс контроллера и задержка перед повтором
            self.reset();
            self.delay();
        }

        // Если все попытки исчерпаны
        unsafe { asm!("jmp fail", options(noreturn)); }
    }
    // Вставь эту функцию где-нибудь в disk.rs или в main.rs

    fn read_sectors_safe(&self, count: u16) {
        let sector = ((self.lba % SPT as u32) + 1) as u16;
        let temp   = (self.lba / SPT as u32) as u16;
        let head   = temp % HEADS;
        let cyl    = temp / HEADS;

        let cx = (cyl << 8) | sector;
        let dx = (head << 8) | 0x00;

        for attempt in 0..MAX_RETRIES {
            print!(".");
            let mut err: u16;

            // Вычисляем команду заранее
            let cmd: u16 = 0x0200 | count;   // AH=02, AL=count (1 или 2)

            unsafe {
                asm!("sti", options(nostack));

                asm!(
                "int 0x13",
                "mov ax, 0",
                "jnc 1f",
                "inc ax",
                "1:",
                inlateout("ax") cmd => err,   // ← вот так правильно
                in("bx") self.buffer,
                in("cx") cx,
                in("dx") dx,
                options(nostack),
                );
            }

            self.delay();

            if err == 0 {
                return;
            }

            println!("retry {}", err);
            self.reset();
            self.delay();
        }

        unsafe { asm!("jmp fail", options(noreturn)); }
    }

    pub fn read_sectors(&mut self, total_sectors: u16, target: u32) {
        let mut remaining = total_sectors;
        let mut dst = target;

        while remaining > 0 {
            // Сколько секторов осталось до конца текущего трека?
            let pos_in_track = (self.lba % SPT as u32) as u16;
            let sectors_to_track_end = SPT - pos_in_track;

            // Берём минимум из: оставшихся, 8 (максимум за раз), и сколько влезет в трек
            let batch = core::cmp::min(remaining, core::cmp::min(8, sectors_to_track_end));

            // Читаем batch секторов (1 или 2)
            self.read_sectors_safe(batch);

            // Копируем прочитанное в высокую память через защищённый режим
            let bytes_to_copy = (batch as u32) * SECTOR_SIZE;
            unsafe {
                copy_to_high_memory(self.buffer, dst, bytes_to_copy as usize);
            }

            self.lba += batch as u32;
            remaining -= batch;
            dst += bytes_to_copy;

            if (total_sectors - remaining) % 64 == 0 {
                print!(".");
            }
        }
        println!();
    }
}

// Вставь эту функцию где-нибудь в disk.rs или в main.rs

unsafe fn copy_to_high_memory(src_offset: u16, dst_phys: u32, len: usize) {
    print!(".");
    asm!(
    "cli",                    // отключаем прерывания на время
    "push ds",
    "push es",
    "push fs",
    "push gs",

    // Входим в protected mode
    "mov eax, cr0",
    "or al, 1",
    "mov cr0, eax",

    // Плоские сегменты
    "mov ax, 0x10",
    "mov ds, ax",
    "mov es, ax",

    // Копирование
    "movzx esi, {src:x}",
    "mov edi, {dst:e}",
    "mov ecx, {len:e}",
    "rep movsb",

    // Выходим из protected mode
    "mov eax, cr0",
    "and al, 0xfe",
    "mov cr0, eax",

    "pop gs",
    "pop fs",
    "pop es",
    "pop ds",
    "sti",                    // включаем прерывания обратно

    src = in(reg) src_offset,
    dst = in(reg) dst_phys,
    len = in(reg) len,
    out("eax") _,
    out("ecx") _,
    out("esi") _,
    out("edi") _,
    options(nostack),
    );
}