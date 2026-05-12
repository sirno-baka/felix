use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};
use core::ptr::null_mut;

pub struct Allocator {
    next: AtomicUsize,
}

impl Allocator {
    // 8 МБ heap начиная с 4 МБ (после kernel + stack)
    // Это безопасное место при identity paging
    const HEAP_START: usize = 0x0040_0000;   // 4 MiB
    const HEAP_SIZE:  usize = 8 * 1024 * 1024; // 8 MiB (можно увеличить до 16-32 если нужно)

    pub const fn new() -> Self {
        Allocator {
            next: AtomicUsize::new(Self::HEAP_START),
        }
    }
}

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align();

        // Выравниваем по align
        let current = self.next.load(Ordering::Relaxed);
        let aligned = (current + align - 1) & !(align - 1);
        let new_next = aligned + size;

        // Проверяем, не вышли ли за пределы heap
        if new_next > Self::HEAP_START + Self::HEAP_SIZE {
            return null_mut(); // Out of memory
        }

        self.next.store(new_next, Ordering::Relaxed);

        aligned as *mut u8
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Пока настоящий free не нужен — bump-аллокатор просто растёт
        // (можно потом заменить на freelist)
    }
}
#[global_allocator]
pub(crate) static ALLOCATOR: Allocator = Allocator::new();
