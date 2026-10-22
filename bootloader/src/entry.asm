.section .start, "ax"
.global _start
.code16

_start:
    # Stage1 preserves the BIOS boot-drive ABI in DL. Capture it before the
    # Rust entry code or another BIOS call has a chance to clobber it.
    mov byte ptr [STAGE2_BOOT_DRIVE], dl
    call stage2_main

1:
    cli
    hlt
    jmp 1b
