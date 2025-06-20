TARGET := $(shell rustc -vV | sed -n 's/^host: //p')
VERSION := $(shell grep '^version' crates/edr_deno/Cargo.toml | head -n1 | cut -d '"' -f2)

.PHONY: deno-package

deno-package:
	cargo build --release -p edr_deno
	@ext=so; \
	[ -f target/release/libedr_deno.dylib ] && ext=dylib; \
	[ -f target/release/edr_deno.dll ] && ext=dll; \
	cp target/release/libedr_deno.$$ext crates/edr_deno/edr_deno.$(TARGET).$$ext; \
	tar czf nomicfoundation-edr-deno-$(VERSION).tgz crates/edr_deno/README.md crates/edr_deno/bindings/bindings.ts crates/edr_deno/edr_deno.$(TARGET).$$ext
	@echo "Created nomicfoundation-edr-deno-$(VERSION).tgz"
