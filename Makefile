TARGET := $(shell rustc -vV | sed -n 's/^host: //p')
VERSION := $(shell grep '^version' crates/edr_deno/Cargo.toml | head -n1 | cut -d '"' -f2)

.PHONY: deno-package

deno-package:
	cargo build --release -p edr_deno
	@case "$(TARGET)" in \
		*-apple-darwin) ext=dylib; src=target/release/libedr_deno.$$ext ;; \
		*-windows-*) ext=dll; src=target/release/edr_deno.$$ext ;; \
		*) ext=so; src=target/release/libedr_deno.$$ext ;; \
	esac; \
	cp $$src crates/edr_deno/edr/edr_deno.$(TARGET).$$ext
	tar czf nomicfoundation-edr-deno-$(VERSION).tgz crates/edr_deno/edr/*
