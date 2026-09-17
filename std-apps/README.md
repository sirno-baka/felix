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
/bin/hello first second
```

## Twitch radio

`twitch-radio` plays the audio-only HLS rendition of a public live Twitch
channel directly in Felix:

```text
/bin/twitch-radio monstercat
```

Enter `+` or `-` followed by Return to change the volume, and `q` to stop.
The first version supports the usual Twitch MPEG-TS/ADTS AAC-LC stream at
48 kHz and writes PCM16 stereo to `/dev/audio`. It reconnects automatically
after network and offline errors.

## Rhai system scripts

The `rhai` application is the system script runtime. It supports script files,
one-line evaluation, and an interactive REPL:

```text
/bin/rhai /etc/init.rhai
/bin/rhai -e "print(40 + 2)"
/bin/rhai
```

The shell automatically sends files ending in `.rhai` through `/bin/rhai`, so a
script can also be launched directly:

```text
/home/user/window-demo.rhai
```

Scripts receive `ARGS`, `PROGRAM`, and (for files) `SCRIPT`. Felix-specific
objects include `fs`, `http`, `json`, and `ui`, plus environment access,
`sleep`, and synchronous process execution through `run`. The default
`/etc/init.conf` launches `/etc/init.rhai` once during startup. Rhai imports
resolve from `/lib/rhai` without a module cache or manifest.
The current 32-bit runtime uses Rhai's `only_i32` and `no_float` modes because
the PopugOS sysroot does not yet provide the C/libm floating-point symbols.

`rhai-ide` is the graphical system editor for `.rhai` programs. It provides
token highlighting, line numbers, live parser diagnostics, identifier
completion, file open/save, and a Run command with captured output.

Scripts can create retained native interfaces through `ui.window`. Containers
provide rows, columns, panels, spacing and basic widgets, with size, padding,
gap and grow layout controls. Events returned by `app.poll()` are maps with a
`kind` and, for widget signals, a `target`. A complete example is installed as
`/home/user/window-demo.rhai`.

## Add another application

1. Add its directory to `members` in `Cargo.toml`.
2. Add the produced binary name to `APPS` in `Makefile`.
3. Run `make install`.

The applications are statically linked and do not depend on `libfelix`.
