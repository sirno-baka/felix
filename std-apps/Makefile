TARGET := i686-unknown-popugos
CARGO ?= cargo
PROFILE ?= release
ROOTFS ?= ../rootfs
APPS := hello thread-smoke tokio-smoke reqwest-smoke ui-smoke filemanager rhai rhai-ide
LOCAL_CARGO_CONFIG := .cargo/config.local.toml
CARGO_CONFIG_ARG := $(if $(wildcard $(LOCAL_CARGO_CONFIG)),--config "$(LOCAL_CARGO_CONFIG)",)

.PHONY: all build install image run clean

all: build

build:
	$(CARGO) $(CARGO_CONFIG_ARG) build --target "$(TARGET)" --profile "$(PROFILE)"

install: build
	@mkdir -p "$(ROOTFS)/bin"
	@for app in $(APPS); do \
		cp -f "target/$(TARGET)/$(PROFILE)/$$app" "$(ROOTFS)/bin/$$app"; \
		echo "installed $(ROOTFS)/bin/$$app"; \
	done

image: install
	$(MAKE) -C .. image

run: install
	$(MAKE) -C .. run

clean:
	$(CARGO) $(CARGO_CONFIG_ARG) clean
