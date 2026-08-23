ENTRY(_start)

SECTIONS {
    . = 0x7c00 + 512; /* 0x7e00, just after the boot */

    _bootloader_start = .;

    .start : {
        *(.start)
    }

    .text : {
        *(.text .text.*)
    }

    .rodata : {
        *(.rodata .rodata.*)
    }

    .data : {
        *(.data .data.*)
    }

    .bss : {
        *(.bss .bss.*)
    }

    /* 64 sectors = 32 KiB, stays below the 64K DMA boundary (0x10000) */
    . = _bootloader_start + 0x8000 - 2;
    .end_marker : {
        SHORT(0xdead)
    }

    /DISCARD/ : {
        *(.eh_frame)
        *(.eh_frame.*)
        *(.eh_frame_hdr)
        *(.eh_frame_hdr.*)
        *(.gcc_except_table)
        *(.gcc_except_table.*)
        *(.debug*)
        *(.comment)
        *(.note*)
        *(.got)
        *(.got.*)
        *(.note.GNU-stack)
    }
}
