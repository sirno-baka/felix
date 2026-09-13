# popugos

Small `std`-userspace crate for PopugOS-specific APIs which do not belong in
Rust `std`.

Current scope: window manager / display integration.

Do not add wrappers for services already provided by `std` or Tokio such as
filesystem, TCP/UDP, clocks, threads, synchronization, or ordinary I/O.
