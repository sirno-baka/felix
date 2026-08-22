UNAME := $(shell uname)

ifeq ($(UNAME), Darwin)
	SFDISK := $(shell brew --prefix util-linux)/sbin/sfdisk
	OBJCOPY := $(shell brew --prefix binutils)/bin/objcopy
	E2MKFS := $(shell brew --prefix e2fsprogs)/sbin/mkfs.ext2
	E2CP   := $(shell brew --prefix e2tools)/bin/e2cp
	E2MKDIR := $(shell brew --prefix e2tools)/bin/e2mkdir
endif

ifeq ($(UNAME), Linux)
	SFDISK := /sbin/sfdisk
	OBJCOPY := objcopy
	E2MKFS := mkfs.ext2
	E2CP   := e2cp
	E2MKDIR := e2mkdir
endif

# Partition layout (must match disk.layout and bootloader PART_LBA)
PART_START := 2048
PART_SECTORS := 129024
DISK_MB := 64

.PHONY: all
all: get-deps build objcopy image
	@echo "Felix has been successfully built!"

.PHONY: get-deps
get-deps:
ifeq ($(UNAME), Darwin)
	@echo "Downloading MacOS build tools..."
	@brew list util-linux >/dev/null 2>&1 || brew install util-linux
	@brew list e2fsprogs >/dev/null 2>&1 || brew install e2fsprogs
	@brew list e2tools >/dev/null 2>&1 || brew install e2tools
	@brew list binutils >/dev/null 2>&1 || brew install binutils
endif
ifeq ($(UNAME), Linux)
	@echo "Linux: need sfdisk, mkfs.ext2, e2cp, e2mkdir, objcopy"
endif

.PHONY: build
build:
	@echo "Building Felix..."
	@cargo build --target=x86_16-felix.json --package=felix-boot --release
	@cargo build --target=x86_16-felix.json --package=felix-bootloader --release
	@cargo build --target=x86_32-felix.json --package=felix-kernel --release
	@cargo build --target=x86_32-felix.json --package=hello --release
	@cargo build --target=x86_32-felix.json --package=shell --release

.PHONY: objcopy
objcopy:
	@echo "Objcopy..."
	@mkdir -p build
	@$(OBJCOPY) -I elf32-i386 -O binary -S --strip-all \
		target/x86_16-felix/release/felix-boot build/boot.bin
	@$(OBJCOPY) -I elf32-i386 -O binary -S --strip-all \
		target/x86_16-felix/release/felix-bootloader build/bootloader.bin
	@$(OBJCOPY) -I elf32-i386 -O binary -S --strip-all \
		target/x86_32-felix/release/felix-kernel build/kernel.bin
	@cp -f target/x86_32-felix/release/hello build/hello
	@cp -f target/x86_32-felix/release/shell build/shell

# Disk image:
#   LBA 0        — MBR (boot.bin), partition table from disk.layout
#   LBA 1..64    — stage2 bootloader (MBR gap)
#   LBA 2048+    — one ext2 partition with /boot/kernel.bin + apps
.PHONY: image
image:
	@echo "=== Creating $(DISK_MB) MiB disk image ==="
	@rm -f build/disk.img build/rootfs.img
	@dd if=/dev/zero of=build/disk.img bs=1M count=$(DISK_MB) status=none

	@echo "=== Partition table ==="
	@$(SFDISK) build/disk.img < disk.layout

	@echo "=== MBR + stage2 into gap ==="
	@dd if=build/boot.bin of=build/disk.img bs=512 count=1 conv=notrunc status=none
	@dd if=build/bootloader.bin of=build/disk.img bs=512 seek=1 conv=notrunc status=none

	@echo "=== ext2 rootfs ($(PART_SECTORS) sectors) ==="
	@dd if=/dev/zero of=build/rootfs.img bs=512 count=$(PART_SECTORS) status=none
	@$(E2MKFS) -q -I 128 -b 1024 \
		-O ^64bit,^metadata_csum,^dir_index,^ext_attr,^resize_inode \
		build/rootfs.img

	@echo "=== Populating /boot and apps ==="
	@$(E2MKDIR) build/rootfs.img:/boot 2>/dev/null || true
	@$(E2CP) -p build/kernel.bin build/rootfs.img:/boot/kernel.bin
	@$(E2CP) -p build/shell build/rootfs.img:/shell
	@$(E2CP) -p build/hello build/rootfs.img:/hello
	@echo "  → /boot/kernel.bin, /shell, /hello"

	@echo "=== Embed partition at LBA $(PART_START) ==="
	@dd if=build/rootfs.img of=build/disk.img bs=512 seek=$(PART_START) conv=notrunc status=none

	@echo "=== Restore partition table (dd may have clobbered MBR code only; table is fine) ==="
	@$(SFDISK) build/disk.img < disk.layout
	@dd if=build/boot.bin of=build/disk.img bs=446 count=1 conv=notrunc status=none

	@rm -f build/rootfs.img
	@echo "=== disk.img ready ==="
	@ls -lh build/disk.img

.PHONY: clean
clean:
	@cargo clean
	@rm -rf build

.PHONY: run
run: all
	@echo "Running Felix (IDE disk)..."
	@-killall qemu-system-i386 2>/dev/null || true
	@qemu-system-i386 \
		-drive file=build/disk.img,index=0,media=disk,format=raw,if=ide \
		-no-reboot -no-shutdown -m 64M -serial stdio

# Same kernel.bin — useful later for PXE/TFTP / qemu -kernel
.PHONY: run-kernel
run-kernel: build objcopy
	@echo "Direct -kernel (netboot-style file load, no disk boot chain)..."
	@-killall qemu-system-i386 2>/dev/null || true
	@qemu-system-i386 \
		-kernel build/kernel.bin \
		-drive file=build/disk.img,index=0,media=disk,format=raw,if=ide \
		-no-reboot -no-shutdown -m 64M -serial stdio

.PHONY: debug
debug: all
	@-killall qemu-system-i386 2>/dev/null || true
	@qemu-system-i386 \
		-drive file=build/disk.img,index=0,media=disk,format=raw,if=ide \
		-no-reboot -no-shutdown -m 64M -serial stdio -s -S &
