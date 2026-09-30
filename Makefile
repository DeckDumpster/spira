CARGO ?= cargo

.PHONY: build test dist

build:
	@command -v $(CARGO) >/dev/null 2>&1 || { \
	    printf 'make build: cargo not on PATH — install Rust (https://rustup.rs/)\n' >&2; exit 1; }
	$(CARGO) build --release --workspace

test:
	@command -v $(CARGO) >/dev/null 2>&1 || { \
	    printf 'make test: cargo not on PATH — install Rust (https://rustup.rs/)\n' >&2; exit 1; }
	$(CARGO) test --workspace

dist:
	@command -v $(CARGO) >/dev/null 2>&1 || { \
	    printf 'make dist: cargo not on PATH — install Rust (https://rustup.rs/)\n' >&2; exit 1; }
	$(CARGO) build --release --workspace
	bash spira/build-tarball.sh build --workspace .
