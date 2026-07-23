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
- `interrupt-sync/` - Interrupt synchronization utilities (`SpinMutex`)

## Features

### Bootloader

- BIOS compatible (also works on UEFI with CSM enabled)
- Global Descriptor Table loading
- Unreal Mode switching (to use 32-bit addresses in 16-bit Real Mode)
- A20 line enablement
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
- PCI/IDE disk initialization
- Ext2 filesystem mounting and VFS initialization
- Task manager initialization and first task execution (`/shell`)

#### GDT & TSS (`gdt.rs`, `tss.rs`)

- 7-entry GDT: null, kernel code (0x08), kernel data (0x10), user code (0x18), user data (0x20), TSS (0x28), null
- Task State Segment (TSS) with stack pointer for ring 0 transitions (esp0/ss0)
- Flat 4 GiB memory model for kernel and user segments

#### Paging & Memory Management (`memory/paging.rs`)

- 32-bit x86 paging with 4 KiB pages
- Recursive page directory mapping at index 1023 (0xFFFFF000) for PT/PD access
- Large page (4 MiB) identity mapping for first 32 MB
- Page directory per task for isolated virtual memory
- Frame allocator (`PageManager`) with global `SpinMutex<PageManager>` from `interrupt-sync`
- User heap allocation starting at 0x20000000
- `copy_kernel_mappings()` to copy kernel page tables to new tasks with explicit task PD physical address
- Page reference counting (`PageRefcounts`) for tracking page allocations and unmapping

#### Multitasking & Scheduling (`multitasking/task.rs`)

- Task manager with max 8 tasks (`MAX_TASKS = 8`)
- Task structure: 32 KiB stack, page directory, CPU state, file descriptor table, heap pointer, page reference counts
- `CPUState` struct: eax, ebx, ecx, edx, esi, edi, ebp, eip, cs, eflags, esp, ss
- Round-robin CPU scheduler triggered by timer interrupt
- Idle task with `hlt` instruction
- Task creation via `add_task()`, removal via `remove_task()`

#### Interrupts & Exceptions (`interrupts/`)

- IDT with exception handlers and custom interrupts
- Timer interrupt (IRQ0) for CPU scheduling
- Keyboard interrupt (IRQ1) for input handling
- Exception handlers for CPU faults

#### Drivers (`drivers/`, `pci/ide/`)

- PIC driver (`drivers/pic.rs`) - Programmable Interrupt Controller
- Keyboard driver (`drivers/keyboard.rs`) - PS/2 keyboard input
- Keyboard buffer (`drivers/keyboard_buffer.rs`) - Queue-based keyboard input buffer with blocking read support
- PCI/IDE disk driver (`pci/ide/`) - ATA/ATAPI disk access with channel and device support
- Ext2 filesystem support (`filesystem/ext2.rs`)

#### System Calls (`syscalls/handler.rs`)

Interrupt 0x80 with the following syscalls:

- `SYS_EXIT` - terminate current task
- `SYS_OPEN` - open file by path
- `SYS_READ` - read from file descriptor (fd 0 supports blocking read from keyboard buffer)
- `SYS_WRITE` - write to file descriptor (fd 0/1 go to VGA console)
- `SYS_CLOSE` - close file descriptor
- `SYS_MKDIR` - create directory
- `SYS_RMDIR` - remove directory
- `SYS_UNLINK` - delete file
- `SYS_EXECVE` - load and execute ELF binary
- `SYS_MALLOC` - allocate memory (per-task heap with page reference counting)
- `SYS_REALLOC` - reallocate memory
- `SYS_FREE` - free allocated memory
- `SYS_LS` - list directory entries

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
- `ls [path]` - lists directory entries with file types and sizes
- `cat <filename>` - displays content of a file
- `run <file>` - loads file as task and adds it to the task list
- `ps` - lists running tasks
- `mkdir <name>` - creates a directory
- `rmdir <name>` - removes a directory
- `rm <file>` - deletes a file
- `write <file> <data>` - writes data to a file
- `alloc` - tests memory allocation

### libfelix (Standard Library)

Located in `lib/`, provides:

- `mutex` - `SpinMutex` implementation for interrupt synchronization
- `print!` macro able to print formatted text to screen
- `sys_alloc` - system allocation utilities
- `syscall` - syscall wrappers

## Build System

- Uses `Makefile` for building and creating disk images
- Requires: `rustup`, `mtools`, `dosfstools`, `fdisk`, `e2fsprogs`, `e2tools`, `binutils`
- Builds disk image as `build/disk.img` (64 MiB with ext2 rootfs)
- Can be built using Docker or natively on MacOS/Linux

## Running

- QEMU: `make run` or `qemu-system-i386 -drive file=build/disk.img,index=0,media=disk,format=raw,if=ide -no-reboot -no-shutdown -m 64M -serial stdio`
- Debug mode: `make debug` (starts QEMU with gdb server on port 1234, waits for connection)
- Can be booted on real x86 hardware by copying `build/disk.img` to a USB drive

## Target Specifications

- `x86_16-felix.json` - 16-bit real mode target for bootloader
  - Arch: x86, CPU: i386
  - No dynamic linking, no redzone
  - Panic strategy: abort

- `x86_32-felix.json` - 32-bit protected mode target for kernel and apps
  - Arch: x86, CPU: i386
  - No dynamic linking, no redzone, soft-float, no SSE/MMX
  - Panic strategy: abort
  - Relocation model: static

## Rust Features Used

- `#![no_std]` and `#![no_main]` for kernel and bootloader
- `#![feature(naked_functions)]` - for syscall interrupt handler
- `#![feature(pointer_byte_offsets)]` - for pointer arithmetic
- `#![feature(unsize)]` and `#![feature(coerce_unsized)]` - for trait object coercion
- `#![feature(inline_const)]` - for constant expressions

</content>
<parameter=filePath>
/home/sirno/RustroverProjects/felix/README.md