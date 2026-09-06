//! CompactFlash PC Card CIS and configuration handling.

use core::ptr::{read_volatile, write_volatile};

use crate::memory::paging::{PAGING, PTEFlags};
use crate::time::sleep;

use super::pc16::{addrwin, Pc16};
use super::{CardInfo, CardType, CF_MEM_PHYS, CF_MEM_SIZE, CF_MEM_VIRT};

const TUPLE_VERS1: u8 = 0x15;
const TUPLE_CONFIG: u8 = 0x1A;
const TUPLE_CFTABLE_ENTRY: u8 = 0x1B;
const TUPLE_FUNCID: u8 = 0x21;
const TUPLE_FUNCE: u8 = 0x22;
const TUPLE_END: u8 = 0xFF;


pub fn map_attribute_memory() {
    let flags = PTEFlags::new().present().writable();
    let mut paging = unsafe { PAGING.lock() };
    let _ = paging.map_physical_range(CF_MEM_PHYS, CF_MEM_SIZE, CF_MEM_VIRT, flags);
    crate::println!(
        "[PCMCIA] mapping CF attribute memory phys=0x{:08x} size=0x{:x} -> virt=0x{:08x}",
        CF_MEM_PHYS, CF_MEM_SIZE, CF_MEM_VIRT
    );
}

#[inline]
unsafe fn attr_read8(offset: u32) -> u8 {
    read_volatile((CF_MEM_VIRT + offset) as *const u8)
}

#[inline]
unsafe fn attr_write8(offset: u32, value: u8) {
    write_volatile((CF_MEM_VIRT + offset) as *mut u8, value);
}

fn parse_cftable_default(raw: u8) -> Option<u8> {
    if (raw & 0x40) != 0 { Some(raw & 0x3f) } else { None }
}

pub fn read_cis() -> Option<CardInfo> {
    let mut pos = 0u32;
    let mut config_base = None;
    let mut first_default = None;
    let mut have_cftable_1 = false;
    let mut func_id = None;

    // crate::println!("[PCMCIA] reading CF attribute memory / CIS...");

    while pos < 0x400 {
        let code = unsafe { attr_read8(pos) };
        if code == TUPLE_END {
            // crate::println!("[PCMCIA] CIS END at +0x{:03x}", pos);
            break;
        }
        if code == 0x00 {
            pos += 2;
            continue;
        }

        let len = unsafe { attr_read8(pos + 2) };
        // crate::println!(
        //     "[PCMCIA] CIS tuple @0x{:03x}: code={:02x} len={}",
        //     pos, code, len
        // );

        // let dump_len = core::cmp::min(len as u32, 24);
        // if dump_len != 0 {
        //     crate::print!("[PCMCIA]   data:");
        //     for i in 0..dump_len {
        //         crate::print!(" {:02x}", unsafe { attr_read8(pos + 4 + i * 2) });
        //     }
        //     crate::println!("");
        // }

        match code {
            TUPLE_VERS1 => {
                // crate::print!("[PCMCIA]   version/product: ");
                // let mut p = pos + 4;
                // let end = p + len as u32 * 2;
                // while p < end {
                //     let b = unsafe { attr_read8(p) };
                //     if b == 0xff { break; }
                //     if b >= 0x20 && b < 0x7f { crate::print!("{}", b as char); }
                //     else if b == 0 { crate::print!(" "); }
                //     p += 2;
                // }
                // crate::println!("");
            }
            TUPLE_FUNCID if len >= 1 => {
                let id = unsafe { attr_read8(pos + 4) } & 0x7f;
                func_id = Some(id);
                // crate::println!("[PCMCIA]   FUNCID={:02x}", id);
            }
            TUPLE_FUNCE if len >= 2 => {
                // let subtype = unsafe { attr_read8(pos + 4) };
                // let iface = unsafe { attr_read8(pos + 6) };
                // crate::println!(
                //     "[PCMCIA]   FUNCE subtype={:02x} iface={:02x}",
                //     subtype, iface
                // );
            }
            TUPLE_CONFIG if len >= 4 => {
                let size = unsafe { attr_read8(pos + 4) };
                let rasz = (size & 0x03) as u32;
                if rasz <= 3 && (2 + rasz + 1) <= len as u32 {
                    let mut base = 0u32;
                    for i in 0..=rasz {
                        let b = unsafe { attr_read8(pos + 8 + i * 2) } as u32;
                        base |= b << (i * 8);
                    }
                    config_base = Some(base);
                    // crate::println!(
                    //     "[PCMCIA]   CONFIG base=0x{:08x} rasz={}",
                    //     base,
                    //     rasz + 1
                    // );
                }
            }
            TUPLE_CFTABLE_ENTRY if len >= 1 => {
                let raw = unsafe { attr_read8(pos + 4) };
                let idx = raw & 0x3f;
                let is_default = (raw & 0x40) != 0;
                if idx == 1 {
                    have_cftable_1 = true;
                }
                if is_default && first_default.is_none() {
                    first_default = Some(idx);
                }
                // crate::println!(
                //     "[PCMCIA]   CFTABLE index={}{}",
                //     idx,
                //     if (raw & 0x40) != 0 { " default" } else { "" }
                // );
            }
            _ => {}
        }

        pos += 4 + len as u32 * 2;
    }

    let base = config_base;
    let index = if have_cftable_1 { Some(1) } else { first_default };
    if let Some(idx) = index {
        crate::println!(
            "[PCMCIA] selected CFTABLE index={}{}",
            idx,
            if have_cftable_1 { " (compatible ATA profile)" } else { " (first default)" }
        );
    }

    Some(CardInfo {
        card_type: CardType::from_funcid(func_id.unwrap_or(0xff)),
        func_id,
        config_base: base,
        config_index: index,
    })
}

pub fn configure_card(pc16: &Pc16, config_base: u32, config_index: u8) -> bool {
    if config_base >= CF_MEM_SIZE {
        // crate::println!(
        //     "[PCMCIA] card: CONFIG base 0x{:x} outside mapped attribute window",
        //     config_base
        // );
        return false;
    }

    // CF/ATA configuration: select the CIS entry and enable I/O + function.
    // The 0x40 function-enable bit is specific to memory/IO function cards;
    // this path is called only after CardType::FixedDisk selection.
    super::store_card_cor(config_base, config_index);
    let cor_value = 0x40 | (config_index & 0x3f);
    // crate::println!(
    //     "[PCMCIA] card: writing COR @0x{:x} = 0x{:02x} (CFTABLE={})",
    //     config_base, cor_value, config_index
    // );
    unsafe { attr_write8(config_base, cor_value); }
    sleep(20);

    let cor = unsafe { attr_read8(config_base) };
    // crate::println!("[PCMCIA] card: COR readback={:02x}", cor);
    if cor == 0xff { return false; }

    unsafe {
        pc16.configure_cf_io();
        let mut awinen = pc16.awinen();
        awinen |= addrwin::MEM0 | addrwin::IO0;
        pc16.write_reg8(super::pc16::reg::AWINEN, awinen);
    }

    // crate::println!(
    //     "[PCMCIA] fixed-disk configured: IO=0x{:03x}-0x{:03x} AWINEN={:02x} IOCTRL={:02x}",
    //     super::CF_IO_BASE, super::CF_IO_END,
    //     unsafe { pc16.awinen() }, unsafe { pc16.ioctrl() }
    // );
    true
}
