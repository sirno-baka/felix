# Felix OS

Experimental **32-bit x86 operating system** written from scratch in Rust (`#![no_std]`).

Originally a bachelor thesis project by [Gianmatteo Palmieri](https://gian.im); currently extended with higher-half kernel, per-task paging, Ext2/VFS, and a userspace networking stack (smoltcp + Intel 8255x).

> Target: IA-32 (i386), BIOS boot, QEMU / real hardware.

---

## Status

| Area | State |
|------|--------|
| Boot (BIOS → PM) | ✅ |
| Higher-half kernel + paging | ✅ |
| Multitasking (round-robin) | ✅ |
| Syscalls (`int 0x80`, Linux i386 numbers) | ✅ |
| Ext2 + VFS + per-task FD table | ✅ |
| ELF loader (`execve`) | ✅ |
| Userspace apps (`shell`, `hello`) | ✅ |
| Networking (UDP sockets) | ✅ working echo |
| TCP sockets | 🔷 stubs / partial |
| SMP / 64-bit | ❌ |

---

## Architecture

```
+---------------------------+  ring 3
|  shell / hello / apps     |  libfelix (syscalls, File, print)
+---------------------------+
            | int 0x80
+---------------------------+  ring 0
|  syscalls  |  VFS / Ext2  |
|  scheduler |  net stack   |
|  paging    |  drivers     |
+---------------------------+
|  boot → protected mode    |
+---------------------------+
```

- **Higher-half kernel** at `0xC0000000+`
- **Per-task page directories**: user mappings private, kernel half shared
- **Identity map** 0–32 MiB (DMA, early boot) + MMIO for NIC
- **TSS** for ring3 → ring0 stack switch on interrupts/syscalls

---

## Features

### Boot

- 16-bit BIOS stage (`boot/`) + 32-bit loader (`bootloader/`)
- A20, GDT, unreal mode, protected mode jump into kernel

### Kernel core

- **GDT / TSS** — kernel/user code+data segments, `esp0` for syscalls
- **Paging** — 4 KiB pages, recursive PD at `0xFFC00000`, 4 MiB large pages for low memory
- **Frame allocator** + kernel heap (`0xC1400000`…`0xC2000000`)
- **IDT** — exceptions, IRQ0 (timer/schedule), IRQ1 (keyboard), `int 0x80`
- **Scheduler** — up to 8 tasks, round-robin on timer tick, idle task with `hlt`

### Filesystem

- Ext2 on IDE disk (PCI ATA)
- VFS layer, path resolve, mkdir/rmdir/unlink
- Per-task **file descriptor table** (`File` / `Socket` variants)

### Syscalls (Linux i386 numbers)

| # | Name | Notes |
|---|------|--------|
| 1 | `exit` | |
| 3 / 4 | `read` / `write` | fd 0 = keyboard (blocking), 1 = console |
| 5 / 6 | `open` / `close` | |
| 7 / 8 / 10 | `mkdir` / `rmdir` / `unlink` | |
| 11 | `execve` | load ELF into new task |
| 200–202 | `malloc` / `free` / `realloc` | per-task user heap |
| 302 | `ls` | directory listing into buffer |
| 359 | `socket` | AF_INET, SOCK_DGRAM / STREAM |
| 361 | `bind` | INADDR_ANY → smoltcp `IpListenEndpoint { addr: None }` |
| 369 / 371 | `sendto` / `recvfrom` | UDP echo works |
| 373 | `shutdown` | |

### Networking

- **Driver**: Intel 82557/82559 (`i8255x`) — Rx/Tx rings, DMA, MMIO
- **Stack**: [smoltcp](https://github.com/smoltcp-rs/smoltcp) 0.11 (IPv4, UDP, TCP, ICMP, Ethernet)
- Poll from timer IRQ (non-blocking `try_lock`) and from socket syscalls
- Userspace UDP bind / recvfrom / sendto verified with QEMU user net + host `nc -u`

### Userspace (`libfelix`)

- Syscall wrappers (`int 0x80`, `inlateout("eax")`)
- `print!` / `println!` → `write(1, …)`
- `File` API: `open`, `read`, `read_to_end`, `write_all`, `read_to_string`
- Socket constants and wrappers matching kernel numbers

### Apps

- **`shell`** — `ls`, `cat`, `run`, `ps`, `mkdir`, `rmdir`, `rm`, `write`, …
- **`hello`** — UDP echo server example (`0.0.0.0:1234`)

---

## Project layout

```
felix/
├── boot/              # 16-bit first stage
├── bootloader/        # 32-bit loader
├── kernel/            # higher-half kernel
│   ├── src/
│   │   ├── memory/     # paging, allocator
│   │   ├── multitasking/
│   │   ├── syscalls/
│   │   ├── filesystem/ # VFS + Ext2
│   │   ├── drivers/    # PIC, keyboard, i8255x
│   │   ├── net/        # smoltcp integration
│   │   ├── pci/ ide/
│   │   └── …
│   └── Cargo.toml
├── lib/               # libfelix (userspace)
├── apps/
│   ├── shell/
│   └── hello/         # UDP echo demo
├── interrupt-sync/    # SpinMutex safe under IRQ
├── x86_16-felix.json / x86_32-felix.json
└── Makefile
```

---

## Build

**Dependencies**

- Rust nightly (see `rust-toolchain.toml`)
- `mtools`, `dosfstools`, `fdisk` / `sfdisk`, `e2fsprogs`, `e2tools`, `binutils` (`objcopy`)
- QEMU (`qemu-system-i386`)

```bash
# Linux (Debian/Ubuntu)
sudo apt install build-essential qemu-system-x86 \
  mtools dosfstools fdisk e2fsprogs e2tools binutils

make          # build + disk image → build/disk.img
make run      # QEMU without NIC
make clean
```

**Network-enabled run** (UDP hostfwd + pcap dump):

```bash
make run-floppy   # or equivalent QEMU line with:
#   -netdev user,id=net0,hostfwd=udp::1234-:1234 \
#   -device i82559er,netdev=net0,mac=52:54:00:12:34:56 \
#   -object filter-dump,id=f1,netdev=net0,file=guest.pcap
```

From the host:

```bash
echo "hello" | nc -u -w1 127.0.0.1 1234
# or against guest IP in user-net: 10.0.2.15
```

**Debug**

```bash
make debug          # QEMU -s -S (gdbstub :1234)
gdb -x my_gdb.sh    # or target remote :1234
```

---

## Roadmap

### Near term

1. **Blocking / async sockets** — sleep on empty `recvfrom` instead of busy-poll; wake from NIC IRQ / timer poll  
2. **TCP end-to-end** — finish `connect` / `listen` / `accept`, simple TCP echo app  
3. **Proper errno** — return negative Linux-style errors instead of `0` / `usize::MAX`  
4. **NIC RX ring hardening** — stricter RFD recycle, drop non-OK frames, less promisc if possible  
5. **Userspace polish** — richer `libfelix` (`TcpStream`/`UdpSocket` style API, DNS later)

### Medium term

6. **Pipe / more FDs** — pipes between tasks, dup, better stdin/stdout redirection in shell  
7. **Better memory** — demand paging, growable user stack/heap, free-list frame allocator  
8. **Signals / kill** — minimal signal delivery for `Ctrl+C` and task control  
9. **Disk robustness** — writeback cache, sync, more Ext2 operations (rename, larger files)  
10. **Build ergonomics** — single `make run-net` target, less `cargo clean` in default build

### Longer term

11. **SMP** — APIC, per-CPU runqueues (hard on current design)  
12. **User networking tools** — tiny `ping`, `nc`-like app in-tree  
13. **Security basics** — stricter USER bits, no kernel identity in user PD where avoidable  
14. **Optional**: UEFI boot path, or 64-bit port (large rewrite)

---

## Design notes (networking)

- Kernel address space is **shared** across all task page directories (same PT frames for PDE 768…1022). Deep-copying kernel PTs caused page faults under user CR3 during `poll`/`recv`.  
- UDP `bind(0.0.0.0)` must use smoltcp `IpListenEndpoint { addr: None, port }` — binding to `Some(0.0.0.0)` only matches the zero address and yields ICMP port unreachable.  
- Timer IRQ must not clobber `eax` when reloading `ds`/`es` (use `cx`); syscall path should not `sti` immediately before `iretd` (IF comes back from user eflags).

---

## License

MIT — see [LICENSE](LICENSE).

---

## Credits

- Original author: **Gianmatteo Palmieri**  
- Stack: **smoltcp**  
- Inspired by classic hobby OSdev (Linux i386 ABI, Intel 8255x docs, Ext2)
