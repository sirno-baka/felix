# Rust `std` applications for Felix

This is a separate Cargo workspace for ordinary Rust applications using the
local `i686-unknown-popugos` standard library. Existing `apps/*` remain on the
Felix `no_std + libfelix` target.

## Build the PopugOS sysroot

From the sibling Rust checkout:

```sh
cd ../../rust
BOOTSTRAP_SKIP_TARGET_SANITY=1 ./x build \
  --stage 1 library/std \
  --target i686-unknown-popugos \
  --set llvm.download-ci-llvm=true \
  --set build.optimized-compiler-builtins=false
```

## Build and install applications

From this directory:

```sh
make build
make install
```

Use these `make` targets rather than a bare `cargo build`: the Makefile
isolates this workspace from Felix's parent `.cargo/config.toml`, which is
configured for the two existing `no_std` targets.

`make install` copies every name in `APPS` from the target directory to the
Felix `rootfs`. To rebuild the disk image immediately, run:

```sh
make image
```

After booting Felix, run the example from its shell:

```text
/hello first second
```

## Add another application

1. Add its directory to `members` in `Cargo.toml`.
2. Add the produced binary name to `APPS` in `Makefile`.
3. Run `make install`.

The applications are statically linked and do not depend on `libfelix`.
