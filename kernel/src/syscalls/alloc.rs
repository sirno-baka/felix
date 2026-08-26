use crate::memory::allocator::ALLOCATOR;
use core::alloc::{GlobalAlloc, Layout};

pub fn handle_alloc(size: u32, align: u32) -> u32 {
    let layout =
        Layout::from_size_align(size as usize, align as usize).unwrap_or(Layout::new::<u8>());

    let ptr = unsafe { ALLOCATOR.alloc(layout) };
    ptr as u32
}

pub fn handle_dealloc(ptr: u32, size: u32, align: u32) {
    let layout =
        Layout::from_size_align(size as usize, align as usize).unwrap_or(Layout::new::<u8>());

    unsafe {
        ALLOCATOR.dealloc(ptr as *mut u8, layout);
    }
}
