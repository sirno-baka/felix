use core::arch::asm;
use core::ptr::write_bytes;
use core::fmt;
use interrupt_sync::SpinMutex;
use crate::println;
use crate::sync::mutex::Mutex;

const PAGE_SHIFT: usize = 12;
pub const PAGE_SIZE: usize = 1 << PAGE_SHIFT;
const ENTRIES: usize = 1024;

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct VirtAddr(pub u32);

impl VirtAddr {
    pub const fn null() -> Self {
        VirtAddr(0)
    }
    pub const fn is_null(&self) -> bool {
        self.0 == 0
    }
    pub const fn page_align(&self) -> Self {
        VirtAddr(self.0 & !(PAGE_SIZE as u32 - 1))
    }
}

impl fmt::Display for VirtAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:08x}", self.0)
    }
}

impl From<u32> for VirtAddr {
    fn from(v: u32) -> Self {
        VirtAddr(v)
    }
}

impl From<usize> for VirtAddr {
    fn from(v: usize) -> Self {
        VirtAddr(v as u32)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct PhysAddr(pub u32);

impl PhysAddr {
    pub const fn null() -> Self {
        PhysAddr(0)
    }
    pub const fn is_null(&self) -> bool {
        self.0 == 0
    }
    pub const fn page_align(&self) -> Self {
        PhysAddr(self.0 & !(PAGE_SIZE as u32 - 1))
    }
    pub const fn page_offset(&self) -> u32 {
        self.0 & (PAGE_SIZE as u32 - 1)
    }
    pub const fn page_number(&self) -> u32 {
        self.0 >> PAGE_SHIFT
    }
}

impl fmt::Display for PhysAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:08x}", self.0)
    }
}

impl From<u32> for PhysAddr {
    fn from(v: u32) -> Self {
        PhysAddr(v)
    }
}

impl From<usize> for PhysAddr {
    fn from(v: usize) -> Self {
        PhysAddr(v as u32)
    }
}

// Индекс для рекурсивного отображения (последняя запись каталога)
const RECURSIVE_INDEX: usize = 1023;
// Базовый виртуальный адрес для доступа к таблицам страниц
const VIRT_PT_BASE: u32 = 0xFFC00000;
// Виртуальный адрес самого каталога страниц (через рекурсию)
const VIRT_PD_BASE: u32 = 0xFFFFF000;

static mut USER_HEAP_NEXT: u32 = 0x0010_0000; // Начало памяти для пользовательских задач (после нулевой страницы)

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
    pub const DIR_PAGE_SIZE: u32 = 1 << 7;

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

#[derive(Copy, Clone)]
#[repr(C, align(4096))]
pub struct PageDirectory {
    pub entries: [u32; ENTRIES],
    pub next_free_frame: u32,
}

impl PageDirectory {
    pub const fn new() -> Self {
        PageDirectory {
            entries: [0; ENTRIES],
            next_free_frame: 0,
        }
    }

    /// Делегирует выделение в глобальный FrameAllocator (PAGING).
    /// Это главное исправление — теперь все таски получают уникальные физические фреймы.
    pub fn alloc_frame(&mut self) -> u32 {
        unsafe {
            PAGING.lock().alloc_frame()
        }
    }

    pub fn ensure_page_table(
        &mut self,
        pd_index: usize,
        alloc_frame: &mut dyn FnMut() -> u32,
    ) -> *mut [u32; ENTRIES] {
        let pde = &mut self.entries[pd_index];
        if *pde == 0 || (*pde & PDEFlags::PRESENT) == 0 {
            let pt_phys = alloc_frame();
            let pt_ptr = Self::get_page_table_ptr(pd_index);
            unsafe {
                write_bytes(pt_ptr as *mut u8, 0, 4096);
            }
            *pde = (pt_phys << 12) | PDEFlags::PRESENT | PDEFlags::WRITABLE;
            PageDirectory::flush_page((pd_index as u32) << 22);
        }
        Self::get_page_table_ptr(pd_index)
    }

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

    pub(crate) fn get_page_table_ptr(pd_index: usize) -> *mut [u32; ENTRIES] {
        let vaddr = VIRT_PT_BASE + (pd_index as u32 * 4096);
        vaddr as *mut [u32; ENTRIES]
    }

    fn setup_recursive(&mut self, pd_phys: u32) {
        self.entries[RECURSIVE_INDEX] = pd_phys | PDEFlags::PRESENT | PDEFlags::WRITABLE;
    }

    pub fn map(&mut self, virtual_page: u32, physical_page: u32, flags: PTEFlags, alloc_frame: &mut dyn FnMut() -> u32) {
        let pd_index = (virtual_page >> 10) as usize;
        let pt_index = (virtual_page & 0x3FF) as usize;

        let pde = &mut self.entries[pd_index];
        if *pde == 0 || (*pde & PDEFlags::PRESENT) == 0 {
            let pt_phys = alloc_frame();
            let pt_ptr = Self::get_page_table_ptr(pd_index);
            unsafe {
                write_bytes(pt_ptr as *mut u8, 0, 4096);
            }

            let pde_flags = PDEFlags::new()
                .present()
                .writable()
                .user()
                .bits();

            *pde = (pt_phys << 12) | pde_flags;
            PageDirectory::flush_page((virtual_page << 12) as u32);
        }

        let pt_ptr = Self::get_page_table_ptr(pd_index);
        unsafe {
            (*pt_ptr)[pt_index] = (physical_page << 12) | flags.bits();
        }
        PageDirectory::flush_page((virtual_page << 12) as u32);
    }

    pub fn map_large(&mut self, virtual_addr: u32, physical_addr: u32, flags: PDEFlags) {
        let pd_index = (virtual_addr >> 22) as usize;
        let pde_value = (physical_addr & 0xFFC00000)
            | flags.bits()
            | PDEFlags::DIR_PAGE_SIZE;
        self.entries[pd_index] = pde_value;
        PageDirectory::flush_page(virtual_addr & 0xFFC00000);
    }

    pub fn unmap(&mut self, virtual_addr: u32) {
        let pd_index = (virtual_addr >> 22) as usize;
        if pd_index >= ENTRIES {
            return;
        }
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

    pub fn translate(&self, virtual_addr: u32) -> Option<u32> {
        let pd_index = (virtual_addr >> 22) as usize;
        let pt_index = ((virtual_addr >> 12) & 0x3FF) as usize;
        let offset = virtual_addr & 0xFFF;

        let pd_entry = self.entries[pd_index];
        if pd_entry & PDEFlags::PRESENT == 0 {
            return None;
        }

        if pd_entry & PDEFlags::DIR_PAGE_SIZE != 0 {
            let phys_base = pd_entry & 0xFFC00000;
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

    pub fn switch(&self) {
        let pd_phys = &self.entries as *const [u32; ENTRIES] as u32;
        unsafe {
            asm!("mov cr3, {}", in(reg) pd_phys);
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

    pub fn alloc_and_map_user_page(&mut self, virt_addr: u32) {
        let vpage = virt_addr >> 12;
        let pd_idx = (vpage >> 10) as usize;
        let pt_idx = (vpage & 0x3FF) as usize;

        let need_table = {
            let pde = self.entries[pd_idx];
            pde == 0 || (pde & PDEFlags::PRESENT) == 0
        };

        if need_table {
            let pt_phys = self.alloc_frame();
            let pde_flags = PDEFlags::new()
                .present()
                .writable()
                .user()
                .bits();

            self.entries[pd_idx] = (pt_phys << 12) | pde_flags;
            PageDirectory::flush_page((pd_idx as u32) << 22);

            // Старый способ обнуления PT (через физический адрес)
            let pt_virt = (pt_phys << 12) as *mut u8;
            unsafe {
                write_bytes(pt_virt, 0, 4096);
            }
        }

        let pde = self.entries[pd_idx];
        let pt_phys = (pde >> 12) << 12;
        let pt_virt = pt_phys as *mut [u32; ENTRIES];

        // Если страница уже смаплена — не трогаем (не теряем данные!).
        let existing_pte = unsafe { (*pt_virt)[pt_idx] };
        if existing_pte & PTEFlags::PRESENT != 0 {
            return;
        }

        let phys_frame = self.alloc_frame();

        unsafe {
            (*pt_virt)[pt_idx] = (phys_frame << 12)
                | PTEFlags::PRESENT
                | PTEFlags::WRITABLE
                | PTEFlags::USER
                | PTEFlags::DIRTY;
        }

        PageDirectory::flush_page(virt_addr);
    }
}

pub struct PageManager {
    pub dir: PageDirectory,
    pub next_free_page: u32,
}

impl PageManager {
    pub const fn new() -> Self {
        PageManager {
            dir: PageDirectory::new(),
            next_free_page: 0,
        }
    }

    pub fn alloc_phys_frame(&mut self) -> u32 {
        let frame = self.next_free_page;
        self.next_free_page += 1;
        frame
    }

    pub fn free_phys_frame(&mut self, _frame: u32) {
    }

    fn alloc_frame(&mut self) -> u32 {
        let frame = self.next_free_page;
        self.next_free_page += 1;
        frame
    }

    pub fn init(&mut self, kernel_end: u32) {
        let cr4: u32;
        unsafe {
            asm!("mov {}, cr4", out(reg) cr4);
            let cr4 = cr4 | (1 << 4);
            asm!("mov cr4, {}", in(reg) cr4);
        }

        for i in 0..8u32 {
            let base = i * 0x400000u32; // 4 MiB large pages, identity mapped
            self.dir.map_large(
                base,
                base,
                PDEFlags::new().present().writable().accessed().user().large_page(),
            );
        }

        let pd_phys = &self.dir as *const PageDirectory as u32;
        self.dir.setup_recursive(pd_phys);
        self.dir.switch();

        // Маппим первые 8MB (включая VGA 0xB8000) и область ядра 0xC000_0000
        let start_page = 0; // Начинаем с нуля для VGA и низкой памяти
        let kernel_start_page = (0xC000_0000 >> 12) as usize;
        let kernel_end_page = ((kernel_end + 4095) >> 12) as usize;
        
        // Сначала маппим низкую память (0 - 8MB) для VGA и пользовательских задач
        for vpage in start_page..(8 * 1024 / 4) {
            let pd_idx = (vpage >> 10) as usize;
            let pt_idx = (vpage & 0x3FF) as usize;

            let need_table = {
                let pde = self.dir.entries[pd_idx];
                pde == 0 || (pde & PDEFlags::PRESENT) == 0
            };

            if need_table {
                let pt_phys = self.alloc_frame();

                let pde = &mut self.dir.entries[pd_idx];
                let pde_flags = PDEFlags::new()
                    .present()
                    .writable()
                    .user()
                    .bits();

                *pde = (pt_phys << 12) | pde_flags;

                PageDirectory::flush_page((pd_idx as u32) << 22);

                let pt_ptr = PageDirectory::get_page_table_ptr(pd_idx);
                unsafe {
                    write_bytes(pt_ptr as *mut u8, 0, 4096);
                }
            }

            let pt_ptr = PageDirectory::get_page_table_ptr(pd_idx);
            unsafe {
                (*pt_ptr)[pt_idx] = (vpage << 12) | PTEFlags::PRESENT | PTEFlags::WRITABLE | PTEFlags::USER | PTEFlags::ACCESSED;
            }
            PageDirectory::flush_page((vpage << 12) as u32);
        }
        
        // Теперь маппим ядро по адресу 0xC000_0000
        for vpage in kernel_start_page..kernel_end_page {
            let phys_page = vpage - kernel_start_page; // Физически ядро начинается с 0 после загрузчика
            let pd_idx = (vpage >> 10) as usize;
            let pt_idx = (vpage & 0x3FF) as usize;

            let need_table = {
                let pde = self.dir.entries[pd_idx];
                pde == 0 || (pde & PDEFlags::PRESENT) == 0
            };

            if need_table {
                let pt_phys = self.alloc_frame();

                let pde = &mut self.dir.entries[pd_idx];
                let pde_flags = PDEFlags::new()
                    .present()
                    .writable()
                    .user()
                    .bits();

                *pde = (pt_phys << 12) | pde_flags;

                PageDirectory::flush_page((pd_idx as u32) << 22);

                let pt_ptr = PageDirectory::get_page_table_ptr(pd_idx);
                unsafe {
                    write_bytes(pt_ptr as *mut u8, 0, 4096);
                }
            }

            let pt_ptr = PageDirectory::get_page_table_ptr(pd_idx);
            unsafe {
                (*pt_ptr)[pt_idx] = (phys_page << 12) | PTEFlags::PRESENT | PTEFlags::WRITABLE | PTEFlags::USER | PTEFlags::ACCESSED;
            }
            PageDirectory::flush_page((vpage << 12) as u32);
        }

        self.next_free_page = kernel_end_page as u32;

        self.dir.switch();

        unsafe {
            let cr0: u32;
            asm!("mov {}, cr0", out(reg) cr0);
            let cr0 = cr0 | (1 << 31);
            asm!("mov cr0, {}", in(reg) cr0);
        }
    }

    pub fn alloc_user_memory(&mut self, size: u32) -> u32 {
        if size == 0 {
            return 0;
        }

        let start = unsafe { USER_HEAP_NEXT };

        let pages = (size + 4095) / 4096;
        for i in 0..pages {
            let virt_addr = start + (i as u32 * 4096);
            self.alloc_and_map(virt_addr);
        }

        unsafe {
            USER_HEAP_NEXT = start + (pages as u32 * 4096);
        }

        start
    }

    pub fn alloc_and_map(&mut self, virt: u32) -> u32 {
        let vpage = virt >> 12;
        let pd_idx = (vpage >> 10) as usize;
        let pt_idx = (vpage & 0x3FF) as usize;

        let need_table = {
            let pde = self.dir.entries[pd_idx];
            pde == 0 || (pde & PDEFlags::PRESENT) == 0
        };

        if need_table {
            let pt_phys = self.alloc_frame();

            let pde = &mut self.dir.entries[pd_idx];
            let pde_flags = PDEFlags::new()
                .present()
                .writable()
                .user()
                .bits();

            *pde = (pt_phys << 12) | pde_flags;

            PageDirectory::flush_page((pd_idx as u32) << 22);
            let pt_virt = VIRT_PT_BASE + (pd_idx as u32) * PAGE_SIZE as u32;
            PageDirectory::flush_page(pt_virt);

            let pt_ptr = PageDirectory::get_page_table_ptr(pd_idx);
            unsafe {
                write_bytes(pt_ptr as *mut u8, 0, 4096);
            }
        }

        let phys_frame = self.alloc_frame();

        let pt_ptr = PageDirectory::get_page_table_ptr(pd_idx);
        unsafe {
            (*pt_ptr)[pt_idx] = (phys_frame << 12)
                | PTEFlags::PRESENT
                | PTEFlags::WRITABLE
                | PTEFlags::USER
                | PTEFlags::DIRTY;
        }

        PageDirectory::flush_page(virt);

        unsafe {
            core::ptr::write_bytes(virt as *mut u8, 0, PAGE_SIZE);
        }
        PageDirectory::flush_page(virt);

        phys_frame
    }

    pub fn dir_phys(&self) -> u32 {
        &self.dir.entries as *const [u32; ENTRIES] as u32
    }
}

pub fn setup_kernel_page_dir(pd: &mut PageManager, end_page: u32) {
    pd.dir = PageDirectory::new();
    let pd_phys = &pd.dir as *const PageDirectory as u32;
    pd.dir.setup_recursive(pd_phys);
    pd.dir.switch();

    for i in 0..8u32 {
        let base = i * 0x400000u32; // 4 MiB large pages, identity mapped
        pd.dir.map_large(
            base,
            base,
            PDEFlags::new().present().writable().accessed().user().large_page(),
        );
    }

    for vpage in (32 * 1024 * 1024 >> 12)..end_page {
        let pd_idx = (vpage >> 10) as usize;
        let pt_idx = (vpage & 0x3FF) as usize;

        let need_table = {
            let pde = pd.dir.entries[pd_idx];
            pde == 0 || (pde & PDEFlags::PRESENT) == 0
        };

        if need_table {
            let pt_phys = pd.alloc_frame();
            let pde = &mut pd.dir.entries[pd_idx];
            let pde_flags = PDEFlags::new()
                .present()
                .writable()
                .user()
                .bits();
            *pde = (pt_phys << 12) | pde_flags;
            PageDirectory::flush_page((pd_idx as u32) << 22);
            let pt_ptr = PageDirectory::get_page_table_ptr(pd_idx);
            unsafe {
                write_bytes(pt_ptr as *mut u8, 0, 4096);
            }
        }

        let pt_ptr = PageDirectory::get_page_table_ptr(pd_idx);
        unsafe {
            (*pt_ptr)[pt_idx] = (vpage << 12) | PTEFlags::PRESENT | PTEFlags::WRITABLE | PTEFlags::USER | PTEFlags::ACCESSED;
        }
        PageDirectory::flush_page((vpage << 12) as u32);
    }

    pd.dir.switch();
}

pub static mut KERNEL_PD_PHYS: u32 = 0;
pub static mut KERNEL_END_PAGE: u32 = 0;

pub fn copy_kernel_mappings(task_dir: &mut PageDirectory, task_pd_phys: u32) {
    unsafe {
        // 1. Копируем identity mapping первых 32 МБ (large pages) — обязательно!
        for i in 0..8u32 {
            let base = i * 0x400000u32; // 4 MiB large pages, identity mapped
            task_dir.map_large(
                base,
                base,
                PDEFlags::new().present().writable().accessed().user().large_page(),
            );
        }

        let end_page = KERNEL_END_PAGE;
        let kernel_pd_phys = KERNEL_PD_PHYS;

        if kernel_pd_phys == 0 {
            println!("[copy_kernel_mappings] ERROR: KERNEL_PD_PHYS not set!");
            return;
        }

        // println!("[copy] Copying kernel mappings up to page {}", end_page);

        // 2. Копируем все kernel-страницы (от 32MB и выше)
        for pd_idx in 0..1024u32 {
            let start_page = pd_idx * 1024;
            if start_page >= end_page {
                break;
            }

            let global_pde = *(kernel_pd_phys as *const u32).add(pd_idx as usize);

            if (global_pde & PDEFlags::PRESENT) == 0 {
                continue;
            }

            let global_pt_phys = (global_pde >> 12) << 12;
            let global_pt = global_pt_phys as *const [u32; 1024];

            let need_table = {
                let tde = task_dir.entries[pd_idx as usize];
                tde == 0 || (tde & PDEFlags::PRESENT) == 0
            };

            if need_table {
                let pt_phys = task_dir.alloc_frame();
                let pde = &mut task_dir.entries[pd_idx as usize];
                let pde_flags = PDEFlags::new()
                    .present()
                    .writable()
                    .user()
                    .bits();
                *pde = (pt_phys << 12) | pde_flags;
                PageDirectory::flush_page(pd_idx << 22);
                let pt_ptr = PageDirectory::get_page_table_ptr(pd_idx as usize);
                core::ptr::write_bytes(pt_ptr as *mut u8, 0, 4096);
            }

            let task_pt = PageDirectory::get_page_table_ptr(pd_idx as usize);
            let max_pt = if start_page + 1024 > end_page {
                (end_page - start_page) as usize
            } else {
                1024
            };

            for pt_idx in 0..max_pt {
                let pte = (*global_pt)[pt_idx];
                if (pte & PTEFlags::PRESENT) == 0 {
                    continue;
                }
                (*task_pt)[pt_idx] = pte;
            }
        }

        // === САМОЕ ВАЖНОЕ ===
        task_dir.entries[RECURSIVE_INDEX] = task_pd_phys
            | PDEFlags::PRESENT
            | PDEFlags::WRITABLE;

        // println!("[copy] Kernel mappings + recursive entry copied successfully for task PD phys = 0x{:08x}", task_pd_phys);
    }
}

/// Глобальный экземпляр менеджера страниц
pub static mut PAGING: SpinMutex<PageManager> = SpinMutex::new(PageManager::new());
