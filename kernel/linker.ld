ENTRY(_start)

/* Higher-half kernel: virtual base 0xC1000000, physical load 0x01000000 */
KERNEL_OFFSET = 0xC0000000;
KERNEL_PHYS  = 0x01000000;
KERNEL_VIRT  = KERNEL_PHYS + KERNEL_OFFSET;   /* 0xC1000000 */

SECTIONS {
    /* Virtual address where symbols live */
    . = KERNEL_VIRT;

    _kernel_start = .;
    _kernel_phys_start = KERNEL_PHYS;

    .start : AT(ADDR(.start) - KERNEL_OFFSET) {
        *(.start)
    }

    .text : AT(ADDR(.text) - KERNEL_OFFSET) {
        *(.text .text.*)
    }

    .rodata : AT(ADDR(.rodata) - KERNEL_OFFSET) {
        *(.rodata .rodata.*)
    }

    .data : AT(ADDR(.data) - KERNEL_OFFSET) {
        *(.data .data.*)
    }

    .bss : AT(ADDR(.bss) - KERNEL_OFFSET) {
        *(.bss .bss.*)
        *(COMMON)
    }

    .eh_frame : AT(ADDR(.eh_frame) - KERNEL_OFFSET) {
        *(.eh_frame .eh_frame.*)
    }
    .eh_frame_hdr : AT(ADDR(.eh_frame_hdr) - KERNEL_OFFSET) {
        *(.eh_frame_hdr .eh_frame_hdr.*)
    }

    /* Keep image size exactly 1 MiB (2048 sectors) as before */
    . = _kernel_start + 0x00100000 - 2;
    .end_marker : AT(ADDR(.end_marker) - KERNEL_OFFSET) {
        SHORT(0xdead)
    }

    _kernel_end = .;
    _kernel_phys_end = _kernel_end - KERNEL_OFFSET;
}