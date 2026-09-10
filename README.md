# Felix OS

Felix is an experimental **32-bit x86 (IA-32) operating system written from scratch in Rust**.

The kernel and most of the system are `#![no_std]`. Felix boots through a custom BIOS boot chain, runs a higher-half kernel, has its own paging, process model, VFS, device drivers, a software window compositor, native ELF userspace, and experimental WASM/WASI execution.

The project is developed against both **QEMU** and real legacy hardware, especially the **Sony VAIO PCG-C1MAH / C1M generation**.

> Current tree status: **September 2026**  
> Workspace version: **0.4.0**  
> Primary target: **i386 / BIOS / uniprocessor x86**

---

## Contents

- [At a glance](#at-a-glance)
- [Current hardware status](#current-hardware-status)
- [Architecture](#architecture)
- [Boot process](#boot-process)
- [Kernel initialization](#kernel-initialization)
- [Memory layout](#memory-layout)
- [Paging and allocators](#paging-and-allocators)
- [Interrupts and timer](#interrupts-and-timer)
- [Processes and multitasking](#processes-and-multitasking)
- [Signals, pipes and file descriptors](#signals-pipes-and-file-descriptors)
- [System calls](#system-calls)
- [Filesystems and storage](#filesystems-and-storage)
- [Driver status](#driver-status)
- [USB / OHCI](#usb--ohci)
- [PCMCIA / CardBus / CompactFlash](#pcmcia--cardbus--compactflash)
- [Graphics](#graphics)
- [Window manager](#window-manager)
- [libfelix and userspace UI](#libfelix-and-userspace-ui)
- [Applications](#applications)
- [Networking](#networking)
- [WASM / WASI](#wasm--wasi)
- [Debugging](#debugging)
- [Project layout](#project-layout)
- [Building and running](#building-and-running)
- [Known limitations](#known-limitations)

---

# At a glance

| Area | Current implementation |
|---|---|
| Architecture | 32-bit x86 / IA-32, little-endian, BIOS boot |
| Kernel | Rust `no_std`, higher-half kernel |
| Kernel physical load | `0x01000000` |
| Kernel virtual base | `0xC1000000` |
| Paging | x86 paging with 4 MiB PSE kernel mappings + 4 KiB user/MMIO pages |
| RAM detection | BootInfo / BIOS E801 / CMOS fallback, clamped to 64 MiB..1 GiB |
| Scheduler | Preemptive round-robin |
| PIT frequency | Currently 200 Hz |
| Task limit | 8 task slots, slot 0 is idle |
| Userspace | Ring 3 native i386 ELF executables |
| Syscalls | `int 0x80` |
| VFS | Mount table with longest-prefix routing |
| Filesystems | ext2, FAT/FAT32, DevFS |
| Storage | ATA PIO, CompactFlash ATA, USB Mass Storage |
| USB | OHCI USB 1.1; control, bulk and interrupt transfers; hotplug polling |
| Input | PS/2 keyboard + PS/2 mouse; experimental USB HID support |
| Graphics | VESA LFB, 16/24/32 bpp; ATI Mobility Radeon M6 experimental native-mode support |
| LFB virtual address | `0xD0000000` |
| Window system | In-kernel software compositor, max 8 windows |
| Userspace UI | `libfelix`, `embedded-graphics`, retained-mode Taffy UI |
| Network stack | smoltcp-based IPv4/TCP/UDP, currently experimental and disabled by default |
| WASM | wasmi + partial WASI, experimental |
| Real-hardware target | Sony VAIO PCG-C1MAH / C1M family |

---

# Current hardware status

## QEMU

The normal `make run` configuration currently uses:

- i386 system emulation
- 128 MiB RAM
- IDE disk image
- VGA/VESA graphics
- RTL8139 NIC
- PCI OHCI controller
- USB Mass Storage backed by a FAT test image
- serial output to stdio
- port `0xE9` debug logging

QEMU remains the fastest environment for general kernel development and GDB debugging.

## Sony VAIO PCG-C1MAH

Felix is also tested on real Sony PictureBook-era hardware.

Working or validated on the current tree:

- higher-half boot and paging
- PIT/PIC interrupts
- PS/2 keyboard and mouse
- VESA framebuffer
- IDE/ATA storage
- FAT32 and ext2 mounting
- PCMCIA/CompactFlash path on supported controller backends
- two ALi/ULi M5237 OHCI controllers
- USB Mass Storage
- FAT32 USB mount at paths such as `/mnt/usb0`
- USB disconnect → VFS unmount
- USB reconnect → re-enumeration and remount
- repeated USB hotplug without the earlier IRQ/MMIO lockups

The Sony platform exposed several hardware-specific bugs that QEMU did not reproduce. The important fixes are documented in [USB / OHCI](#usb--ohci) and [Debugging](#debugging).

---

# Architecture

A simplified view of Felix:

```text
 BIOS
  │
  ▼
 Stage 1 boot sector (16-bit)
  │  loads stage 2 from LBA 1..
  ▼
 Felix bootloader
  │  unreal mode
  │  ext2 loader
  │  RAM detection
  │  VESA setup
  │  optional PXE RAM disk
  ▼
 32-bit protected mode
  │
  ▼
 Kernel @ phys 0x01000000
  │
  ├── higher-half paging @ 0xC0000000+
  ├── GDT / TSS / IDT / PIC / PIT
  ├── VFS + storage drivers
  ├── USB / PCMCIA polling
  ├── smoltcp network stack (experimental)
  ├── in-kernel window compositor
  └── task scheduler
          │
          ├── native ring-3 ELF applications
          │      └── libfelix
          │
          └── experimental wasmi/WASI tasks
```

The native userspace is intentionally small and Unix-like where useful, but Felix is not attempting to be Linux ABI compatible as a whole. Some syscall numbers and data layouts intentionally mirror Linux i386 to simplify porting code.

---

# Boot process

Felix has its own multi-stage BIOS boot chain.

## Stage 1 — `felix-boot`

`boot/` builds a 16-bit boot sector.

The first stage:

1. starts in BIOS real mode;
2. clears the text screen and prints a minimal boot banner;
3. reads **64 sectors starting from LBA 1**;
4. jumps to the loaded second-stage bootloader.

The standard disk image layout reserves this low-LBA area for the bootloader.

## Stage 2 — `felix-bootloader`

The bootloader performs the heavier setup:

1. initializes a GDT;
2. enables A20;
3. enters 16-bit unreal mode;
4. finds the ext2 partition;
5. mounts ext2;
6. loads `/kernel.bin` to physical **`0x01000000`**;
7. detects physical RAM;
8. optionally detects a PXE/iPXE-style network boot;
9. writes a `BootInfo` structure for the kernel;
10. chooses and enables a VESA linear framebuffer mode;
11. switches to 32-bit protected mode;
12. calls the kernel entry at physical `0x01000000`.

## BootInfo

BootInfo lives at physical **`0x00007000`**.

Magic:

```text
0xFE11B007
```

Fields:

```rust
struct BootInfo {
    magic: u32,
    disk_phys: u32,
    disk_sectors: u32,
    flags: u32,
    mem_bytes: u32,
}
```

The kernel prefers `BootInfo.mem_bytes` for RAM size when the magic is valid, otherwise it falls back to CMOS probing.

## PXE / network boot

The bootloader contains support for detecting PXE/iPXE remnants and can copy the whole boot disk into RAM, then pass the RAM disk through BootInfo.

Current default RAM-disk physical base:

```text
0x02000000
```

This path is experimental; see the memory-layout limitation later in this document because this legacy address currently overlaps the fixed kernel heap reservation.

## VESA hand-off

Framebuffer metadata is written at physical:

```text
0x00005000
```

The structure contains:

- physical LFB address
- pitch
- width
- height
- bits per pixel

The VESA boot code accepts 16, 24 and 32 bpp modes, prefers suitable wide modes, then 800×600, then known VBE fallback modes.

---

# Kernel initialization

The kernel begins at physical `0x01000000`, constructs early dual mappings, jumps into the higher half, then builds the final page directory.

The current initialization sequence is broadly:

```text
RAM detection
  ↓
early paging / higher-half jump
  ↓
GDT + TSS
  ↓
final PageManager / CR3
  ↓
IDT + CPU exceptions + syscall gate
  ↓
framebuffer / graphics
  ↓
window manager
  ↓
8259 PIC
  ↓
keyboard / PS/2 mouse
  ↓
PCI / IDE / root filesystem
  ↓
PCMCIA
  ↓
USB OHCI
  ↓
TaskManager + idle task + userspace shell
  ↓
IRQ masks
  ↓
PIT @ 200 Hz
  ↓
STI → scheduler
```

The network stack is present in the source tree but its normal boot initialization is currently disabled while device/resource coexistence is still being cleaned up.

---

# Memory layout

Felix uses a higher-half kernel and deliberately reserves a number of fixed areas for boot structures, heap, framebuffer and MMIO.

These tables describe the **current implementation**, not a permanent ABI. Some entries are target-specific.

## Physical memory map

| Physical range/address | Purpose |
|---|---|
| `0x00000000..0x00FFFFFF` | Low memory, BIOS areas, bootstrap data and early structures |
| `0x00005000` | `FramebufferInfo` from the bootloader |
| `0x00006000` | VBE controller-info scratch buffer |
| `0x00006200` | VBE mode-info scratch buffer |
| `0x00007000` | `BootInfo` |
| `0x00090000` | Approximate bootloader protected-mode stack top |
| `0x00200000` | Early temporary page directory used during the higher-half transition |
| `0x01000000..0x017FFFFF` | Reserved physical kernel image window |
| `0x01800000..0x027FFFFF` | Fixed 16 MiB kernel heap backing |
| `0x02000000..` | Legacy PXE RAM-disk base — **currently overlaps part of the fixed heap reservation; experimental** |
| `0x02800000..RAM_END` | Frame allocator: page tables, task stacks, user pages, surfaces, etc. |

Physical RAM used by the paging subsystem is detected at runtime, aligned to 4 MiB and clamped to a current practical range of **64 MiB to 1 GiB**.

## Virtual address map

| Virtual range/address | Purpose |
|---|---|
| `0x00000000..0xBFFFFFFF` | User address space / low identity mappings as applicable |
| ELF `p_vaddr` | Native executable mappings; typical applications are linked in low user space |
| `0x40000000 + slot×0x10000000` | Per-task malloc/brk region base |
| `0x60000000..` | Automatic `mmap` region |
| `< 0xB0000000` | Current upper bound for automatic mmap growth |
| `0xBFFDF000..0xBFFFF000` | Native user stack reservation, 32 pages / 128 KiB |
| `0xBFFFF000` | Native user stack top |
| `0xC0000000` | Higher-half physical-memory offset |
| `0xC1000000` | Kernel image virtual start (`0x01000000 + 0xC0000000`) |
| roughly `0xC1200000..0xC1600000` | Historical/boot kernel-stack area; stack grows downward |
| `0xC1800000..0xC27FFFFF` | 16 MiB kernel heap |
| `0xC2800000` | Kernel heap end |
| `0xD0000000` | Permanent VESA LFB mapping |
| `0xD1000000` | ATI Mobility Radeon M6 MMIO mapping |
| `0xE0000000` | PCMCIA/CardBus controller MMIO window in current layout |
| `0xE0001000` | CF attribute-memory virtual window |
| `0xE0100000` | OHCI virtual MMIO slot 0 |
| `0xE0102000` | OHCI virtual MMIO slot 1 |
| `0xFFC00000..0xFFFFEFFF` | Recursive page-table window |
| `0xFFFFF000` | Current page directory through recursive mapping |

### Device-specific physical resources currently used

On the Sony/PCMCIA path, CompactFlash attribute memory is currently mapped from a physical window around:

```text
phys 0xF0001000 → virt 0xE0001000
```

The CF ATA I/O window uses legacy I/O ports around:

```text
0xC000..0xC00F
```

OHCI physical BAR addresses are PCI-discovered at runtime; only their **virtual slots** are fixed to `0xE0100000 + N×0x2000`.

---

# Paging and allocators

## Kernel page tables

Felix enables x86 PSE and maps detected RAM using 4 MiB pages for the large identity/higher-half windows.

For each physical 4 MiB chunk:

```text
phys X → virt X
phys X → virt 0xC0000000 + X
```

The final kernel directory also installs a recursive mapping at PDE 1023.

## Per-task page directories

Every native task gets its own page directory.

`copy_kernel_mappings()` shares:

1. the initial identity-mapped physical-memory large pages;
2. the entire higher-half kernel region, PDE 768 through 1022, including kernel MMIO mappings;
3. a task-specific recursive PDE pointing at the task's own directory.

User ELF mappings can replace low identity large-page PDEs with normal 4 KiB user page tables when required.

This shared higher-half design is important: kernel heap, framebuffer and device MMIO remain accessible when a syscall or IRQ happens while a user task CR3 is active.

## Kernel heap

The kernel global allocator owns:

```text
virt 0xC1800000..0xC2800000
phys 0x01800000..0x02800000
size 16 MiB
```

It combines:

- a bump pointer for new storage;
- a free-list for returned blocks;
- interrupt exclusion around allocator critical sections.

The allocator is intentionally kept away from the boot/kernel stack area.

## Physical frame allocator

The frame allocator begins at:

```text
phys 0x02800000
```

It is currently a monotonic/bump allocator.

`free_phys_frame()` does not yet recycle frames, so page-table, task and mapping churn consumes physical memory until reboot.

## User heap

Native userspace uses a simple per-task layout:

```text
heap_base(slot) = 0x40000000 + slot * 0x10000000
```

Current characteristics:

- first 2 MiB are pre-mapped at exec time;
- custom `SYS_MALLOC` uses bump allocation;
- `SYS_MALLOC` has a current soft ceiling of roughly 128 MiB per slot;
- `brk` growth is currently capped at 32 MiB;
- `SYS_REALLOC` allocates a new range and copies data;
- `SYS_FREE` is intentionally a no-op at present;
- pages are not generally returned to the frame allocator.

This is sufficient for current applications but not a general-purpose production allocator.

## mmap

Automatic `mmap` allocation begins around:

```text
0x60000000
```

It stays below roughly `0xB0000000` to leave room for the user stack.

Supported forms include anonymous and simple file-backed mappings. A single mapping is currently limited to 64 MiB.

---

# Interrupts and timer

Felix uses the legacy dual **8259A PIC**.

| Source | IRQ / vector | Current role |
|---|---:|---|
| CPU exceptions | vectors `0..31` | exception handlers |
| PIT | IRQ0 / vector 32 | scheduler tick |
| PS/2 keyboard | IRQ1 / vector 33 | keyboard input |
| PIC cascade | IRQ2 | slave PIC cascade |
| Shared legacy PCI INTx | IRQ9 | currently kept masked for USB/PCMCIA polling |
| PS/2 mouse | IRQ12 / vector 44 | mouse input |
| Syscalls | vector `0x80` | userspace → kernel |

The current PIT rate is:

```text
200 Hz
```

The normal PIC configuration enables PIT, keyboard, cascade and PS/2 mouse. IRQ9 remains masked because the Sony C1M generation shares it between several PCI/CardBus devices while Felix does not yet have a generic shared-INTx dispatcher.

USB and PCMCIA hotplug therefore use polling rather than their shared hardware interrupt.

---

# Processes and multitasking

Felix has a small preemptive round-robin scheduler.

## Task model

- maximum task slots: **8**
- slot 0: kernel idle task
- native applications: ring 3
- each task has its own page directory
- each task has a 64 KiB kernel stack
- TSS `esp0` is updated when switching into a user task
- tasks track parent, zombie state and exit code
- `wait()` / `WNOHANG` are supported

Native user context uses standard Felix GDT selectors:

```text
CS = 0x1B
SS = 0x23
EFLAGS includes IF (0x202)
```

The idle task runs in ring 0 and uses `hlt`; it also services polling work such as USB and PCMCIA hotplug.

## ELF loader

Native executables must be:

- ELF
- `ET_EXEC`
- machine `EM_386`
- below `0xC0000000`

`PT_LOAD` segments are mapped at the virtual addresses encoded in the ELF file. BSS is zero-filled.

The initial user stack contains a Linux-style `argc` / `argv` layout with an empty environment list.

---

# Signals, pipes and file descriptors

## Signals

Felix currently implements a small Unix-like signal layer.

Recognized signal numbers include:

| Signal | Number | Default action |
|---|---:|---|
| `SIGHUP` | 1 | terminate |
| `SIGINT` | 2 | terminate |
| `SIGQUIT` | 3 | terminate |
| `SIGKILL` | 9 | terminate, cannot be overridden |
| `SIGTERM` | 15 | terminate |

Native tasks can install simple signal handlers through `sigaction` or use `SIG_IGN` where allowed.

A pending user handler is delivered by editing the task's saved user CPU state so execution resumes at the handler with the signal number on the user stack.

Killing/exiting a task also closes its descriptors and destroys its owned windows.

## Pipes

Anonymous pipes are implemented in kernel memory.

Current limits:

- maximum 16 pipes
- 4096-byte ring buffer per pipe
- reader/writer reference counts
- blocking read/write using `sti` + `hlt`
- non-blocking mode through `fcntl(O_NONBLOCK)`
- EOF when the final writer disappears

The shell uses these pipes for pipelines and for child stdin/stdout capture.

## File descriptors

Per-task descriptor tables can contain:

- console input/output
- regular files
- directories
- DevFS devices
- pipes
- sockets

Supported operations include open/read/write/close, lseek, dup2, fcntl and poll.

---

# System calls

System calls use:

```text
int 0x80
```

The syscall entry disables interrupts, saves general-purpose registers, calls the Rust dispatcher, accepts a possibly changed saved-stack pointer after a task switch, then returns through `iretd`.

## Core / Unix-like syscalls

| Number | Name | Notes |
|---:|---|---|
| 1 | `exit` | task exit |
| 3 | `read` | files/devices/pipes/stdin |
| 4 | `write` | files/devices/pipes/stdout |
| 5 | `open` | subset of Linux flags |
| 6 | `close` | close FD |
| 7 | `mkdir` | VFS mkdir |
| 8 | `rmdir` | VFS rmdir |
| 10 | `unlink` | remove file |
| 11 | `execve` | spawn native i386 ELF from memory |
| 19 | `lseek` | seek files/devices |
| 37 | `kill` | queue signal |
| 42 | `pipe` | anonymous pipe |
| 45 | `brk` | user heap growth |
| 54 | `ioctl` | currently mostly `ENOTTY` stub |
| 55 | `fcntl` | `F_GETFL`, `F_SETFL`, nonblocking |
| 63 | `dup2` | descriptor duplication |
| 67 | `sigaction` | simple signal handler API |
| 90 | `mmap` | old i386 arg structure |
| 91 | `munmap` | unmap user pages |
| 114 | `wait` | parent/child wait; `WNOHANG` |
| 168 | `poll` | pipes/files/sockets + timeout |
| 192 | `mmap2` | simple mapping implementation |
| 195 | `stat64` | Linux-like stat layout |
| 197 | `fstat64` | Linux-like stat layout |
| 220 | `getdents64` | directory iteration |
| 252 | `exit_group` | currently equivalent to exit |

## Felix custom syscalls

| Number | Name |
|---:|---|
| 200 | `malloc` |
| 201 | `free` |
| 202 | `realloc` |
| 302 | `ls` convenience syscall |
| 1000 | `execve_wasm` |

## Socket syscalls

Implemented paths currently include:

| Number | Name | Status |
|---:|---|---|
| 359 | `socket` | implemented |
| 361 | `bind` | implemented |
| 362 | `connect` | implemented |
| 363 | `listen` | implemented |
| 364 | `accept4` | stub / not implemented |
| 369 | `sendto` | implemented for TCP/UDP abstraction |
| 371 | `recvfrom` | implemented for TCP/UDP abstraction |
| 373 | `shutdown` | implemented |

Several other Linux i386 socket syscall numbers are defined for future compatibility but are not yet dispatched.

## Window / service syscalls

| Number | Name | Purpose |
|---:|---|---|
| 400 | `wm_create` | create a window |
| 401 | `wm_destroy` | destroy a window |
| 402 | `wm_move` | move window |
| 403 | `wm_info` | query live geometry/state |
| 404 | `wm_flip` | full or partial client-surface present |
| 405 | `wm_focus` | focus/raise window |
| 406 | `wm_screen` | screen dimensions |
| 407 | `mouse_state` | mouse snapshot |
| 408 | `wm_poll` | window event queue |
| 409 | `wm_windows` | enumerate windows |
| 410 | `pci_list` | enumerate PCI functions |
| 411 | `ifconfig` | network configuration API |
| 412 | `fb_info` | framebuffer information |
| 413 | `fb_blit` | user-buffer rectangle → framebuffer |

---

# Filesystems and storage

## VFS

Felix has a mountable VFS with longest-prefix path routing.

It supports:

- a root filesystem
- additional mounted filesystems under `/mnt/...`
- runtime unmount, used by removable devices
- DevFS under `/dev`

## Root filesystem selection

At boot, the filesystem layer can discover several block devices/filesystems.

The current preference is approximately:

1. BootInfo/PXE RAM disk when present;
2. ATA disks;
3. scan available partitions/filesystems;
4. prefer a filesystem containing `/shell`;
5. otherwise prefer ext2;
6. otherwise use the first mountable filesystem.

## ext2

ext2 is the normal root filesystem in the standard disk image.

The Makefile creates a conservative ext2 format suitable for the small custom implementation, disabling modern features such as 64-bit metadata checksums.

## FAT / FAT32

FAT support is used heavily for removable media.

Tested current path:

```text
Kingston DataTraveler → USB MSC → FAT32 → /mnt/usb0
```

Mounting, file reads, disconnect/unmount and reconnect/remount work on the current Sony test machine.

## DevFS

DevFS is mounted at:

```text
/dev
```

It exposes character/block-style endpoints such as:

- `null`
- `zero`
- ATA disks (`sda`, etc.)
- PCMCIA/CompactFlash block devices when present

USB Mass Storage currently mounts through the removable-filesystem path rather than necessarily being represented as a traditional `/dev/sdX` node.

## IDE / ATA

The IDE path uses legacy PIO I/O ports:

```text
primary:   0x1F0 / control 0x3F4
secondary: 0x170 / control 0x374
```

Implemented functionality includes:

- ATA/ATAPI identification
- PIO reads/writes
- LBA28
- LBA48
- CHS fallback paths

IDE DMA is not currently the normal path.

---

# Driver status

The following table distinguishes code that exists from code that is part of the current stable boot path.

| Driver/subsystem | Status | Notes |
|---|---|---|
| 8259 PIC | **working** | legacy interrupt controller |
| PIT | **working** | current scheduler rate 200 Hz |
| PS/2 keyboard | **working** | IRQ1 |
| PS/2 mouse | **working** | IRQ12 |
| VESA LFB | **working** | 16/24/32 bpp |
| ATI Radeon Mobility M6 | **experimental / hardware-specific** | Sony native-panel work |
| PCI enumeration | **working** | userspace `lspci` support |
| IDE ATA PIO | **working** | root storage path |
| ext2 | **working** | standard root FS |
| FAT/FAT32 | **working** | removable media |
| DevFS | **working** | block/char endpoints |
| OHCI USB 1.1 | **working on QEMU and Sony C1M** | hotplug polling |
| USB Mass Storage | **working** | BOT plus CBI/CB support code |
| USB HID | **experimental** | basic boot keyboard/mouse code; PS/2 remains primary input |
| USB hubs | **basic / experimental** | downstream enumeration support |
| Ricoh R5C475 PCMCIA | **active backend** | ExCA/CardBus/CF path |
| CompactFlash ATA | **working on supported controller path** | BlockDevice + removable mount |
| Toshiba ToPIC100 | **source backend exists, not wired into active registry** | `1179:0617` implementation present |
| RTL8139 | **experimental / disabled by default** | networking integration in progress |
| Intel 8255x | **experimental** | older QEMU/development path |
| smoltcp IPv4/TCP/UDP | **implemented, disabled by default** | user socket API exists |

---

# USB / OHCI

Felix currently implements **OHCI USB 1.1**.

Discovery is PCI-class based:

```text
class 0x0C / subclass 0x03 / prog-if 0x10
```

The HCD is therefore not restricted to a single vendor/device ID.

Current OHCI functionality:

- up to two statically allocated controller DMA areas
- HCCA
- control endpoint transfers
- bulk transfers
- interrupt transfers
- root-hub port enumeration
- root-hub hotplug polling
- address assignment
- class-driver matching
- disconnect cleanup
- reconnect/re-enumeration

Felix currently does **not** implement UHCI, EHCI or xHCI.

## USB class drivers

### Mass Storage

`usb-storage` supports the following protocol paths in the current source:

- Bulk-Only Transport / BBB (`bInterfaceProtocol = 0x50`)
- CBI (`0x00`)
- CB (`0x01`)

The mass-storage driver implements the SCSI operations required for normal block access, including inquiry, capacity probing and sector I/O.

Working tested device:

```text
Kingston DataTraveler 3.0
```

Although the stick is USB 3.x capable, the old laptop talks to it through the USB 1.1 OHCI controller at full speed.

### HID

Basic USB HID boot-protocol keyboard/mouse support exists, including protocol setup and report parsing paths. It is still experimental; the current normal desktop input stack uses PS/2 devices.

### External hubs

A basic external-hub driver exists and can read the hub descriptor, power/reset downstream ports and enumerate children. It should still be considered experimental compared with the root-hub path.

## ALi / ULi M5237 quirks

The Sony PCG-C1MAH contains ALi/ULi M5237 OHCI controllers (`10b9:5237`). Real hardware required several important quirks that QEMU did not expose.

### 1. Never touch `HcFmInterval` on M5237

The M5237 can hard-lock on access to the OHCI `HcFmInterval` register.

Felix therefore marks this controller with `skip_fminterval` and does not read or write that register.

### 2. Do not issue software HCR on M5237

The normal OHCI HCR reset path destroyed the firmware-programmed frame-scheduler state on the Sony hardware.

Symptoms were especially distinctive:

- frame counter advanced;
- HCCA DMA worked;
- root-hub port reset worked;
- `ControlHeadED` was programmed;
- `CLF` remained set;
- `ControlCurrentED` stayed zero;
- every control TD remained `CC=0xF` / Not Accessed.

The working M5237 sequence is therefore:

```text
HCFS → USBRESET
preserve firmware frame scheduler
clear Felix control/bulk lists
install Felix HCCA
HCFS → OPERATIONAL
```

No HCR is performed on this controller.

### 3. Correct OHCI Not-Accessed condition code

New TDs must start with:

```text
CC = 0xF
```

Using `0xE` happened to survive under emulation but was not valid for the real controller's hardware-owned initial state.

### 4. Stable per-controller EP0 DMA memory

EP0 control descriptors and buffers use a permanently allocated, aligned controller DMA page rather than transient mixed heap/stack objects.

### 5. Protocol timing uses the OHCI frame clock

The old `spin_ms()` helper was CPU-speed dependent and was not a real millisecond delay on the Crusoe CPU.

For an operational OHCI controller, protocol timing now uses `HcFmNumber`, whose frame cadence is approximately 1 ms:

- connect debounce: about 100 frames
- port-reset timeout: about 100 frames
- post-reset recovery: about 10 frames
- EP0 timeout: about 500 frames
- post-`SET_ADDRESS`: about 2 frames
- bulk timeout: about 2000 frames
- interrupt timeout: about 200 frames

This makes USB timing independent of both CPU speed and the PIT's 200/1000 Hz scheduler frequency.

### 6. Hotplug is polling-based

On the Sony C1M generation, legacy IRQ9 is shared by CardBus/USB and other devices.

Felix currently has one handler per legacy PIC vector rather than a generic shared-INTx dispatcher, so OHCI hardware interrupts remain disabled and IRQ9 stays masked.

Root-hub status is polled from idle instead.

### 7. Disconnect state is fully cleaned

On disconnect Felix:

- unbinds the USB class driver;
- unmounts removable media;
- removes stale persistent bulk EDs;
- rebuilds `HcBulkHeadED`;
- clears ControlHead/Current state;
- resets endpoint toggles;
- resets EP0 metadata;
- rearms a failed-enumeration latch for the next physical connection.

---

# PCMCIA / CardBus / CompactFlash

The PCMCIA subsystem contains:

- PCI controller discovery
- ExCA / i82365-style register access
- socket status handling
- CIS tuple parsing
- memory-window setup
- I/O-window setup
- polling hotplug
- driver matching
- CompactFlash ATA binding

## CompactFlash ATA

The CF driver presents a card as a normal Felix `BlockDevice`, allowing the normal filesystem probing and removable-mount path to be reused.

The current implementation uses an ATA PIO-style CF I/O window and can mount FAT/ext2-compatible media through the same VFS helpers used by other disks.

## Controller backends

### Ricoh R5C475 / R5C475II

The active controller registry contains the Ricoh backend (`1180:0475` family path).

### Toshiba ToPIC100

A Toshiba ToPIC100 backend for `1179:0617` exists in the source tree, but it is currently **not connected to the active controller registry** in `pcmcia/mod.rs`.

It should therefore be treated as an implementation in progress rather than advertised as an active driver.

## Hotplug IRQ policy

PCMCIA hotplug is currently polling-based for the same reason as OHCI: legacy IRQ9 is shared on the Sony target and Felix does not yet have a shared legacy PCI interrupt dispatcher.

---

# Graphics

## VESA framebuffer

The bootloader chooses a VBE linear framebuffer and writes its information to physical `0x5000`.

The kernel permanently maps the LFB at:

```text
0xD0000000
```

Supported pixel depths in the framebuffer driver include:

- 16 bpp
- 24 bpp
- 32 bpp

The higher-half LFB mapping is shared into task page directories as a kernel mapping, so interrupts and syscalls can safely access it even when a user task CR3 is active.

## ATI Mobility Radeon M6

The tree contains a hardware-specific ATI Radeon Mobility M6 / RV100 path, targeting hardware such as the Sony PictureBook.

It maps MMIO around:

```text
0xD1000000
```

The driver experiments with native panel programming, including CRTC, PLL and LVDS state, while trying to preserve useful BIOS setup.

This path remains experimental; VESA is the normal generic graphics interface.

---

# Window manager

Felix contains an **in-kernel software window compositor**.

Current limit:

```text
MAX_WINDOWS = 8
```

## Window model

Each window has:

- an owner task slot;
- geometry;
- title;
- kernel-side client surface;
- visibility/focus state;
- a small event ring;
- decoration flags.

Userspace maintains its own local BGRX client buffer. `wm_flip` copies either the entire client area or a dirty rectangle into the kernel-side surface, after which the compositor updates the screen.

## Decorations and interaction

The kernel WM implements:

- title bars
- close buttons
- optional frameless windows
- focus
- z-order / raise-to-front
- title-bar dragging
- bottom-right resizing
- software mouse cursor
- dirty-region recomposition
- clipped redraw

Current UI geometry includes an 18-pixel title bar and a small bottom-right resize grip.

Closing a window through the kernel close-button path also terminates its owning user task.

## Events

Each window has a fixed ring of approximately 32 events.

Event types include:

```text
MouseMove
MouseDown
MouseUp
KeyDown
KeyUp
Close
FocusIn
FocusOut
Resize
```

Input is routed according to focus and hit testing.

## Window flags

Current flags include:

- title-bar hint
- close-button hint
- fullscreen-button hint
- frameless hint

The default is a titled window with a close button.

## Rendering path

```text
Userspace application
    │
    │ draws into local BGRX buffer
    │
    ├── wm_flip(full buffer)
    │          or
    └── wm_flip_rect(dirty rectangle)
               │
               ▼
        kernel window surface
               │
               ▼
        software compositor
               │
               ▼
        LFB @ 0xD0000000
```

There is currently no GPU-accelerated compositing.

---

# libfelix and userspace UI

`lib/` provides the userspace runtime and higher-level Felix APIs.

It is itself `#![no_std]` and contains modules for:

- syscall wrappers
- process/runtime startup
- argument parsing
- files
- printing
- signals
- async helpers
- networking
- window management
- retained-mode UI
- allocation support

## Window API

`libfelix::wm::Window`:

- creates/destroys kernel windows;
- owns a local 32-bit BGRX client buffer;
- can move/focus/query a window;
- polls WM events;
- automatically rebuilds its local surface on resize;
- supports full `flip()` and partial `flip_rect()`;
- implements `embedded_graphics::DrawTarget<Rgb888>`.

Dropping a `Window` destroys the corresponding kernel window.

## Retained-mode UI

`libfelix::ui` is a retained-mode UI layer built around **Taffy** for layout and `embedded-graphics` for drawing.

### Containers

- row
- column
- generic flex
- panel
- spacer
- scroll view + scroll content

### Widgets

- `Label`
- `Button`
- `TextInput`

### UI features

- retained widget tree
- intrinsic widget measurement
- Taffy flexbox styles
- focus management
- hover/pressed state
- click callbacks
- keyboard dispatch
- scrolling
- draggable scrollbar
- programmatic scroll-to-top/bottom
- dirty rectangles
- clipped drawing
- partial window flips

Typical application loop:

```rust
loop {
    ui.process(&mut window);
}
```

`Ui::process()` performs event polling, event dispatch, callbacks, layout when needed, dirty rendering and presentation.

---

# Applications

The Makefile automatically builds every `apps/<name>` package. Packages named `wasm-*` are built for `wasm32-wasip1`; other applications are native i386 userspace programs.

## `shell`

The main Felix shell is a **userspace GUI terminal**, not a kernel console.

Current features:

- windowed terminal
- simple VT/escape-sequence handling
- persistent command history in `/shell_hist`
- Up/Down history navigation
- tab completion
- external native ELF execution
- automatic WASM execution based on file magic
- pipes: `|`
- input redirection: `<`
- output redirection: `>`
- append redirection: `>>`
- child stdin/stdout/stderr plumbing through kernel pipes
- Ctrl+C → `SIGINT`
- live child output in the terminal window

Built-ins currently include:

```text
help
exit / quit
pwd
cd
ls
cat
mkdir
rmdir
rm
path
ps        (currently a stub)
clear
echo
head
lspci
ifconfig
```

`lspci` uses PCI IDs in userspace to display readable vendor/device/class names.

## `show`

`show` is the Felix UI 2.0 showcase.

It demonstrates:

- Taffy row/column layouts
- panels
- scroll views
- labels
- buttons
- text input
- focus
- callbacks
- dynamic labels
- dynamic layout
- scrolling and scrollbars

## `dd`

A small byte/block copier:

```text
dd if=<src> of=<dst> [bs=N] [count=N] [skip=N]
```

It can copy between regular files and block devices exposed through DevFS.

## `http-client`

Experimental HTTP client using the Felix networking library.

Supports:

- GET
- POST
- inline body data
- file upload
- selectable content type
- saving response body to a file

## `ssh`

Experimental asynchronous SSH client using Sunset and the Felix `edge-nal` adapter.

The current program is a development/test client with a hard-coded QEMU-style endpoint and credentials, not a general interactive SSH utility yet.

## `wasm-hello`

Minimal WASI test application used to validate the WASM execution path.

---

# Networking

Networking code exists but is currently **experimental and disabled in the normal boot path**.

Implemented pieces include:

- RTL8139 driver
- Intel 8255x development driver
- smoltcp IPv4 stack
- ARP
- IPv4
- TCP
- UDP
- DHCP configuration path
- static IPv4 configuration
- socket descriptor integration
- polling
- userspace socket syscalls
- `ifconfig`
- HTTP client library/app
- SSH experiment

The normal `main.rs` currently does not enable the network driver initialization by default.

Reasons include unfinished resource/IRQ coexistence on real legacy hardware. Some older experimental NIC code also uses fixed MMIO areas that overlap other current device windows, so it should not be treated as production-ready hardware management.

## ifconfig

The shell can expose network configuration when a supported NIC/stack is initialized:

```text
ifconfig
ifconfig dhcp
ifconfig IP/PREFIX [GATEWAY]
```

## Socket limitations

Client TCP/UDP paths are much more complete than server-side semantics. In particular `accept4` is still a stub, and several BSD/Linux socket operations are not implemented.

---

# WASM / WASI

Felix contains an experimental WebAssembly execution path using **wasmi**.

Special syscall:

```text
SYS_EXECVE_WASM = 1000
```

The shell detects the WebAssembly magic and selects this path automatically.

## Runtime

A WASM task gets:

- a wasmi `Engine`
- module instance
- linear memory
- a partial WASI linker
- Felix file/socket host calls

The runtime looks for `_start`, then `main`.

## Important privilege note

WASM tasks currently execute their interpreter entry in **ring 0**:

```text
CS = 0x08
SS = 0x10
```

So this is not currently a CPU-privilege sandbox equivalent to native ring-3 ELF userspace. WASM isolation relies primarily on the interpreter's linear-memory model.

## Partial WASI support

Implemented or partially implemented host calls include:

- `proc_exit`
- `fd_read`
- `fd_write`
- `fd_close`
- `fd_fdstat_get`
- argument/environment helpers
- socket open/connect/send/recv/shutdown paths
- basic option/flag stubs
- `random_get`
- `clock_time_get`

Some of these are placeholders:

- `random_get` is not cryptographically secure;
- `clock_time_get` currently returns a placeholder time in the WASI path;
- several socket/WASI functions are no-op compatibility stubs.

The WASI layer should therefore be considered developmental rather than standards-complete.

---

# Debugging

Felix has several debugging outputs:

- VGA/text output during early boot
- QEMU debug port `0xE9`
- serial output in QEMU configurations
- framebuffer panic/exception display
- recent kernel log ring
- F12 debug dump on the real machine

## F12 / framebuffer debug safety

A significant real-hardware bug was found here.

An older `fb_panic` implementation temporarily mapped a framebuffer at virtual `0xE0000000` by directly replacing a PDE in the **currently running task's** page directory.

Unfortunately that same 4 MiB PDE contains live device MMIO such as PCMCIA and both OHCI virtual windows.

The result was spectacularly confusing: after an F12 dump the PCI BARs were still correct, but OHCI register reads became zero/garbage because the virtual MMIO range had been replaced by a framebuffer large page.

The current implementation fixes this permanently:

- `fb_panic` **does not create or modify page-table mappings**;
- it only verifies the already-installed framebuffer mapping at `0xD0000000`;
- if the mapping is not available, it falls back rather than corrupting another PDE.

This fix is important to preserve when changing panic/debug code.

---

# Project layout

```text
felix/
├── boot/                 # 16-bit first-stage boot sector
├── bootloader/           # unreal-mode/ext2/VESA/protected-mode loader
├── kernel/               # 32-bit Rust kernel
│   └── src/
│       ├── drivers/      # input, WM, framebuffer, USB, PCMCIA, NICs, etc.
│       ├── filesystem/   # VFS, ext2, FAT, DevFS
│       ├── interrupts/   # IDT, exceptions, timer, IRQ handlers
│       ├── memory/       # paging and kernel allocator
│       ├── multitasking/ # Task + scheduler
│       ├── net/          # smoltcp integration/socket state
│       ├── pci/          # PCI and IDE
│       ├── syscalls/     # int 0x80 dispatcher + WASM host path
│       ├── elf.rs        # i386 ELF loader
│       ├── signal.rs
│       ├── pipe.rs
│       └── fb_panic.rs
├── lib/                  # libfelix userspace runtime/API/UI/networking
├── apps/
│   ├── shell/
│   ├── show/
│   ├── dd/
│   ├── http-client/
│   ├── ssh/
│   └── wasm-hello/
├── rootfs/               # extra files copied to the root ext2 image
├── pxe/                  # network-boot assets/scripts
├── build/                # generated artifacts
├── x86_16-felix.json     # custom 16-bit Rust target
├── x86_32-felix.json     # custom 32-bit Rust target
├── disk.layout
├── Cargo.toml
└── Makefile
```

---

# Building and running

Felix uses custom Rust target JSON files and unstable Cargo target-spec support, so a suitable **nightly Rust toolchain** is required.

The workspace builds `core`, `compiler_builtins` and `alloc` for the custom target.

## Linux tools

The Makefile expects tools equivalent to:

- `cargo` / nightly Rust
- `objcopy`
- `sfdisk`
- `mkfs.ext2`
- `e2cp`
- `qemu-system-i386`
- `mkfs.vfat` / `dosfstools` for the generated USB test image

macOS paths for several Homebrew tools are also handled by the Makefile.

## Build the standard disk image

```bash
make
```

This performs roughly:

```text
build boot stage
build bootloader
build kernel
build native apps
build wasm-* apps for wasm32-wasip1
objcopy boot/kernel images
create 32 MiB disk image
create ext2 root filesystem
copy kernel/apps/rootfs files
install boot sectors/partition table
```

Generated main image:

```text
build/disk.img
```

## Standard disk-image layout

The current Makefile creates a 32 MiB disk:

```text
LBA 0          MBR / stage 1
LBA 1..        stage 2 bootloader area
LBA 2048..     ext2 filesystem
```

The ext2 filesystem receives:

- `/kernel.bin`
- native applications
- WASM applications
- files from `rootfs/`

## Run in QEMU

```bash
make run
```

The run target creates a test USB image if necessary and starts QEMU with an IDE boot disk, RTL8139 and PCI OHCI + USB storage.

## Debug in QEMU

```bash
make debug
```

The debug target starts QEMU paused with GDB support and extra interrupt/error logging.

> Note: the current development `make debug` recipe contains a machine-specific USB image path and may need local adjustment.

## Legacy floppy image

```bash
make floppy-image
make run-floppy
```

## Generate USB test image

```bash
make usb-image
```

This creates a 64 MiB MBR/FAT16 test image used by the QEMU OHCI setup.

## Clean

```bash
make clean
```

---

# Known limitations

Felix is an actively developed hobby/research OS. The following are deliberate current constraints rather than hidden production guarantees.

## Core / memory

- uniprocessor only; no SMP scheduler
- maximum 8 task slots
- frame allocator does not recycle physical frames
- userspace malloc is bump-style and `free()` is currently a no-op
- kernel heap is a fixed 16 MiB window
- RAM is currently clamped to 64 MiB..1 GiB for the paging model
- process heap/mmap management is intentionally simple
- no copy-on-write or demand paging
- PXE RAM disk at `0x02000000` currently overlaps the fixed `0x01800000..0x02800000` kernel heap reservation and must be relocated before that path can be considered safe

## Interrupts / devices

- no generic shared PCI INTx dispatcher
- IRQ9 is kept masked on the Sony target
- USB and PCMCIA hotplug are polling-based
- only OHCI USB 1.1 is implemented; no UHCI/EHCI/xHCI
- no general driver unload/hot-unload framework
- some MMIO regions are still fixed by individual drivers rather than assigned by a central resource manager
- ToPIC100 code exists but is not currently registered by the active PCMCIA core

## Graphics / desktop

- maximum 8 windows
- software compositor
- software cursor
- no GPU acceleration
- ATI M6 native-mode path is hardware-specific/experimental
- event queues are fixed-size rings

## Filesystem / POSIX compatibility

- ext2/FAT implementations cover Felix's current needs, not every filesystem feature
- `ioctl` is largely a stub
- syscall compatibility is Linux-inspired but incomplete
- shell `ps` is currently a placeholder

## Networking

- network initialization is disabled in the default boot path
- NIC integration is experimental
- `accept4` is not implemented
- several socket syscalls are defined but not implemented
- no claim of complete BSD/POSIX sockets compatibility

## WASM

- partial WASI only
- WASM interpreter task currently runs ring 0
- WASI random/time functions include development placeholders
- not a security sandbox suitable for untrusted code

---

# Development notes

Felix deliberately targets hardware old enough to expose assumptions that modern emulators often hide. A recurring rule in this tree is therefore:

> If QEMU works but the PCG-C1MAH does not, first verify real hardware timing, PCI/MMIO ownership, DMA visibility and shared legacy IRQ behavior before adding emulator-specific workarounds.

The current USB implementation is a good example: QEMU tolerated invalid or overly optimistic behavior that the real ALi M5237 did not.

When changing memory mappings, also remember that device MMIO and the framebuffer are shared into every task's higher-half mappings. Debug/panic code must never silently replace live PDEs.

---

Felix is experimental by design, but the current tree already boots into a graphical userspace, runs native ELF applications, mounts ext2/FAT media, performs real USB Mass Storage hotplug on legacy hardware, and provides enough kernel/userspace infrastructure to continue growing into a much more complete small operating system.
