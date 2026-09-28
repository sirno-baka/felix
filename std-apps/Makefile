TARGET := i686-unknown-popugos
CARGO ?= cargo
PROFILE ?= release
ROOTFS ?= ../rootfs
APPS := hello thread-smoke tokio-smoke reqwest-smoke netbench telnetd ui-smoke filemanager rhai rhai-ide twitch-radio
LOCAL_CARGO_CONFIG := .cargo/config.local.toml
CARGO_CONFIG_ARG := $(if $(wildcard $(LOCAL_CARGO_CONFIG)),--config "$(LOCAL_CARGO_CONFIG)",)
SSE2_CARGO_CONFIG := .cargo/config.sse2.toml
SSE2_TARGET_DIR := target-sse2

.PHONY: all build build-sse2 install image run clean

all: build

build:
	$(CARGO) $(CARGO_CONFIG_ARG) build --target "$(TARGET)" --profile "$(PROFILE)"

build-sse2:
	CARGO_TARGET_DIR="$(SSE2_TARGET_DIR)" $(CARGO) $(CARGO_CONFIG_ARG) --config "$(SSE2_CARGO_CONFIG)" build --target "$(TARGET)" --profile "$(PROFILE)" -p twitch-radio

install: build build-sse2
	@mkdir -p "$(ROOTFS)/bin"
	@for app in $(APPS); do \
		cp -f "target/$(TARGET)/$(PROFILE)/$$app" "$(ROOTFS)/bin/$$app"; \
		echo "installed $(ROOTFS)/bin/$$app"; \
	done
	@cp -f "$(SSE2_TARGET_DIR)/$(TARGET)/$(PROFILE)/twitch-radio" "$(ROOTFS)/bin/twitch-radio-sse2"
	@echo "installed $(ROOTFS)/bin/twitch-radio-sse2"

image: install
	$(MAKE) -C .. image

run: install
	$(MAKE) -C .. run

clean:
	$(CARGO) $(CARGO_CONFIG_ARG) clean
	rm -rf "$(SSE2_TARGET_DIR)"
