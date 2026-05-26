// exceptions.rs
use core::arch::asm;
use crate::multitasking::task::CPUState;   // ← ОБЯЗАТЕЛЬНО добавить
use crate::println;

#[no_mangle]
pub extern "C" fn exception_handler(int: u32, eip: u32, cs: u32, eflags: u32) {
    println!("\n=== EXCEPTION {} ===", int);
    if int == 0x0E {
        println!("PAGE FAULT!");
    } else {
        println!("Exception type: {:#x}", int);
    }
    println!("EIP: {:#x}, CS: {:#x}, EFLAGS: {:#b}", eip, cs, eflags);
    println!("System halted");
    loop {
        unsafe { asm!("hlt"); }
    }
}

// ==================== ИСПРАВЛЕННЫЙ PAGE FAULT ====================
#[naked]
pub extern "C" fn page_fault() {
    unsafe {
        asm!(
        "cli",
        // ТОЧНО ТАКОЙ ЖЕ порядок, как в timer() и syscall()
        "push ebp",
        "push edi",
        "push esi",
        "push edx",
        "push ecx",
        "push ebx",
        "push eax",
        "push esp",
        "call page_fault_handler",
        "add esp, 4",
        "mov esp, eax",
        "pop eax",
        "pop ebx",
        "pop ecx",
        "pop edx",
        "pop esi",
        "pop edi",
        "pop ebp",
        "add esp, 4",      // ← убираем error_code (CPU пушит его автоматически)
        "sti",
        "iretd",
        options(noreturn)
        );
    }
}

#[no_mangle]
pub extern "C" fn page_fault_handler(esp: u32) -> u32 {
    let state = unsafe { &*(esp as *const CPUState) };
    let eip = state.eip;
    let cs = state.cs;
    let eflags = state.eflags;
    let ss = state.ss;
    println!("PAGE FAULT!");
    println!("EIP: {:#x} | CS: {:#x} | EFLAGS: {:#b}",
             eip, cs, eflags);
    println!("User ESP: {:#x} | SS: {:#x}", esp, ss);

    // Можно позже добавить CR2 (faulting address), но для начала хватит
    loop {
        unsafe { asm!("hlt"); }
    }
}

// Остальные обработчики (оставляем как были)
#[naked]
pub extern "C" fn div_error() {
    unsafe {
        asm!("push 0x00; call exception_handler; add esp, 4; iretd", options(noreturn))
    }
}

#[naked]
pub extern "C" fn invalid_opcode() {
    unsafe {
        asm!("push 0x06; call exception_handler; add esp, 4; iretd", options(noreturn))
    }
}

#[naked]
pub extern "C" fn double_fault() {
    unsafe {
        asm!("push 0x08; call exception_handler; add esp, 4; iretd", options(noreturn))
    }
}

#[naked]
pub extern "C" fn general_protection_fault() {
    unsafe {
        asm!("push 0x0d; call exception_handler; add esp, 4; iretd", options(noreturn))
    }
}

#[naked]
pub extern "C" fn generic_handler() {
    unsafe {
        asm!("push 0xff; call exception_handler; add esp, 4; iretd", options(noreturn))
    }
}