TARGET := $(shell rustc -vV | sed -n 's/^host: //p')
VERSION := $(shell grep '^version' crates/edr_deno/Cargo.toml | head -n1 | cut -d '"' -f2)

.PHONY: deno-package

deno-package:
	cargo build --release -p edr_deno
	@ext=so; \
	[ -f target/release/libedr_deno.dylib ] && ext=dylib; \
	cp target/release/libedr_deno.$$ext crates/edr_deno/edr/edr_deno.$(TARGET).$$ext
	tar czf nomicfoundation-edr-deno-$(VERSION).tgz crates/edr_deno/edr/*
