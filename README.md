# Felix OS - Project Information

## Overview
**Felix OS** is an experimental operating system for the Intel IA-32 architecture (x86), written completely from scratch in Rust without external dependencies. This project is part of a bachelor thesis in computer engineering by Gianmatteo Palmieri.

## Project Structure (Cargo Workspace)
- `boot/` - 16-bit bootloader (target: `x86_16-felix.json`)
- `bootloader/` - 32-bit bootloader (target: `x86_16-felix.json`)
- `kernel/` - 32-bit kernel (target: `x86_32-felix.json`)
- `apps/hello/` - Hello world app
- `apps/shell/` - Shell application
- `lib/` - Standard library (`libfelix`)
- `interrupt-sync/` - Interrupt synchronization utilities

## Features

### Bootloader
- BIOS compatible (also works on UEFI with CSM enabled)
- Global Descriptor Table loading
- Unreal Mode switching (to use 32-bit addresses in 16-bit Real Mode)
- Kernel copying from disk to protected memory
- 32-bit Protected Mode switching
- Kernel jumping

### Kernel Architecture

The kernel is located in `kernel/` and implements a complete 32-bit protected mode OS with the following components:

#### System Initialization (`main.rs`)
- Entry point `_start()` with custom stack setup (stack at 0x0020_0000)
- GDT + TSS initialization
- Paging initialization (recursive page directory mapping at 0xFFC00000)
- IDT loading with exceptions, timer, keyboard, and syscall (0x80) interrupts
- PIC initialization
- Ext2 filesystem mounting and VFS initialization
- Task manager initialization and first task execution (`/hello`)

#### GDT & TSS (`gdt.rs`, `tss.rs`)
- 7-entry GDT: null, kernel code (0x08), kernel data (0x10), user code (0x18), user data (0x20), TSS (0x28), null
- Task State Segment (TSS) with stack pointer for ring 0 transitions (esp0/ss0)
- Flat 4 GiB memory model for kernel and user segments

#### Paging & Memory Management (`memory/paging.rs`)
- 32-bit x86 paging with 4 KiB pages
- Recursive page directory mapping at index 1023 (0xFFFFF000) for PT/PD access
- Large page (4 MiB) identity mapping for first 32 MB
- Page directory per task for isolated virtual memory
- Frame allocator (`PageManager`) with global `PAGING` mutex
- User heap allocation starting at 0x20000000
- `copy_kernel_mappings()` to copy kernel page tables to new tasks

#### Multitasking & Scheduling (`multitasking/task.rs`)
- Task manager with max 8 tasks (`MAX_TASKS = 8`)
- Task structure: 32 KiB stack, page directory, CPU state, file descriptor table, heap pointer
- `CPUState` struct: eax, ebx, ecx, edx, esi, edi, ebp, eip, cs, eflags, esp, ss
- Round-robin CPU scheduler triggered by timer interrupt
- Idle task with `hlt` instruction
- Task creation via `add_task()`, removal via `remove_task()`

#### Interrupts & Exceptions (`interrupts/`)
- IDT with exception handlers and custom interrupts
- Timer interrupt (IRQ0) for CPU scheduling
- Keyboard interrupt (IRQ1) for input handling
- Exception handlers for CPU faults

#### Drivers (`drivers/`)
- PIC driver (`pic.rs`) - Programmable Interrupt Controller
- Keyboard driver (`keyboard.rs`) - PS/2 keyboard input
- Keyboard buffer (`keyboard_buffer.rs`) - Queue-based keyboard input buffer
- ATA disk driver (`disk.rs`) - IDE/ATA disk access
- Ext2 filesystem support (`filesystem/ext2.rs`)

#### System Calls (`syscalls/handler.rs`)
Interrupt 0x80 with the following syscalls:
- `SYS_EXIT` - terminate current task
- `SYS_OPEN` - open file by path
- `SYS_READ` - read from file descriptor
- `SYS_WRITE` - write to file descriptor (fd 0/1 go to VGA console)
- `SYS_CLOSE` - close file descriptor
- `SYS_MKDIR` - create directory
- `SYS_RMDIR` - remove directory
- `SYS_UNLINK` - delete file
- `SYS_EXECVE` - load and execute ELF binary
- `SYS_MALLOC` - allocate memory (per-task heap)
- `SYS_REALLOC` - reallocate memory
- `SYS_FREE` - free allocated memory

#### ELF Loading (`elf.rs`)
- Load `ET_EXEC`, `EM_386` ELF binaries
- Parse program headers (`PT_LOAD`)
- Map segments to target virtual address
- Handle `.bss` zero-initialization
- Calculate correct entry point with ELF base offset

#### Filesystem & VFS (`filesystem/`)
- Virtual filesystem (`Vfs`) with root inode
- Ext2 filesystem driver
- File descriptor table per task
- File modes: ReadOnly, WriteOnly, ReadWrite

### Shell Commands
- `help` - shows available commands
- `ls` - lists root directory entries
- `cat <filename>` - displays content of a file
- `test <a,b,c>` - runs a dummy task
- `run <file>` - loads file as task and adds it to the task list
- `ps` - lists running tasks
- `rt <id>` - removes specified task

### libfelix (Standard Library)
- `print!` macro able to print formatted text to screen

## Build System
- Uses `Makefile` for building and creating disk images
- Requires: `rustup`, `mtools`, `dosfstools`, `fdisk`, `e2fsprogs`, `e2tools`
- Builds disk image as `build/disk.img` (64 MiB with ext2 rootfs)
- Can be built using Docker or natively on MacOS/Linux

## Running
- QEMU: `make run` or `qemu-system-i386 -drive file=build/disk.img,index=0,media=disk,format=raw,if=ide -no-reboot -no-shutdown -m 64M -serial stdio`
- Debug mode: `make debug` (starts QEMU with gdb server on port 1234)
- Can be booted on real x86 hardware by copying `build/disk.img` to a USB drive

## Target Specifications
- `x86_16-felix.json` - 16-bit real mode target for bootloader
- `x86_32-felix.json` - 32-bit protected mode target for kernel and apps
    - Arch: x86, CPU: i386
    - No dynamic linking, no redzone, soft-float, no SSE/MMX
    - Panic strategy: abort

## Roadmap (Planned Features)
- Paging
- Memory allocator
- VESA video driver
- Networking
- SATA AHCI disk driver
- Graphical user interface
