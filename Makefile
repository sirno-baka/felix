UNAME := $(shell uname)

#MacOS tools
ifeq ($(UNAME), Darwin)
	SFDISK := $(shell brew --prefix util-linux)/sbin/sfdisk
	OBJCOPY := $(shell brew --prefix binutils)/bin/objcopy
endif

ifeq ($(UNAME), Linux)
	SFDISK := /sbin/sfdisk
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

# Native userspace = every apps/<name> except wasm-*
NATIVE_APPS := $(sort $(filter-out wasm-%,$(patsubst apps/%/Cargo.toml,%,$(wildcard apps/*/Cargo.toml))))
WASM_APPS   := $(sort $(patsubst apps/%/Cargo.toml,%,$(wildcard apps/wasm-*/Cargo.toml)))
ROOTFS_FILES := $(shell find rootfs -type f ! -name README ! -name '.gitkeep' 2>/dev/null)

.PHONY: all
all: get-deps build objcopy image
	@echo "Felix has been successfully built!"

.PHONY: get-deps
get-deps:
ifeq ($(UNAME), Darwin)
	@echo "Downloading MacOS build tools..."
	@brew list util-linux > /dev/null || brew install util-linux
	@brew list e2fsprogs > /dev/null || brew install e2fsprogs
	@brew list binutils > /dev/null || brew install binutils
	@brew list e2tools > /dev/null || brew install e2tools
endif

ifeq ($(UNAME), Linux)
	@echo "Downloading Linux build tools..."
endif

.PHONY: build
build:
	@echo "Building Felix..."
	@echo "  native apps: $(NATIVE_APPS)"
	@echo "  wasm apps:   $(WASM_APPS)"
	@cargo build --target=x86_16-felix.json --package=felix-boot --release -Z json-target-spec
	@cargo build --target=x86_16-felix.json --package=felix-bootloader -Z json-target-spec
	@cargo build --target=x86_16-felix.json --package=felix-bootloader --release -Z json-target-spec
	@cargo build --target=x86_32-felix.json --package=felix-kernel -Z json-target-spec
	@cargo build --target=x86_32-felix.json --package=felix-kernel --release -Z json-target-spec
	@for p in $(NATIVE_APPS); do \
		echo "  cargo build $$p"; \
		cargo build --target=x86_32-felix.json --package=$$p --release -Z json-target-spec; \
	done
	@for p in $(WASM_APPS); do \
		echo "  cargo build $$p (wasm)"; \
		cargo build --target=wasm32-wasip2 --package=$$p --release; \
	done

.PHONY: objcopy
objcopy:
	@echo "Copying Felix..."
	@mkdir -p build/apps
	@$(OBJCOPY) -I elf32-i386 -O binary -S --strip-all \
        target/x86_16-felix/release/felix-boot build/boot.bin
	@$(OBJCOPY) -I elf32-i386 -O binary target/x86_16-felix/debug/felix-bootloader build/bootloader.bin
	@$(OBJCOPY) -I elf32-i386 -O binary target/x86_32-felix/debug/felix-kernel build/kernel.bin
	@for p in $(NATIVE_APPS); do \
		cp -f target/x86_32-felix/release/$$p build/$$p; \
		cp -f target/x86_32-felix/release/$$p build/apps/$$p; \
		echo "  → build/$$p"; \
	done
	@for p in $(WASM_APPS); do \
		if [ -f target/wasm32-wasip2/release/$$p.wasm ]; then \
			cp -f target/wasm32-wasip2/release/$$p.wasm build/$$p.wasm; \
			echo "  → build/$$p.wasm"; \
		fi; \
	done
	@# keep historic /wasm name used by the shell
	@if [ -f build/wasm-hello.wasm ]; then cp -f build/wasm-hello.wasm build/wasm; fi

# Copy userspace + rootfs extras into an ext2 image ($1 = image path).
define populate_ext2
	@echo "=== Populating $(1) ==="
	@$(E2CP) -p build/kernel.bin $(1):/kernel.bin && echo "  → /kernel.bin"
	@for p in $(NATIVE_APPS); do \
		$(E2CP) -p build/$$p $(1):/$$p && echo "  → /$$p"; \
	done
	@for w in build/*.wasm; do \
		[ -f "$$w" ] || continue; \
		base=$$(basename $$w); \
		$(E2CP) -p $$w $(1):/$$base && echo "  → /$$base"; \
	done
	@if [ -f build/wasm ]; then $(E2CP) -p build/wasm $(1):/wasm && echo "  → /wasm"; fi
	@if [ -f build/busybox.wasm ]; then $(E2CP) -p build/busybox.wasm $(1):/busybox && echo "  → /busybox"; fi
	@if [ -d rootfs ]; then \
		find rootfs -type f ! -name README ! -name '.gitkeep' | while read -r f; do \
			rel=$${f#rootfs/}; \
			$(E2CP) -p "$$f" $(1):/$$rel && echo "  → /$$rel"; \
		done; \
	fi
endef

.PHONY: floppy-image
floppy-image:
	@echo "=== Creating 1.44 MB floppy image ==="
	@rm -f build/floppy.img
	@dd if=/dev/zero of=build/floppy.img bs=1K count=1440 status=none
	@dd if=build/boot.bin of=build/floppy.img bs=512 conv=notrunc status=none
	@dd if=build/bootloader.bin of=build/floppy.img bs=512 seek=1 conv=notrunc status=none
	@dd if=build/kernel.bin of=build/floppy.img bs=512 seek=65 conv=notrunc status=none
	@rm -f build/ext2.img || true
	@KERNEL_BYTES=$$(wc -c < build/kernel.bin); \
	KERNEL_SECTORS=$$(( (KERNEL_BYTES + 555) / 512 )); \
	EXT2_START_SECTOR=$$((65 + KERNEL_SECTORS)); \
	EXT2_SIZE_SECTORS=$$((2880 - EXT2_START_SECTOR)); \
	EXT2_SIZE_BYTES=$$((EXT2_SIZE_SECTORS * 512)); \
	echo "Kernel size: $$KERNEL_BYTES bytes ($$KERNEL_SECTORS sectors)"; \
	echo "EXT2 starts at sector: $$EXT2_START_SECTOR, size: $$EXT2_SIZE_BYTES bytes"; \
	dd if=/dev/zero of=build/ext2.img bs=1 count=$$EXT2_SIZE_BYTES status=none; \
	mkfs.ext2 -I 128 -O ^64bit,^metadata_csum,^dir_index,^ext_attr,^resize_inode build/ext2.img
	$(call populate_ext2,build/ext2.img)
	@KERNEL_BYTES=$$(wc -c < build/kernel.bin); \
	KERNEL_SECTORS=$$(( (KERNEL_BYTES + 555) / 512 )); \
	EXT2_START_SECTOR=$$((65 + KERNEL_SECTORS)); \
	dd if=build/ext2.img of=build/floppy.img bs=512 seek=$$EXT2_START_SECTOR conv=notrunc status=none
	@echo "=== Floppy image ready ==="
	@ls -lh build/floppy.img

.PHONY: image
image:
	@echo "=== Creating 32 MiB bootable disk (MBR | bootloader | ext2) ==="
	@rm -f build/disk.img build/rootfs.img
	@dd if=/dev/zero of=build/disk.img bs=1M count=32 status=none
	@$(SFDISK) build/disk.img < disk.layout
	@$(SFDISK) --list build/disk.img
	@dd if=build/boot.bin of=build/disk.img bs=512 conv=notrunc status=none
	@dd if=build/bootloader.bin of=build/disk.img bs=512 seek=1 conv=notrunc status=none
	@dd if=/dev/zero of=build/rootfs.img bs=512 count=63488 status=none
	@$(E2MKFS) -I 128 -O ^64bit,^metadata_csum,^dir_index,^ext_attr,^resize_inode build/rootfs.img
	$(call populate_ext2,build/rootfs.img)
	@dd if=build/rootfs.img of=build/disk.img bs=512 seek=2048 conv=notrunc status=none
	@$(SFDISK) build/disk.img < disk.layout
	@$(SFDISK) --list build/disk.img
	@mkdir -p pxe/assets/felix
	@cp -f build/disk.img pxe/assets/felix/disk.img 2>/dev/null || true
	@cp -f build/disk.img pxe/assets/disk.img 2>/dev/null || true
	@rm -f build/rootfs.img
	@ls -lh build/disk.img
	@echo "=== Disk image ready ==="

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

.PHONY: usb-image
usb-image:
	@mkdir -p build
	@if [ ! -f build/usb.img ]; then \
		echo "=== Creating 64 MiB USB stick (MBR + FAT16 partition) ==="; \
		dd if=/dev/zero of=build/usb.img bs=1M count=64 status=none; \
		printf 'label: dos\nstart=2048, type=0e\n' | $(SFDISK) build/usb.img; \
		if command -v mkfs.vfat >/dev/null 2>&1; then \
			mkfs.vfat -F 16 -n FELIXUSB --offset 2048 build/usb.img; \
		elif command -v newfs_msdos >/dev/null 2>&1; then \
			newfs_msdos -F 16 -v FELIXUSB -S 512 -o 2048 build/usb.img; \
		else \
			echo "install dosfstools (mkfs.vfat)"; \
		fi; \
	fi

.PHONY: run
run: all usb-image
	@echo "Running Felix from HDD image..."
	@killall qemu-system-i386 || true
	@qemu-system-i386 \
		-drive file=build/disk.img,index=0,media=disk,format=raw,if=ide \
		-boot order=c \
		-netdev user,id=net0 \
		-device rtl8139,netdev=net0,mac=52:54:00:12:34:56 \
		-device pci-ohci,id=ohci \
		-drive if=none,id=usbstick,format=raw,file=build/usb.img \
		-device usb-storage,bus=ohci.0,drive=usbstick \
		-no-reboot -no-shutdown -vga std -m 128M \
		-debugcon file:debug.log -serial stdio

.PHONY: debug
debug: all usb-image
	@echo "Debugging Felix..."
	@killall qemu-system-i386 || true
	@qemu-system-i386 \
		-drive file=build/disk.img,index=0,media=disk,format=raw,if=ide \
		-boot order=c \
		-no-reboot -d int,guest_errors -debugcon file:debug.log -no-shutdown \
		-netdev user,id=net0,dns=10.0.2.3 \
                 -device rtl8139,netdev=net0,mac=52:54:00:12:34:56 \
                 -object filter-dump,id=f1,netdev=net0,file=guest.pcap \
                 -device pci-ohci,id=ohci \
                 -drive if=none,id=usbstick,format=raw,file=build/usb.img \
                 -device usb-storage,bus=ohci.0,drive=usbstick \
		-m 128M -s -S &
