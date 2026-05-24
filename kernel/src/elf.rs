use elf::ElfBytes;
use elf::endian::AnyEndian;
use elf::abi::{ET_EXEC, EM_386, PT_LOAD};
use crate::memory::paging::PAGING;
use crate::println;

#[derive(Debug)]
pub enum ElfLoadError {
    InvalidElf,
    NotExecutable,
    WrongMachine,
    NoProgramHeaders,
}

pub fn load_elf(
    binary: &[u8],
    target_base: u32,   // куда мы хотим загрузить (0xa00000 + slot*size)
    _max_size: u32,     // пока не используем
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

    let segments = file.segments()
        .ok_or(ElfLoadError::NoProgramHeaders)?;

    let mut elf_base: Option<u32> = None;
    for phdr in segments.iter() {
        if phdr.p_type == PT_LOAD {
            let vaddr = phdr.p_vaddr as u32;
            elf_base = Some(match elf_base {
                Some(base) => base.min(vaddr),
                None => vaddr,
            });
        }
    }
    let elf_base = elf_base.ok_or(ElfLoadError::NoProgramHeaders)?;

    println!("[elf] ELF base vaddr: {:#x}, target_base: {:#x}", elf_base, target_base);
    

    // Загружаем все сегменты **относительно** target_base
    for phdr in segments.iter() {
        if phdr.p_type == PT_LOAD {
            load_program_header_forced(&phdr, binary, target_base, elf_base)?;
        }
    }

    // Правильная точка входа с учётом сдвига
    let entry_point = target_base + (header.e_entry as u32 - elf_base);
    Ok(entry_point)
}

fn load_program_header_forced(
    ph: &elf::segment::ProgramHeader,
    binary: &[u8],
    target_base: u32,
    elf_base: u32,
) -> Result<(), ElfLoadError> {
    let vaddr = ph.p_vaddr as u32;
    let memsz = ph.p_memsz as u32;
    let filesz = ph.p_filesz as u32;
    let offset = ph.p_offset as usize;

    // Сдвиг относительно начала ELF
    let dst_offset = (vaddr - elf_base) as usize;

    println!("[elf] Loading segment vaddr={:#x} → target={:#x} (offset {:#x})",
             vaddr, target_base + dst_offset as u32, dst_offset);

    // 1. Копируем данные
    if filesz > 0 {
        let src = &binary[offset..offset + filesz as usize];
        unsafe {
            core::ptr::copy_nonoverlapping(
                src.as_ptr(),
                (target_base + dst_offset as u32) as *mut u8,
                filesz as usize,
            );
        }
    }

    // 2. Обнуляем .bss
    if memsz > filesz {
        let bss_start = target_base + dst_offset as u32 + filesz;
        let bss_size = (memsz - filesz) as usize;
        unsafe {
            core::ptr::write_bytes(bss_start as *mut u8, 0, bss_size);
        }
    }

    Ok(())
}