// kernel/src/memory/allocator.rs
use crate::println;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{self, null_mut};
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use interrupt_sync::SpinMutex;
use interrupt_sync::SpinMutexGuard;

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
    // Boot stack: ESP=0xC1600000 grows DOWN to ~0xC1200000.
    // Leave a 2 MiB guard between stack top and heap so a deep call or
    // interrupt frame cannot smash the first heap objects.
    //
    // After -m 128M / LARGE_PAGE_COUNT=32 the higher-half large pages cover
    // phys 0..128 MiB. Kernel heap lives in the already-mapped window:
    //   virt 0xC1800000..0xC2800000  →  phys 0x01800000..0x02800000 (16 MiB).
    // Frame allocator (user PTs, stacks, surfaces) starts at FRAME_ALLOC_START
    // = 0x02800000 so it never collides with this region.
    const HEAP_START: usize = 0xC180_0000;
    const HEAP_END: usize = 0xC280_0000;

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
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(head) => current_head = head,
            }
        }
    }
}

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // MUST cli for the whole critical section. Timer IRQ runs smoltcp which
        // allocates; without cli that re-enters the free-list and either deadlocks
        // on SpinMutex or corrupts VecDeque metadata.
        interrupt_sync::without_interrupts(|| {
            let _guard: SpinMutexGuard<()> = self.lock.lock();
            self.alloc_inner(layout)
        })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() {
            return;
        }
        interrupt_sync::without_interrupts(|| {
            let _guard: SpinMutexGuard<()> = self.lock.lock();
            let layout = Self::adjust_layout(layout);
            self.add_free_block(ptr, layout.size());
        })
    }
}

impl Allocator {
    unsafe fn alloc_inner(&self, layout: Layout) -> *mut u8 {
        let layout = Self::adjust_layout(layout);
        let size = layout.size();
        let align = layout.align();

        // 1. Free list (under lock + cli — plain linked list, no CAS races)
        let mut prev: *mut FreeBlock = null_mut();
        let mut current = self.free_list.load(Ordering::Relaxed);

        while !current.is_null() {
            let block_size = (*current).size;
            let block_ptr = current as *mut u8;

            if block_size >= size && (block_ptr as usize) % align == 0 {
                let next_block = (*current).next;
                if prev.is_null() {
                    self.free_list.store(next_block, Ordering::Relaxed);
                } else {
                    (*prev).next = next_block;
                }
                let remaining = block_size - size;
                if remaining > Self::HEADER_SIZE * 2 {
                    let remainder_ptr = block_ptr.add(size);
                    self.add_free_block(remainder_ptr, remaining);
                }
                return block_ptr;
            }

            prev = current;
            current = (*current).next;
        }

        // 2. Bump
        let current_bump = self.bump_next.load(Ordering::Relaxed);
        let aligned = Self::align_up(current_bump, align);
        let new_next = aligned + size;
        if new_next > Self::HEAP_END {
            return null_mut();
        }
        self.bump_next.store(new_next, Ordering::Relaxed);
        aligned as *mut u8
    }
}

#[global_allocator]
pub(crate) static ALLOCATOR: Allocator = Allocator::new();
