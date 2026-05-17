// kernel/src/memory/allocator.rs
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{self, null_mut};
use core::sync::atomic::{AtomicUsize, Ordering};
use crate::println;

/// Заголовок свободного блока
#[repr(C)]
struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

pub struct Allocator {
    bump_next: AtomicUsize,
    free_list: AtomicUsize, // *mut FreeBlock
}

impl Allocator {
    const HEAP_START: usize = 0x0040_0000;
    const HEAP_END:   usize = 0x0f00_0000;

    const HEADER_SIZE: usize = core::mem::size_of::<FreeBlock>();
    const HEADER_ALIGN: usize = core::mem::align_of::<FreeBlock>();

    pub const fn new() -> Self {
        Allocator {
            bump_next: AtomicUsize::new(Self::HEAP_START),
            free_list: AtomicUsize::new(0),
        }
    }

    fn align_up(addr: usize, align: usize) -> usize {
        (addr + align - 1) & !(align - 1)
    }

    /// Превращаем пользовательский Layout в "внутренний" с гарантированным выравниванием
    fn adjust_layout(layout: Layout) -> Layout {
        let align = layout.align().max(Self::HEADER_ALIGN);
        let size = layout.size().max(Self::HEADER_SIZE);
        let size = Self::align_up(size, align);

        // Безопасно, т.к. оригинальный Layout был валидным
        unsafe { Layout::from_size_align_unchecked(size, align) }
    }

    /// Добавляем блок обратно в free list
    unsafe fn add_free_block(&self, ptr: *mut u8, size: usize) {
        let block = ptr as *mut FreeBlock;
        (*block).size = size;
        (*block).next = self.free_list.load(Ordering::Relaxed) as *mut FreeBlock;

        self.free_list.store(block as usize, Ordering::Release);
    }
}

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let layout = Self::adjust_layout(layout); // ← главный фикс
        let size = layout.size();
        let align = layout.align();
        // 1. Сначала ищем в free list (с проверкой выравнивания!)
        let mut prev: *mut FreeBlock = null_mut();
        let mut current: *mut FreeBlock = self.free_list.load(Ordering::Acquire) as *mut FreeBlock;

        while !current.is_null() {
            let block_size = (*current).size;
            let block_ptr = current as *mut u8;

            // Теперь учитываем и размер, и требуемое выравнивание
            if block_size >= size && (block_ptr as usize) % align == 0 {
                // удаляем из списка
                if prev.is_null() {
                    self.free_list.store((*current).next as usize, Ordering::Release);
                } else {
                    (*prev).next = (*current).next;
                }

                // сплитим остаток, если он достаточно большой
                let remaining = block_size - size;
                if remaining > Self::HEADER_SIZE * 2 {
                    let remainder_ptr = block_ptr.add(size);
                    // remainder_ptr теперь гарантированно выровнен
                    self.add_free_block(remainder_ptr, remaining);
                }

                return block_ptr;
            }

            prev = current;
            current = (*current).next;
        }

        // 2. Bump allocation
        let current = self.bump_next.load(Ordering::Relaxed);
        let aligned = Self::align_up(current, align);
        let new_next = aligned + size;

        if new_next > Self::HEAP_END {
            return null_mut(); // Out of memory
        }

        self.bump_next.store(new_next, Ordering::Relaxed);
        aligned as *mut u8
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() {
            return;
        }
        let layout = Self::adjust_layout(layout);
        let size = layout.size();
        self.add_free_block(ptr, size);
        let free = self.free_list.load(Ordering::Acquire);
    }
}

#[global_allocator]
pub(crate) static ALLOCATOR: Allocator = Allocator::new();