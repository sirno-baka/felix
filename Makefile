UNAME := $(shell uname)

#MacOS tools
ifeq ($(UNAME), Darwin)
	SFDISK := $(shell brew --prefix util-linux)/sbin/sfdisk
	MKFS := $(shell brew --prefix dosfstools)/sbin/mkfs.fat
	MCOPY := $(shell brew --prefix mtools)/bin/mcopy
	OBJCOPY := $(shell brew --prefix binutils)/bin/objcopy
endif

ifeq ($(UNAME), Linux)
	SFDISK := /sbin/sfdisk
	MKFS := mkfs.fat
	MCOPY := mcopy
	OBJCOPY := objcopy
endif

# ext2 tools
ifeq ($(UNAME), Darwin)
	E2MKFS := $(shell brew --prefix e2fsprogs)/sbin/mkfs.ext2
	E2CP   := $(shell brew --prefix e2tools)/bin/e2cp
endif

ifeq ($(UNAME), Linux)
	E2MKFS := mkfs.ext2
	E2CP   := e2cp
endif

.PHONY: all
all: get-deps build objcopy image
	@echo "Felix has been successfully built!"

.PHONY: get-deps
get-deps:
ifeq ($(UNAME), Darwin)
	@echo "Downloading MacOS build tools..."
	@brew list util-linux > /dev/null || brew install util-linux
	@brew list e2fsprogs > /dev/null || brew install e2fsprogs
	@brew list mtools > /dev/null || brew install mtools
	@brew list binutils > /dev/null || brew install binutils
	@brew list dosfstools > /dev/null || brew install dosfstools
	@brew list e2tools > /dev/null || brew install e2tools
endif

ifeq ($(UNAME), Linux)
	@echo "Downloading Linux build tools..."
	# TODO: Download linux build tools
	# (на большинстве дистрибутивов достаточно: sudo apt install e2fsprogs e2tools dosfstools mtools)
endif



.PHONY: build
build:
	@cargo clean -p felix-kernel -p http-client -p shell  -p libfelix -p felix-boot  -Z json-target-spec
	@echo "Building Felix..."
	@cargo build --target=x86_16-felix.json --package=felix-boot --release -Z json-target-spec
	@cargo build --target=x86_16-felix.json --package=felix-bootloader -Z json-target-spec
	@cargo build --target=x86_16-felix.json --package=felix-bootloader --release -Z json-target-spec
	@cargo build --target=x86_32-felix.json --package=felix-kernel -Z json-target-spec
	@cargo build --target=x86_32-felix.json --package=felix-kernel --release -Z json-target-spec
	@cargo build --target=x86_32-felix.json --package=http-client --release -Z json-target-spec
	@cargo build --target=x86_32-felix.json --package=shell --release -Z json-target-spec
	@cargo build --target=wasm32-wasip2 --package=wasm-hello --release


.PHONY: objcopy
objcopy:
	@echo "Copying Felix..."
	@mkdir -p build
	@$(OBJCOPY) -I elf32-i386 -O binary -S --strip-all \
        target/x86_16-felix/release/felix-boot build/boot.bin
	@$(OBJCOPY) -I elf32-i386 -O binary target/x86_16-felix/debug/felix-bootloader build/bootloader.bin
	@$(OBJCOPY) -I elf32-i386 -O binary target/x86_32-felix/debug/felix-kernel build/kernel.bin
	@cp target/x86_32-felix/release/http-client build/http-client
	@cp target/x86_32-felix/release/shell build/shell
	@cp target/wasm32-wasip2/release/wasm-hello.wasm build/wasm

.PHONY: floppy-image
floppy-image:
	@echo "=== Creating 1.44 MB floppy image ==="
	@rm -f build/floppy.img

	@echo "=== Creating empty 1.44 MB image (2880 sectors) ==="
	@dd if=/dev/zero of=build/floppy.img bs=1K count=1440 status=none

	@echo "=== Writing boot sector (sector 0) ==="
	@dd if=build/boot.bin of=build/floppy.img bs=512 conv=notrunc status=none

	@echo "=== Writing bootloader (starting from sector 1) ==="
	@dd if=build/bootloader.bin of=build/floppy.img bs=512 seek=1 conv=notrunc status=none

	@echo "=== Writing kernel ==="
	@dd if=build/kernel.bin of=build/floppy.img bs=512 seek=65 conv=notrunc status=none

	@echo "=== Creating ext2 partition in remaining space ==="
	@rm -f build/ext2.img || true
	@KERNEL_BYTES=$$(wc -c < build/kernel.bin); \
	KERNEL_SECTORS=$$(( (KERNEL_BYTES + 555) / 512 )); \
	EXT2_START_SECTOR=$$((65 + KERNEL_SECTORS)); \
	EXT2_SIZE_SECTORS=$$((2880 - EXT2_START_SECTOR)); \
	EXT2_SIZE_BYTES=$$((EXT2_SIZE_SECTORS * 512)); \
	echo "Kernel size: $$KERNEL_BYTES bytes ($$KERNEL_SECTORS sectors)"; \
	echo "EXT2 starts at sector: $$EXT2_START_SECTOR, size: $$EXT2_SIZE_BYTES bytes"; \
	dd if=/dev/zero of=build/ext2.img bs=1 count=$$EXT2_SIZE_BYTES status=none; \
	mkfs.ext2 -I 128 -O ^64bit,^metadata_csum,^dir_index,^ext_attr,^resize_inode build/ext2.img; \
	$(E2CP) -p build/shell build/ext2.img:/shell; \
	$(E2CP) -p build/http-client build/ext2.img:/http-client; \
	echo "  → shell copied to ext2"; \
	dd if=build/ext2.img of=build/floppy.img bs=512 seek=$$EXT2_START_SECTOR conv=notrunc status=none; \

	@echo "=== Проверка суперблока в образе (должно показать '53ef' на смещении 56) ==="
	@KERNEL_BYTES=$$(wc -c < build/kernel.bin); \
	KERNEL_SECTORS=$$(( (KERNEL_BYTES + 555) / 512 )); \
	EXT2_START_SECTOR=$$((65 + KERNEL_SECTORS)); \
	OFFSET_BYTES=$$(( (EXT2_START_SECTOR + 2) * 512 + 56 )); \
	xxd -s $$OFFSET_BYTES -l 2 build/floppy.img || od -A x -t x1 -j $$OFFSET_BYTES -N 2 build/floppy.img

	@echo "=== Floppy image ready ==="
	@ls -lh build/floppy.img

.PHONY: image
image:
	@echo "=== Creating 32 MiB bootable disk (MBR | bootloader | ext2) ==="
	@rm -f build/disk.img build/rootfs.img
	@dd if=/dev/zero of=build/disk.img bs=1M count=32 status=none

	@echo "=== Partition table (ext2 at LBA 2048) ==="
	@$(SFDISK) build/disk.img < disk.layout
	@$(SFDISK) --list build/disk.img

	@echo "=== Writing MBR (LBA 0) and bootloader (LBA 1) ==="
	@dd if=build/boot.bin of=build/disk.img bs=512 conv=notrunc status=none
	@dd if=build/bootloader.bin of=build/disk.img bs=512 seek=1 conv=notrunc status=none

	@echo "=== Creating ext2 rootfs (~31 MiB, 4K blocks, 128-byte inodes) ==="
	@dd if=/dev/zero of=build/rootfs.img bs=512 count=63488 status=none
	@$(E2MKFS) -I 128 -O ^64bit,^metadata_csum,^dir_index,^ext_attr,^resize_inode build/rootfs.img

	@echo "=== Copying /kernel.bin and userspace to ext2 ==="
	@$(E2CP) -p build/kernel.bin build/rootfs.img:/kernel.bin && echo "  → /kernel.bin"
	@$(E2CP) -p build/shell build/rootfs.img:/shell && echo "  → /shell"
	@$(E2CP) -p build/http-client build/rootfs.img:/http-client && echo "  → /hello"
	@$(E2CP) -p build/wasm build/rootfs.img:/wasm && echo "  → /wasm"
	@$(E2CP) -p build/busybox.wasm build/rootfs.img:/busybox && echo "  → /wasm"

	@echo "=== Embedding ext2 at LBA 2048 ==="
	@dd if=build/rootfs.img of=build/disk.img bs=512 seek=2048 conv=notrunc status=none

	@echo "=== Restore MBR partition table (keep boot code) ==="
	@$(SFDISK) build/disk.img < disk.layout
	@$(SFDISK) --list build/disk.img

	@echo "=== Superblock magic (expect ef53 at partition+1024+56) ==="
	#@xxd -s $$((2048 * 512 + 1024 + 56)) -l 2 build/disk.img || \
		od -A x -t x1 -j $$((2048 * 512 + 1024 + 56)) -N 2 build/disk.img

	@mkdir -p pxe/assets/felix
	@cp -f build/disk.img pxe/assets/felix/disk.img 2>/dev/null || true
	@cp -f build/disk.img pxe/assets/disk.img 2>/dev/null || true

	@rm -f build/rootfs.img
	@ls -lh build/disk.img
	@cp build/disk.img pxe/assets/disk.img
	@echo "=== Disk image ready (dd if=build/disk.img of=/dev/sdX) ==="

# Separate FAT32 data disk (whole-disk FAT volume, no partition table).
# Attached in run/debug as the first IDE data drive after the boot disk:
#   index=0 → build/disk.img  (boot, BIOS 0x80 — bootloader hardcodes this)
#   index=1 → build/fat32.img (FAT32 + /shell, BIOS 0x81)
.PHONY: fat32-image
fat32-image:
	@echo "=== Creating 32 MiB FAT32 data disk ==="
	@mkdir -p build
	@rm -f build/fat32.img
	@dd if=/dev/zero of=build/fat32.img bs=1M count=32 status=none
	@$(MKFS) -F 32 -n FELIXFAT  -S 512 build/fat32.img
	@echo "=== Copying shell to FAT32 root ==="
	@# whole-disk FAT has odd geometry for mtools — skip the check
	@$(MCOPY) -o -v -i build/fat32.img build/shell ::/shell \
		&& echo "  → /shell"
	@ls -lh build/fat32.img
	@echo "=== FAT32 disk ready (build/fat32.img) ==="

.PHONY: clean
clean:
	@echo "Cleaning Felix..."
	@cargo clean
	@rm -rf build

.PHONY: run-floppy
run-floppy: all floppy-image
	@echo "Running Felix..."
	@killall qemu-system-i386 || true
	@qemu-system-i386 \
       -drive file=build/floppy.img,index=0,format=raw,if=floppy \
       -drive file=disk.img,index=0,media=disk,format=raw,if=ide \
       -netdev user,id=net0,hostfwd=udp::1234-:1234 \
         -device i82559er,netdev=net0,mac=52:54:00:12:34:56 \
         -object filter-dump,id=f1,netdev=net0,file=guest.pcap \
       -no-reboot -vga std -no-shutdown -m 128M \
       -debugcon file:debug.log -s -S &
	@#qemu-system-i386 -drive file=build/floppy.img,index=0,format=raw,if=floppy -drive file=disk.img,index=0,media=disk,format=raw,if=ide -device i82551,mac=52:54:00:12:34:56 -no-reboot -vga std  -no-shutdown -m 64M -debugcon file:debug.log  -s -S &


.PHONY: run
run: all
	@echo "Running Felix from HDD image..."
	@killall qemu-system-i386 || true
	@qemu-system-i386 \
		-drive file=build/disk.img,index=0,media=disk,format=raw,if=ide \
		-drive file=build/fat32.img,index=1,media=disk,format=raw,if=ide \
		-boot order=c \
		-netdev user,id=net0 \
		-device i82559er,netdev=net0,mac=52:54:00:12:34:56 \
		-no-reboot -no-shutdown -vga std -m 128M \
		-debugcon file:debug.log -serial stdio

# FAT32 is listed first in -drive order below (as requested), but IDE index
# keeps disk.img on 0x80 so the existing bootloader still boots.
.PHONY: debug
debug: all
	@echo "Debugging Felix..."
	@killall qemu-system-i386 || true
	@qemu-system-i386 \
		-drive file=build/fat32.img,index=1,media=disk,format=raw,if=ide \
		-drive file=build/disk.img,index=0,media=disk,format=raw,if=ide \
		-boot order=c \
		-no-reboot -d int,guest_errors -debugcon file:debug.log -no-shutdown \
		-netdev user,id=net0,dns=10.0.2.3 \
                 -device i82559er,netdev=net0,mac=52:54:00:12:34:56 \
                 -object filter-dump,id=f1,netdev=net0,file=guest.pcap \
		-m 128M -s -S &
