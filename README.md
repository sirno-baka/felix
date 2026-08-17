# Felix OS

Experimental 32-bit x86 operating system written from scratch in Rust (`#![no_std]`).

Target: IA-32 (i386), BIOS boot, QEMU and real hardware.

---

## Current Status (August 2026)

### Kernel

- Higher-half kernel (linked at `0xC1000000`)
- Drivers: PIC, PIT, keyboard (IRQ1), PS/2 mouse (IRQ12)
- VESA Linear Framebuffer (LFB) mapped at virtual address **`0xD0000000`**
    - LFB can only be accessed while kernel CR3 is active
- Round-robin multitasking
- System calls via `int 0x80`

### Window Manager (in-kernel)

Simple compositing window manager living in the kernel.

- Maximum of **8 windows**
- Each window has:
    - Its own client surface (application draws into it)
    - Title bar + close button (drawn by the WM)
- WM is responsible only for compositing and window decorations
- Dirty-region compositing (only changed areas are redrawn)

#### Window Syscall API (numbers 400–408)

| Syscall          | Purpose                              |
|------------------|--------------------------------------|
| `create`         | Create a new window                  |
| `destroy`        | Destroy a window                     |
| `move`           | Move a window                        |
| `info`           | Get up-to-date window information    |
| `flip`           | Present the client surface           |
| `focus`          | Set keyboard/mouse focus             |
| `mouse`          | Get current mouse state              |
| `poll_events`    | Retrieve events from the window queue|

#### Events

- Per-window event queue (ring buffer)
- Event types: `MouseMove`, `MouseDown`, `MouseUp`, `KeyDown`
- Focus is set by hit-testing on `MouseDown`
- Events are delivered only to the focused window

### Userspace (`libfelix`)

#### `Window`

- Thin wrapper around the syscall API
- Implements `embedded-graphics::DrawTarget<Rgb888>` (direct drawing is supported)
- `info()` always performs a live syscall

#### Retained UI (`libfelix::ui`)

Simple retained-mode UI framework:

- Widgets: `Button`, `Label`, `TextInput`
- Focus system with event bubbling
- `on_click` callbacks have the signature `FnMut(&mut Ui)`
- `TextInput` handles its own key events (including Backspace and Escape)
- Main loop:

```rust
ui.process();   // poll_events → dispatch → callbacks → draw → flip
```

### Applications

- **shell** — ordinary userspace window (the old kernel `fb_console` has been removed)
- Can create additional windows, buttons and text fields

---

## Rendering Architecture

```
Application                     Kernel (WM)
─────────────                   ────────────────────────────
draws into                      reads client surface
its client surface      →       composites onto LFB (0xD0000000)
calls flip()                    draws title bar + close button
```

Important notes:
- LFB is only accessible from kernel address space (via `with_kernel_cr3`)
- Client surfaces belong to the process

---

## Building & Running

```bash
cargo build
./run.sh          # or the project's usual run script
```

---

## Known Limitations

- Maximum of 8 windows
- No hardware acceleration or GPU double-buffering
- Mouse cursor is software-rendered
- Very fast typing can fill the kernel event queue (mitigated by using a large buffer, e.g. 256 events, on the userspace side)
- All LFB accesses must run under kernel CR3

---

## Future Work (not implemented)

- Smarter EventQueue (MouseMove coalescing, priorities)
- Richer retained UI toolkit
- Inter-window IPC
- More advanced window manager features (resize, minimize, etc.)
```

The file has been written to:

**`/home/workdir/artifacts/README.md`**

You can copy it into your real project root. It is written entirely in English and is self-contained.