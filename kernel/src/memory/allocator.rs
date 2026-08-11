// kernel/src/memory/allocator.rs
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{self, null_mut};
use core::sync::atomic::{AtomicUsize, AtomicPtr, Ordering};
use interrupt_sync::SpinMutex;
use interrupt_sync::SpinMutexGuard;
use crate::println;

/// Заголовок свободного блока
#[repr(C)]
struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

pub struct Allocator {
    lock: SpinMutex<()>,
    bump_next: AtomicUsize,
    free_list: AtomicPtr<FreeBlock>,
}

impl Allocator {
    const HEAP_START: usize = 0xC140_0000;
    const HEAP_END:   usize = 0xC200_0000;  // ~12 MiB heap

    const HEADER_SIZE: usize = core::mem::size_of::<FreeBlock>();
    const HEADER_ALIGN: usize = core::mem::align_of::<FreeBlock>();

    pub const fn new() -> Self {
        Allocator {
            lock: SpinMutex::new(()),
            bump_next: AtomicUsize::new(Self::HEAP_START),
            free_list: AtomicPtr::new(null_mut()),
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

        unsafe { Layout::from_size_align_unchecked(size, align) }
    }

    /// Добавляем блок обратно в free list
    unsafe fn add_free_block(&self, ptr: *mut u8, size: usize) {
        let block = ptr as *mut FreeBlock;
        (*block).size = size;

        let mut current_head = self.free_list.load(Ordering::Acquire);
        loop {
            (*block).next = current_head;
            match self.free_list.compare_exchange(
                current_head,
                block,
                Ordering::Release,
                Ordering::Relaxed
            ) {
                Ok(_) => break,
                Err(head) => current_head = head,
            }
        }
    }
}

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _guard: SpinMutexGuard<()> = self.lock.lock();
        let layout = Self::adjust_layout(layout);
        let size = layout.size();
        let align = layout.align();

        // 1. Ищем в free list
        let mut prev: *mut FreeBlock = null_mut();
        let mut current = self.free_list.load(Ordering::Acquire);

        while !current.is_null() {
            let block_size = (*current).size;
            let block_ptr = current as *mut u8;

            if block_size >= size && (block_ptr as usize) % align == 0 {
                let next_block = (*current).next;

                if prev.is_null() {
                    match self.free_list.compare_exchange(
                        current,
                        next_block,
                        Ordering::AcqRel,
                        Ordering::Relaxed
                    ) {
                        Ok(_) => {
                            let remaining = block_size - size;
                            if remaining > Self::HEADER_SIZE * 2 {
                                let remainder_ptr = block_ptr.add(size);
                                self.add_free_block(remainder_ptr, remaining);
                            }
                            return block_ptr;
                        }
                        Err(_) => {
                            current = self.free_list.load(Ordering::Acquire);
                            prev = null_mut();
                            continue;
                        }
                    }
                } else {
                    if (*prev).next == current {
                        (*prev).next = next_block;
                        let remaining = block_size - size;
                        if remaining > Self::HEADER_SIZE * 2 {
                            let remainder_ptr = block_ptr.add(size);
                            self.add_free_block(remainder_ptr, remaining);
                        }
                        return block_ptr;
                    } else {
                        current = self.free_list.load(Ordering::Acquire);
                        prev = null_mut();
                        continue;
                    }
                }
            }

            prev = current;
            current = (*current).next;
        }

        // 2. Bump allocation
        let mut current_bump = self.bump_next.load(Ordering::Acquire);
        loop {
            let aligned = Self::align_up(current_bump, align);
            let new_next = aligned + size;

            if new_next > Self::HEAP_END {
                return null_mut();
            }

            match self.bump_next.compare_exchange(
                current_bump,
                new_next,
                Ordering::AcqRel,
                Ordering::Relaxed
            ) {
                Ok(_) => return aligned as *mut u8,
                Err(val) => current_bump = val,
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() {
            return;
        }
        let _guard: SpinMutexGuard<()> = self.lock.lock();
        let layout = Self::adjust_layout(layout);
        let size = layout.size();
        self.add_free_block(ptr, size);
    }
}

#[global_allocator]
pub(crate) static ALLOCATOR: Allocator = Allocator::new();
