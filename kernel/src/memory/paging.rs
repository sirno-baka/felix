use core::arch::asm;
use core::ptr::write_bytes;

const PAGE_SHIFT: usize = 12;
pub const PAGE_SIZE: usize = 1 << PAGE_SHIFT;
const ENTRIES: usize = 1024;

// Индекс для рекурсивного отображения (последняя запись каталога)
const RECURSIVE_INDEX: usize = 1023;
// Базовый виртуальный адрес для доступа к таблицам страниц
const VIRT_PT_BASE: u32 = 0xFFC00000;
// Виртуальный адрес самого каталога страниц (через рекурсию)
const VIRT_PD_BASE: u32 = 0xFFFFF000;

#[derive(Copy, Clone)]
pub struct PTEFlags(u32);

impl PTEFlags {
    pub const PRESENT: u32 = 1 << 0;
    pub const WRITABLE: u32 = 1 << 1;
    pub const USER: u32 = 1 << 2;
    pub const ACCESSED: u32 = 1 << 5;
    pub const DIRTY: u32 = 1 << 6;

    pub fn new() -> Self {
        Self(0)
    }

    pub fn present(mut self) -> Self {
        self.0 |= Self::PRESENT;
        self
    }

    pub fn writable(mut self) -> Self {
        self.0 |= Self::WRITABLE;
        self
    }

    pub fn user(mut self) -> Self {
        self.0 |= Self::USER;
        self
    }

    pub fn accessed(mut self) -> Self {
        self.0 |= Self::ACCESSED;
        self
    }

    pub fn dirty(mut self) -> Self {
        self.0 |= Self::DIRTY;
        self
    }

    pub fn bits(self) -> u32 {
        self.0
    }
}

#[derive(Copy, Clone)]
pub struct PDEFlags(u32);

impl PDEFlags {
    pub const PRESENT: u32 = 1 << 0;
    pub const WRITABLE: u32 = 1 << 1;
    pub const USER: u32 = 1 << 2;
    pub const ACCESSED: u32 = 1 << 5;
    pub const DIR_PAGE_SIZE: u32 = 1 << 7; // 4 MB page

    pub fn new() -> Self {
        Self(0)
    }

    pub fn present(mut self) -> Self {
        self.0 |= Self::PRESENT;
        self
    }

    pub fn writable(mut self) -> Self {
        self.0 |= Self::WRITABLE;
        self
    }

    pub fn user(mut self) -> Self {
        self.0 |= Self::USER;
        self
    }

    pub fn accessed(mut self) -> Self {
        self.0 |= Self::ACCESSED;
        self
    }

    pub fn large_page(mut self) -> Self {
        self.0 |= Self::DIR_PAGE_SIZE;
        self
    }

    pub fn bits(self) -> u32 {
        self.0
    }
}

#[repr(C, align(4096))]
pub struct PageDirectory {
    pub entries: [u32; ENTRIES],
}

impl PageDirectory {
    pub const fn new() -> Self {
        PageDirectory {
            entries: [0; ENTRIES],
        }
    }

    pub fn ensure_page_table(
        &mut self,
        pd_index: usize,
        alloc_frame: &mut dyn FnMut() -> u32,
    ) -> *mut [u32; ENTRIES] {
        let pde = &mut self.entries[pd_index];
        if *pde == 0 || (*pde & PDEFlags::PRESENT) == 0 {
            let pt_phys = alloc_frame(); // выделяем физическую страницу
            unsafe {
                // Очищаем таблицу через рекурсивное отображение
                let pt_ptr = Self::get_page_table_ptr(pd_index);
                write_bytes(pt_ptr as *mut u8, 0, 4096);
            }
            *pde = (pt_phys << 12) | PDEFlags::PRESENT | PDEFlags::WRITABLE;
            PageDirectory::flush_page((pd_index as u32) << 22);
        }
        Self::get_page_table_ptr(pd_index)
    }

    /// Отображает страницу (4 KiB) в уже существующую таблицу.
    /// Паникует, если таблица страниц отсутствует.
    pub fn map_existing(&mut self, virtual_page: u32, physical_page: u32, flags: PTEFlags) {
        let pd_index = (virtual_page >> 10) as usize;
        let pt_index = (virtual_page & 0x3FF) as usize;
        let pde = self.entries[pd_index];
        assert!(pde != 0 && (pde & PDEFlags::PRESENT) != 0, "Page table not present");
        let pt_ptr = Self::get_page_table_ptr(pd_index);
        unsafe {
            (*pt_ptr)[pt_index] = (physical_page << 12) | flags.bits();
        }
        PageDirectory::flush_page(virtual_page << 12);
    }

    /// Возвращает виртуальный указатель на таблицу страниц для заданного индекса каталога.
    /// Использует рекурсивное отображение.
    fn get_page_table_ptr(pd_index: usize) -> *mut [u32; ENTRIES] {
        let vaddr = VIRT_PT_BASE + (pd_index as u32 * 4096);
        vaddr as *mut [u32; ENTRIES]
    }

    /// Инициализирует рекурсивное отображение: последний PDE указывает на сам каталог.
    fn setup_recursive(&mut self, pd_phys: u32) {
        self.entries[RECURSIVE_INDEX] = pd_phys | PDEFlags::PRESENT | PDEFlags::WRITABLE;
    }

    /// Отображает страницу (4 KiB) по виртуальному адресу virtual_page (номер 4K страницы)
    /// на физическую страницу physical_page (номер 4K страницы).
    /// При необходимости создаёт недостающую таблицу страниц.
    pub fn map(&mut self, virtual_page: u32, physical_page: u32, flags: PTEFlags, alloc_frame: &mut dyn FnMut() -> u32) {
        let pd_index = (virtual_page >> 10) as usize; // 22: 10 бит из 32 (сдвиг на 22)
        let pt_index = (virtual_page & 0x3FF) as usize; // младшие 10 бит после сдвига на 12

        let pde = &mut self.entries[pd_index];
        if *pde == 0 || (*pde & PDEFlags::PRESENT) == 0 {
            // Таблица страниц отсутствует – выделяем физическую страницу для неё
            let pt_phys = alloc_frame(); // номер физической страницы
            // Очищаем таблицу страниц через рекурсивное отображение
            let pt_ptr = Self::get_page_table_ptr(pd_index);
            unsafe {
                write_bytes(pt_ptr as *mut u8, 0, 4096);
            }
            // Записываем PDE с флагами
            *pde = (pt_phys << 12) | PDEFlags::PRESENT | PDEFlags::WRITABLE;
            // Сбросим TLB для этого адреса (не обязательно, но безопаснее)
            PageDirectory::flush_page((virtual_page << 12) as u32);
        }

        // Теперь таблица страниц существует – обновляем PTE
        let pt_ptr = Self::get_page_table_ptr(pd_index);
        unsafe {
            (*pt_ptr)[pt_index] = (physical_page << 12) | flags.bits();
        }
        PageDirectory::flush_page((virtual_page << 12) as u32);
    }

    /// Отображает большую страницу (4 MiB).
    pub fn map_large(&mut self, virtual_page: u32, physical_page: u32, flags: PDEFlags) {
        let pd_index = (virtual_page >> 10) as usize; // для 4M страниц номер вирт. страницы – это номер 4M блока
        self.entries[pd_index] = (physical_page << 22) | flags.bits();
        PageDirectory::flush_page((virtual_page << 22) as u32);
    }

    /// Удаляет отображение для заданного виртуального адреса (выравненного по странице).
    pub fn unmap(&mut self, virtual_addr: u32) {
        let pd_index = (virtual_addr >> 22) as usize;
        let pt_index = ((virtual_addr >> 12) & 0x3FF) as usize;

        let entry = self.entries[pd_index];
        if entry == 0 || (entry & PDEFlags::PRESENT) == 0 {
            return;
        }

        if entry & PDEFlags::DIR_PAGE_SIZE != 0 {
            self.entries[pd_index] = 0;
        } else {
            let pt_ptr = Self::get_page_table_ptr(pd_index);
            unsafe {
                (*pt_ptr)[pt_index] = 0;
            }
        }
        PageDirectory::flush_page(virtual_addr);
    }

    /// Транслирует виртуальный адрес в физический.
    pub fn translate(&self, virtual_addr: u32) -> Option<u32> {
        let pd_index = (virtual_addr >> 22) as usize;
        let pt_index = ((virtual_addr >> 12) & 0x3FF) as usize;
        let offset = virtual_addr & 0xFFF;

        let pd_entry = self.entries[pd_index];
        if pd_entry & PDEFlags::PRESENT == 0 {
            return None;
        }

        if pd_entry & PDEFlags::DIR_PAGE_SIZE != 0 {
            // Большая страница 4 MiB
            let phys_base = pd_entry & 0xFFC00000; // биты 22-31
            Some(phys_base | (virtual_addr & 0x3FFFFF))
        } else {
            let pt_ptr = Self::get_page_table_ptr(pd_index);
            let pt_entry = unsafe { (*pt_ptr)[pt_index] };
            if pt_entry & PTEFlags::PRESENT == 0 {
                return None;
            }
            let phys_page = pt_entry >> 12;
            Some((phys_page << 12) | offset)
        }
    }

    /// Загружает физический адрес этого каталога в CR3.
    /// Предполагается, что каталог находится в памяти, отображённой identity (вирт. адрес == физ. адрес).
    pub fn switch(&self) {
        let phys = self as *const PageDirectory as u32;
        unsafe {
            asm!("mov cr3, {}", in(reg) phys);
        }
    }

    pub fn flush_all() {
        unsafe {
            let cr3: u32;
            asm!("mov {}, cr3", out(reg) cr3);
            asm!("mov cr3, {}", in(reg) cr3);
        }
    }

    pub fn flush_page(addr: u32) {
        unsafe {
            asm!("invlpg [{0}]", in(reg) addr);
        }
    }
}

pub struct PageManager {
    pub dir: PageDirectory,
    next_free_page: u32, // номер следующей свободной физической страницы (4 KiB)
}

impl PageManager {
    pub const fn new() -> Self {
        PageManager {
            dir: PageDirectory::new(),
            next_free_page: 0,
        }
    }

    /// Выделить одну физическую страницу (4 KiB)
    pub fn alloc_phys_frame(&mut self) -> u32 {
        let frame = self.next_free_page;
        self.next_free_page += 1;
        frame
    }

    /// Освободить физическую страницу (пока не реализовано – заглушка)
    /// TODO: настоящий аллокатор с битовой картой
    pub fn free_phys_frame(&mut self, _frame: u32) {
        // Пока просто игнорируем – память утекает, но это лучше паники
        // В реальном ядре нужно вернуть страницу в пул свободных
    }

    /// Выделяет физическую страницу (возвращает её номер).
    /// В простейшей реализации просто выдаёт страницы последовательно.
    fn alloc_frame(&mut self) -> u32 {
        let frame = self.next_free_page;
        self.next_free_page += 1;
        frame
    }

    /// Инициализация пагинации.
    /// `kernel_end` – первый байт за пределами ядра (физический адрес).
    pub fn init(&mut self, kernel_end: u32) {
        // Включаем PSE (поддержка больших страниц 4 MiB)
        let cr4: u32;
        unsafe {
            asm!("mov {}, cr4", out(reg) cr4);
            let cr4 = cr4 | (1 << 4);
            asm!("mov cr4, {}", in(reg) cr4);
        }

        // Identity‑mapping первых 32 MB через большие страницы (8 штук по 4 MB)
        for i in 0..8u32 {
            let virt_page = i << 10; // номер 4M страницы: 0, 1, ... 7
            let phys_page = i << 10;
            self.dir.map_large(
                virt_page,
                phys_page,
                PDEFlags::new().present().writable().accessed().user().large_page(),
            );
        }

        // Устанавливаем рекурсивное отображение (последняя запись каталога)
        let pd_phys = &self.dir as *const PageDirectory as u32;
        self.dir.setup_recursive(pd_phys);
        self.dir.switch();

        let start_page = (32 * 1024 * 1024) >> 12;
        let end_page = (kernel_end + 4095) >> 12;

        for vpage in start_page..end_page {
            let pd_idx = (vpage >> 10) as usize;
            let pt_idx = (vpage & 0x3FF) as usize;

            // Проверяем наличие таблицы без мутабельного заимствования
            let need_table = {
                let pde = self.dir.entries[pd_idx];
                pde == 0 || (pde & PDEFlags::PRESENT) == 0
            };

            if need_table {
                let pt_phys = self.alloc_frame();

                // КРИТИЧНО: правильные флаги для PDE (user-mode доступ)
                let pde_flags = PDEFlags::new()
                    .present()
                    .writable()
                    .user()           // ← вот этого не хватало
                    .bits();

                let pde = &mut self.dir.entries[pd_idx];
                *pde = (pt_phys << 12) | pde_flags;

                // Сбрасываем TLB
                PageDirectory::flush_page((pd_idx as u32) << 22);

                // Очищаем таблицу страниц
                let pt_ptr = PageDirectory::get_page_table_ptr(pd_idx);
                unsafe {
                    write_bytes(pt_ptr as *mut u8, 0, 4096);
                }
            }

            // Заполняем PTE
            let pt_ptr = PageDirectory::get_page_table_ptr(pd_idx);
            unsafe {
                (*pt_ptr)[pt_idx] = (vpage << 12) | PTEFlags::PRESENT | PTEFlags::WRITABLE | PTEFlags::USER | PTEFlags::ACCESSED;
            }
            PageDirectory::flush_page((vpage << 12) as u32);
        }

        // Устанавливаем следующий свободный номер фрейма
        self.next_free_page = end_page;

        // Загружаем наш каталог страниц
        self.dir.switch();

        // Включаем страничную адресацию (бит PG в CR0)
        unsafe {
            let cr0: u32;
            asm!("mov {}, cr0", out(reg) cr0);
            let cr0 = cr0 | (1 << 31);
            asm!("mov cr0, {}", in(reg) cr0);
        }
    }

    pub fn alloc_and_map(&mut self, virt: u32) -> u32 {
        let vpage = virt >> 12;
        let pd_idx = (vpage >> 10) as usize;
        let pt_idx = (vpage & 0x3FF) as usize;

        // Проверяем, нужна ли новая таблица страниц
        let need_table = {
            let pde = self.dir.entries[pd_idx];
            pde == 0 || (pde & PDEFlags::PRESENT) == 0
        };

        if need_table {
            let pt_phys = self.alloc_frame();

            // КРИТИЧНО: сначала ставим PDE → рекурсивное отображение заработает
            let pde = &mut self.dir.entries[pd_idx];
            let pde_flags = PDEFlags::new()
                .present()
                .writable()
                .user()
                .bits();

            *pde = (pt_phys << 12) | pde_flags;

            // Сбрасываем TLB
            PageDirectory::flush_page((pd_idx as u32) << 22);

            // Теперь можно безопасно обнулять таблицу через рекурсию
            let pt_ptr = PageDirectory::get_page_table_ptr(pd_idx);
            unsafe {
                write_bytes(pt_ptr as *mut u8, 0, 4096);
            }
        }

        // Выделяем физическую страницу для данных пользователя
        let phys_frame = self.alloc_frame();

        // Записываем PTE
        let pt_ptr = PageDirectory::get_page_table_ptr(pd_idx);
        unsafe {
            (*pt_ptr)[pt_idx] = (phys_frame << 12)
                | PTEFlags::PRESENT
                | PTEFlags::WRITABLE
                | PTEFlags::USER
                | PTEFlags::DIRTY;
        }

        PageDirectory::flush_page(virt);
        phys_frame
    }

    /// Возвращает физический адрес каталога страниц (для отладки)
    pub fn dir_phys(&self) -> u32 {
        &self.dir as *const PageDirectory as u32
    }
}

// Глобальный экземпляр менеджера страниц
pub static mut PAGING: PageManager = PageManager::new();