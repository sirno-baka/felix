//! Physical page cache for file-backed MAP_SHARED mappings.
//!
//! A cache entry owns one physical frame and counts page-table mappings of it.
//! Different processes mapping the same inode/page therefore see exactly the
//! same memory, which also gives process-shared futexes a stable physical key.

use alloc::vec::Vec;
use core::{ptr::write_bytes, slice};

use crate::filesystem::VFS;
use crate::memory::paging::{phys_to_virt, PAGE_SIZE, PAGING};
use crate::spin::KMutex;

#[derive(Clone, Copy)]
struct SharedPage {
    inode: u32,
    offset: u64,
    frame: u32,
    refs: u32,
    write_len: usize,
}

struct SharedPageCache {
    pages: Vec<SharedPage>,
}

impl SharedPageCache {
    const fn new() -> Self {
        Self { pages: Vec::new() }
    }
}

static SHARED_PAGES: KMutex<SharedPageCache> = KMutex::new(SharedPageCache::new());

/// Acquire a physical page containing inode at page-aligned offset.
/// The caller maps the returned frame into its own page table.
pub fn acquire_file_page(inode: u32, offset: u64) -> Result<u32, &'static str> {
    if offset & (PAGE_SIZE as u64 - 1) != 0 {
        return Err("shared mapping offset is not page aligned");
    }

    {
        let mut cache = SHARED_PAGES.lock();
        if let Some(page) = cache
            .pages
            .iter_mut()
            .find(|page| page.inode == inode && page.offset == offset)
        {
            page.refs = page.refs.saturating_add(1);
            return Ok(page.frame);
        }
    }

    let size = VFS
        .get()
        .metadata_inode(inode)
        .map(|meta| meta.size)
        .ok_or("shared mapping inode has no metadata")?;
    if offset >= size {
        return Err("shared mapping starts beyond end of file");
    }
    let write_len = ((size - offset) as usize).min(PAGE_SIZE);

    let frame = unsafe { PAGING.lock().alloc_frame() };
    let phys = frame << 12;
    let virt = phys_to_virt(phys);
    unsafe {
        write_bytes(virt as *mut u8, 0, PAGE_SIZE);
        let page = slice::from_raw_parts_mut(virt as *mut u8, PAGE_SIZE);
        let _ = VFS.get().read_at(inode, offset, page);
    }

    // Do not rely on the giant kernel lock for correctness here. If another
    // caller populated the cache while the I/O above ran, use its frame and
    // return ours to the allocator.
    let mut cache = SHARED_PAGES.lock();
    if let Some(existing) = cache
        .pages
        .iter_mut()
        .find(|page| page.inode == inode && page.offset == offset)
    {
        existing.refs = existing.refs.saturating_add(1);
        let existing_frame = existing.frame;
        drop(cache);
        unsafe { PAGING.lock().free_phys_frame(frame) };
        return Ok(existing_frame);
    }
    cache.pages.push(SharedPage {
        inode,
        offset,
        frame,
        refs: 1,
        write_len,
    });
    Ok(frame)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharedFrameRelease {
    NotShared,
    Retained,
    Last,
}

/// Release one page-table reference. The caller frees the physical frame only
/// for NotShared or Last; Retained means another MAP_SHARED PTE still owns it.
pub fn release_shared_frame(frame: u32) -> SharedFrameRelease {
    let mut cache = SHARED_PAGES.lock();
    let Some(index) = cache.pages.iter().position(|page| page.frame == frame) else {
        return SharedFrameRelease::NotShared;
    };

    let page = cache.pages[index];
    if cache.pages[index].refs > 1 {
        cache.pages[index].refs -= 1;
        return SharedFrameRelease::Retained;
    }
    cache.pages.swap_remove(index);
    drop(cache);

    // Last mapping: persist the valid file portion. Physical-frame lifetime is
    // completed by the caller after this function returns.
    let virt = phys_to_virt(frame << 12);
    unsafe {
        let bytes = slice::from_raw_parts(virt as *const u8, page.write_len);
        let _ = VFS.get().write_at(page.inode, page.offset, bytes);
    }
    SharedFrameRelease::Last
}

/// Release either a private user frame or one reference to a shared frame.
/// Shared frames are physically freed only after their final mapping vanishes.
pub fn release_user_frame(frame: u32) {
    match release_shared_frame(frame) {
        SharedFrameRelease::Retained => {}
        SharedFrameRelease::NotShared | SharedFrameRelease::Last => {
            unsafe { PAGING.lock().free_phys_frame(frame) };
        }
    }
}
