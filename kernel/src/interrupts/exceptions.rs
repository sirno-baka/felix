// exceptions.rs
use core::arch::asm;
use crate::multitasking::task::CPUState;
use crate::println;

#[no_mangle]
pub extern "C" fn exception_handler(int: u32, eip: u32, cs: u32, eflags: u32) {
    let  name = match int {
        0x00 => { "div_error" },
        0x06 => { "invalid_opcode" },
        0x0d => { "general_protection_fault" },
        0xff => { "generic_handler_handler" },
        _ => {""}
    };
    println!("\n=== EXCEPTION {} ===", name);
    println!("EIP: {:#x}, CS: {:#x}, EFLAGS: {:#b}", eip, cs, eflags);
    println!("System halted");
    loop {
        unsafe { asm!("hlt"); }
    }
}

// ==================== НОВЫЕ НАКЕД-ХЕНДЛЕРЫ (одинаковый стиль) ====================

#[naked]
pub extern "C" fn div_error() {
    unsafe {
        asm!(
        "cli",
        "push ebp", "push edi", "push esi", "push edx",
        "push ecx", "push ebx", "push eax",
        "push esp",
        "call div_error_handler",
        "add esp, 4",
        "mov esp, eax",
        "pop eax", "pop ebx", "pop ecx", "pop edx",
        "pop esi", "pop edi", "pop ebp",
        "add esp, 4",      // _dummy_pushed_esp
        "sti",
        "iretd",
        options(noreturn)
        );
    }
}

#[no_mangle]
pub extern "C" fn div_error_handler(esp: u32) -> u32 {
    let state = unsafe { &*((esp as usize + 8) as *const CPUState) };
    exception_handler(0x00, state.eip, state.cs, state.eflags);
    esp
}

// ==============================================

#[naked]
pub extern "C" fn invalid_opcode() {
    unsafe {
        asm!(
        "cli",
        "push ebp", "push edi", "push esi", "push edx",
        "push ecx", "push ebx", "push eax",
        "push esp",
        "call invalid_opcode_handler",
        "add esp, 4",
        "mov esp, eax",
        "pop eax", "pop ebx", "pop ecx", "pop edx",
        "pop esi", "pop edi", "pop ebp",
        "add esp, 4",
        "sti",
        "iretd",
        options(noreturn)
        );
    }
}

#[no_mangle]
pub extern "C" fn invalid_opcode_handler(esp: u32) -> u32 {
    let state = unsafe { &*((esp as usize + 8) as *const CPUState) };
    exception_handler(0x06, state.eip, state.cs, state.eflags);
    esp
}
// ==============================================

#[naked]
pub extern "C" fn general_protection_fault() {
    unsafe {
        asm!(
        "cli",
        "push ebp", "push edi", "push esi", "push edx",
        "push ecx", "push ebx", "push eax",
        "push esp",
        "call general_protection_fault_handler",
        "add esp, 4",
        "mov esp, eax",
        "pop eax", "pop ebx", "pop ecx", "pop edx",
        "pop esi", "pop edi", "pop ebp",
        "add esp, 4",      // _dummy_pushed_esp
        "add esp, 4",      // ← error code (CPU pushes it for #GP)
        "sti",
        "iretd",
        options(noreturn)
        );
    }
}

#[no_mangle]
pub extern "C" fn general_protection_fault_handler(esp: u32) -> u32 {
    let state = unsafe { &*((esp as usize + 4) as *const CPUState) };
    exception_handler(0x0d, state.eip, state.cs, state.eflags);
    esp
}

// ==============================================

#[naked]
pub extern "C" fn double_fault() {
    unsafe {
        asm!(
        "cli",
        "push ebp", "push edi", "push esi", "push edx",
        "push ecx", "push ebx", "push eax",
        "push esp",
        "call double_fault_handler",
        "add esp, 4",
        "mov esp, eax",
        "pop eax", "pop ebx", "pop ecx", "pop edx",
        "pop esi", "pop edi", "pop ebp",
        "add esp, 4",
        "add esp, 4",      // error code
        "sti",
        "iretd",
        options(noreturn)
        );
    }
}

#[no_mangle]
pub extern "C" fn double_fault_handler(esp: u32) -> u32 {
    let state = unsafe { &*((esp as usize + 8) as *const CPUState) };
    exception_handler(0x08, state.eip, state.cs, state.eflags);
    esp
}

// ==============================================

#[naked]
pub extern "C" fn generic_handler() {
    unsafe {
        asm!(
        "cli",
        "push ebp", "push edi", "push esi", "push edx",
        "push ecx", "push ebx", "push eax",
        "push esp",
        "call generic_handler_handler",
        "add esp, 4",
        "mov esp, eax",
        "pop eax", "pop ebx", "pop ecx", "pop edx",
        "pop esi", "pop edi", "pop ebp",
        "add esp, 4",
        "sti",
        "iretd",
        options(noreturn)
        );
    }
}

#[no_mangle]
pub extern "C" fn generic_handler_handler(esp: u32) -> u32 {
    let state = unsafe { &*((esp as usize + 8) as *const CPUState) };
    exception_handler(0xff, state.eip, state.cs, state.eflags);
    esp
}

// page_fault уже был в хорошем виде — оставляем как есть
#[naked]
pub extern "C" fn page_fault() {
    // твой текущий код (или вставь сюда тот, что у тебя сейчас)
    unsafe {
        asm!(
        "cli",
        "push ebp", "push edi", "push esi", "push edx",
        "push ecx", "push ebx", "push eax",
        "push esp",
        "call page_fault_handler",
        "add esp, 4",
        "mov esp, eax",
        "pop eax", "pop ebx", "pop ecx", "pop edx",
        "pop esi", "pop edi", "pop ebp",
        "add esp, 4",
        "add esp, 4",      // error code для page fault
        "sti",
        "iretd",
        options(noreturn)
        );
    }
}

#[no_mangle]
pub extern "C" fn page_fault_handler(esp: u32) -> u32 {
    let mut buffer = [0u8; 64];
    unsafe {
        core::ptr::copy_nonoverlapping(
            esp as *const u8,
            buffer.as_mut_ptr(),
            64
        );
    }
    println!("{:02x?}", buffer);

    let state = unsafe { &*((esp as usize ) as *const CPUState) };
    println!("PAGE FAULT!");
    let eip = state.eip;
    let cs = state.cs;
    let eflags = state.eflags;
    println!("EIP: {:#x} | CS: {:#x} | EFLAGS: {:#b}", eip, cs, eflags);
    esp
}