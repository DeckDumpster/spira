CARGO ?= cargo

.PHONY: build test dist install

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

# install — build a read-only release, gate it, then flip releases/current.
#
# Creates $SPIRA_RELEASES/<sha>/ from a git archive of HEAD, copies compiled
# binaries under bin/, writes a MANIFEST, makes the tree read-only, then runs
# the release's own pre-activate.sh before atomically renaming a temp symlink
# over releases/current. If the release directory already exists the cargo
# build is skipped; only the gate and the symlink flip run, so that rollback
# (make install with a prior COMMIT= on the branch) is cheap — and gated the
# same as a forward install, since a rollback target can be broken too.
#
# SPIRA_RELEASES  default: sibling directory <parent-of-repo>/spira-releases
# COMMIT          git ref to build; default: HEAD
install:
	@set -eu; \
	_root="$$(git rev-parse --show-toplevel)"; \
	_sha="$$(git rev-parse "$${COMMIT:-HEAD}")"; \
	: "$${SPIRA_RELEASES:=$$(dirname "$$_root")/spira-releases}"; \
	mkdir -p "$$SPIRA_RELEASES"; \
	_rel="$$SPIRA_RELEASES/$$_sha"; \
	if [ ! -e "$$_rel" ]; then \
	    command -v $(CARGO) >/dev/null 2>&1 || { \
	        printf 'make install: cargo not on PATH — install Rust (https://rustup.rs/)\n' >&2; exit 1; }; \
	    $(CARGO) build --release --workspace; \
	    mkdir -p "$$_rel/bin"; \
	    git archive "$$_sha" | tar -x -C "$$_rel"; \
	    for _b in loom broker czar-pass queue-watch spira-supervise spira-config spira; do \
	        cp "$$_root/target/release/$$_b" "$$_rel/bin/$$_b"; \
	        chmod +x "$$_rel/bin/$$_b"; \
	    done; \
	    cp "$$_root/cockpit/panel/target/release/panel" "$$_rel/bin/panel"; \
	    chmod +x "$$_rel/bin/panel"; \
	    { printf 'commit %%s\n' "$$_sha"; \
	      for _b in loom broker czar-pass queue-watch spira-supervise spira-config spira panel; do \
	          _h="$$(sha256sum "$$_rel/bin/$$_b" | awk '{print $$1}')"; \
	          printf 'bin/%%s %%s\n' "$$_b" "$$_h"; \
	      done; } > "$$_rel/MANIFEST"; \
	    chmod -R a-w "$$_rel"; \
	    printf 'install: built release %%s\n' "$$_sha"; \
	else \
	    printf 'install: release %%s already present — skipping build\n' "$$_sha"; \
	fi; \
	if [ -x "$$_rel/spira/pre-activate.sh" ]; then \
	    "$$_rel/spira/pre-activate.sh" "$$_rel" || { \
	        printf 'install: pre-activate failed for %s — current left unchanged\n' "$$_sha" >&2; \
	        exit 1; }; \
	else \
	    printf 'install: %s has no spira/pre-activate.sh — refusing to activate an unverifiable release\n' "$$_sha" >&2; \
	    exit 1; \
	fi; \
	_tmp="$$SPIRA_RELEASES/.current.new.$$$$"; \
	ln -sf "$$_sha" "$$_tmp" && mv -T "$$_tmp" "$$SPIRA_RELEASES/current"; \
	printf 'install: current -> %%s\n' "$$_sha"
