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
	@brew list e2tools > /dev/null || brew install e2tools     # ← добавлено
endif

ifeq ($(UNAME), Linux)
	@echo "Downloading Linux build tools..."
	# TODO: Download linux build tools
	# (на большинстве дистрибутивов достаточно: sudo apt install e2fsprogs e2tools)
endif



.PHONY: build
build:
	@cargo clean -p felix-kernel -p hello
	@echo "Building Felix..."
	@cargo build --target=x86_16-felix.json --package=felix-boot
	@cargo build --target=x86_16-felix.json --package=felix-bootloader
	@cargo build --target=x86_32-felix.json --package=felix-kernel
	@cargo build --target=x86_32-felix.json --package=hello --release
	@cargo build --target=x86_32-felix.json --package=shell --release


.PHONY: objcopy
objcopy:
	@echo "Copying Felix..."
	@mkdir -p build
	@$(OBJCOPY) -I elf32-i386 -O binary target/x86_16-felix/debug/felix-boot build/boot.bin
	@$(OBJCOPY) -I elf32-i386 -O binary target/x86_16-felix/debug/felix-bootloader build/bootloader.bin
	@$(OBJCOPY) -I elf32-i386 -O binary target/x86_32-felix/debug/felix-kernel build/kernel.bin
	@cp target/x86_32-felix/release/hello build/hello
	@cp target/x86_32-felix/release/shell build/shell


.PHONY: image
image:
	@echo "=== Creating clean 64 MiB disk image ==="
	@rm -f build/disk.img build/rootfs.img
	@dd if=/dev/zero of=build/disk.img bs=1M count=64

	@echo "=== Applying partition layout ==="
	@$(SFDISK) build/disk.img < disk.layout
	@echo "--- Partition table after first sfdisk ---"
	@$(SFDISK) --list build/disk.img

	@echo "=== Writing boot, bootloader and kernel ==="
	@dd if=build/boot.bin       of=build/disk.img bs=512 conv=notrunc
	@dd if=build/bootloader.bin of=build/disk.img bs=512 seek=2048 conv=notrunc
	@dd if=build/kernel.bin     of=build/disk.img bs=512 seek=4096 conv=notrunc

	@echo "=== Creating ext2 rootfs ==="
	@dd if=/dev/zero of=build/rootfs.img bs=512 count=94208
	@$(E2MKFS) -I 128 -O ^64bit,^metadata_csum,^dir_index,^ext_attr,^resize_inode build/rootfs.img

	@echo "=== Preparing files for copying ==="
	@mkdir -p build/apps
	@cp -f build/*.bin build/apps/ 2>/dev/null || true
	@cp -f build/shell build/apps/ 2>/dev/null || true
	@cp -f build/hello build/apps/ 2>/dev/null || true

	@echo "=== Copying files to ext2 partition ==="
	@for f in build/apps/*; do \
		if [ -f "$$f" ]; then \
			$(E2CP) -p "$$f" build/rootfs.img:/$$(basename "$$f"); \
			echo "  → $$(basename "$$f")"; \
		fi; \
	done

	@echo "=== Inserting ext2 partition back into disk ==="
	@dd if=build/rootfs.img of=build/disk.img bs=512 seek=36864 conv=notrunc

	@echo "=== Re-applying partition table (critical fix) ==="
	@$(SFDISK) build/disk.img < disk.layout

	@echo "=== FINAL partition table check ==="
	@$(SFDISK) --list build/disk.img

	@rm -f build/rootfs.img
	@echo "=== Disk image ready! ==="

.PHONY: clean
clean:
	@echo "Cleaning Felix..."
	@cargo clean
	@rm -rf build

.PHONY: run
run: all
	@echo "Running Felix..."
	@killall qemu-system-i386 || true

	@qemu-system-i386 -drive file=build/disk.img,index=0,media=disk,format=raw,if=ide -no-reboot -no-shutdown -m 64M -serial stdio

.PHONY: debug
debug: all
	@echo "Debugging Felix..."
	@killall qemu-system-i386 || true
	@qemu-system-i386 -drive file=build/disk.img,index=0,media=disk,format=raw,if=ide -no-reboot -d int,guest_errors -no-reboot -no-shutdown \
                                                                                                                                           -no-shutdown \
                                                                                                                                           -m 64M \
                                                                                                                                           -serial stdio -s -S &
