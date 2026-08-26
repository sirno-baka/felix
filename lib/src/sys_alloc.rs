//! Userspace heap: free-list + kernel bump for growth.
//!
//! Kernel SYS_MALLOC is a pure bump (pages stay mapped; SYS_FREE is a no-op
//! there on purpose — unmapping shared pages caused user PF at CR2=0x20).
//! Reuse lives here: freed blocks go on a free list and are handed out again
//! without another syscall when possible.

use crate::syscall::{SYS_FREE, SYS_MALLOC};
use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use core::ptr::{self, null_mut};

/// In-block header while the region is on the free list.
#[repr(C)]
struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

const HEADER_SIZE: usize = core::mem::size_of::<FreeBlock>();
const HEADER_ALIGN: usize = core::mem::align_of::<FreeBlock>();

/// Process-local free list head. Single-threaded userspace for now — no lock.
static mut FREE_HEAD: *mut FreeBlock = null_mut();

fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

fn adjust_layout(layout: Layout) -> Layout {
    let align = layout.align().max(HEADER_ALIGN);
    let size = layout.size().max(HEADER_SIZE);
    let size = align_up(size, align);
    unsafe { Layout::from_size_align_unchecked(size, align) }
}

unsafe fn kernel_malloc(size: usize, align: usize) -> *mut u8 {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_MALLOC => ret,
        in("ebx") size as u32,
        in("ecx") align as u32,
        options(nostack, preserves_flags)
    );
    ret as *mut u8
}

unsafe fn kernel_free(ptr: *mut u8, size: usize, align: usize) {
    // Kernel FREE is intentionally a no-op (bump arena). Still invoke it so
    // a future kernel free-list stays compatible.
    asm!(
        "int 0x80",
        in("eax") SYS_FREE,
        in("ebx") ptr as u32,
        in("ecx") size as u32,
        in("edx") align as u32,
        options(nostack, preserves_flags)
    );
}

unsafe fn free_list_add(ptr: *mut u8, size: usize) {
    if ptr.is_null() || size < HEADER_SIZE {
        return;
    }
    let block = ptr as *mut FreeBlock;
    (*block).size = size;
    (*block).next = FREE_HEAD;
    FREE_HEAD = block;
}

unsafe fn free_list_take(size: usize, align: usize) -> *mut u8 {
    let mut prev: *mut FreeBlock = null_mut();
    let mut cur = FREE_HEAD;
    while !cur.is_null() {
        let block_size = (*cur).size;
        let block_ptr = cur as *mut u8;
        if block_size >= size && (block_ptr as usize) % align == 0 {
            let next = (*cur).next;
            if prev.is_null() {
                FREE_HEAD = next;
            } else {
                (*prev).next = next;
            }
            let remaining = block_size - size;
            if remaining > HEADER_SIZE * 2 {
                free_list_add(block_ptr.add(size), remaining);
            }
            // Caller expects zeroed memory like kernel bump path.
            ptr::write_bytes(block_ptr, 0, size);
            return block_ptr;
        }
        prev = cur;
        cur = (*cur).next;
    }
    null_mut()
}

pub struct SyscallAllocator;

unsafe impl GlobalAlloc for SyscallAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let layout = adjust_layout(layout);
        let size = layout.size();
        let align = layout.align();

        let from_list = free_list_take(size, align);
        if !from_list.is_null() {
            return from_list;
        }
        kernel_malloc(size, align)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() {
            return;
        }
        let layout = adjust_layout(layout);
        free_list_add(ptr, layout.size());
        // Optional notify kernel (no-op today).
        kernel_free(ptr, layout.size(), layout.align());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ptr.is_null() {
            return self.alloc(Layout::from_size_align_unchecked(
                new_size.max(1),
                layout.align(),
            ));
        }
        if new_size == 0 {
            self.dealloc(ptr, layout);
            return null_mut();
        }

        let old = adjust_layout(layout);
        // In-place shrink / same size: keep the block (no copy).
        if new_size <= old.size() {
            return ptr;
        }

        let new_layout = Layout::from_size_align_unchecked(new_size, old.align());
        let new_ptr = self.alloc(new_layout);
        if new_ptr.is_null() {
            return null_mut();
        }
        ptr::copy_nonoverlapping(ptr, new_ptr, old.size().min(new_size));
        self.dealloc(ptr, layout);
        new_ptr
    }
}

#[global_allocator]
static ALLOCATOR: SyscallAllocator = SyscallAllocator;
