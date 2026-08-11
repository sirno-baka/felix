use elf::ElfBytes;
use elf::endian::AnyEndian;
use elf::abi::{ET_EXEC, EM_386, PT_LOAD};
use crate::memory::paging::{phys_to_virt, PageDirectory, PAGE_SIZE};
use crate::println;

#[derive(Debug)]
pub enum ElfLoadError {
    InvalidElf,
    NotExecutable,
    WrongMachine,
    NoProgramHeaders,
    InvalidAddress,
}


/// Загружает ELF по адресам из самого файла (p_vaddr).
/// Перед вызовом CR3 должен быть PD задачи.
/// `page_dir` — PD этой задачи (чтобы домапить страницы сегментов).
/// Возвращает e_entry как есть.
pub fn load_elf(
    binary: &[u8],
    page_dir: &mut PageDirectory,
) -> Result<u32, ElfLoadError> {
    let file = ElfBytes::<AnyEndian>::minimal_parse(binary)
        .map_err(|_| ElfLoadError::InvalidElf)?;

    let header = &file.ehdr;

    if header.e_type != ET_EXEC {
        return Err(ElfLoadError::NotExecutable);
    }
    if header.e_machine != EM_386 {
        return Err(ElfLoadError::WrongMachine);
    }

    let segments = file.segments().ok_or(ElfLoadError::NoProgramHeaders)?;

    let mut min_addr = u32::MAX;
    let mut max_addr = 0u32;

    for phdr in segments.iter() {
        if phdr.p_type != PT_LOAD { continue; }
        let v = phdr.p_vaddr as u32;
        let e = v + phdr.p_memsz as u32;
        min_addr = min_addr.min(v);
        max_addr = max_addr.max(e);
    }

    // Мапим всё от min до max (закрываем gap’ы)
    let mut page = min_addr & !0xFFF;
    let end = (max_addr + 0xFFF) & !0xFFF;
    while page < end {
        if page < 0xC000_0000 {
            page_dir.alloc_and_map_user_page(page);
        }
        page += 0x1000;
    }

    // Потом обычная загрузка сегментов по p_vaddr
    for phdr in segments.iter() {
        if phdr.p_type == PT_LOAD {
            load_segment(&phdr, binary, page_dir)?;
        }
    }

    let entry = header.e_entry as u32;
    println!("[elf] loaded, entry={:#x} min={:#x} max={:#x}", entry, min_addr, max_addr);
    Ok(entry)
}

fn load_segment(
    ph: &elf::segment::ProgramHeader,
    binary: &[u8],
    page_dir: &mut PageDirectory,
) -> Result<(), ElfLoadError> {
    let vaddr  = ph.p_vaddr as u32;
    let memsz  = ph.p_memsz as u32;
    let filesz = ph.p_filesz as u32;
    let offset = ph.p_offset as usize;

    // Не даём грузить сегменты в kernel half
    const KERNEL_OFFSET: u32 = 0xC000_0000;
    if vaddr >= KERNEL_OFFSET || vaddr.saturating_add(memsz) > KERNEL_OFFSET {
        println!("[elf] reject segment vaddr={:#x} memsz={:#x} (kernel range)", vaddr, memsz);
        return Err(ElfLoadError::InvalidAddress);
    }

    println!("[elf] segment vaddr={:#x} filesz={:#x} memsz={:#x}", vaddr, filesz, memsz);

    // Мапим все страницы, которые занимает сегмент (включая bss)
    let start_page = vaddr & !(PAGE_SIZE as u32 - 1);
    let end_page   = (vaddr + memsz + PAGE_SIZE as u32 - 1) & !(PAGE_SIZE as u32 - 1);
    let mut page = start_page;
    while page < end_page {
        page_dir.alloc_and_map_user_page(page);
        page += PAGE_SIZE as u32;
    }

    // Копируем file data
    if filesz > 0 {
        if offset + filesz as usize > binary.len() {
            return Err(ElfLoadError::InvalidElf);
        }
        let src = &binary[offset..offset + filesz as usize];
        unsafe {
            copy_to_user_virt(page_dir, vaddr, src);
        }
    }

    // Обнуляем bss
    if memsz > filesz {
        zero_user_virt(page_dir, vaddr + filesz, (memsz - filesz) as usize);
    }

    Ok(())
}

fn copy_to_user_virt(page_dir: &PageDirectory, mut vaddr: u32, data: &[u8]) {
    let mut offset = 0usize;
    while offset < data.len() {
        let page = vaddr & !0xFFF;
        let page_off = (vaddr & 0xFFF) as usize;
        let chunk = core::cmp::min(data.len() - offset, 0x1000 - page_off);

        let pd_idx = (page >> 22) as usize;
        let pt_idx = ((page >> 12) & 0x3FF) as usize;
        let pde = page_dir.entries[pd_idx];
        let pt_phys = pde & 0xFFFF_F000;
        let pte = unsafe {
            *((phys_to_virt(pt_phys) as *const u32).add(pt_idx))
        };
        let frame_phys = pte & 0xFFFF_F000;

        unsafe {
            core::ptr::copy_nonoverlapping(
                data.as_ptr().add(offset),
                (phys_to_virt(frame_phys) as *mut u8).add(page_off),
                chunk,
            );
        }
        offset += chunk;
        vaddr += chunk as u32;
    }
}

fn zero_user_virt(page_dir: &PageDirectory, mut vaddr: u32, mut len: usize) {
    while len > 0 {
        let page = vaddr & !0xFFF;
        let page_off = (vaddr & 0xFFF) as usize;
        let chunk = core::cmp::min(len, 0x1000 - page_off);

        let pd_idx = (page >> 22) as usize;
        let pt_idx = ((page >> 12) & 0x3FF) as usize;
        let pde = page_dir.entries[pd_idx];
        let pt_phys = pde & 0xFFFF_F000;
        let pte = unsafe {
            *((phys_to_virt(pt_phys) as *const u32).add(pt_idx))
        };
        let frame_phys = pte & 0xFFFF_F000;

        unsafe {
            core::ptr::write_bytes(
                (phys_to_virt(frame_phys) as *mut u8).add(page_off),
                0,
                chunk,
            );
        }
        vaddr += chunk as u32;
        len -= chunk;
    }
}