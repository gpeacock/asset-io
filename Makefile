.PHONY: ci fmt clippy test fuzz-build fuzz fuzz-parse fuzz-write fuzz-xmp fuzz-mpf fuzz-conform fuzz-regress

# Run the full CI preflight locally (mirrors .github/workflows/ci.yml)
ci: fmt clippy test

fmt:
	cargo fmt --check

clippy:
	cargo clippy --all-features -- -D warnings

test:
	cargo test --all-features

# Fuzzing needs nightly and cargo-fuzz (cargo install cargo-fuzz); see fuzz/README.md.
# FUZZ_TIME is seconds per target.
FUZZ_TIME ?= 60

fuzz-build:
	./fuzz.sh build

fuzz:
	./fuzz.sh all $(FUZZ_TIME)

fuzz-parse:
	./fuzz.sh parse $(FUZZ_TIME)

fuzz-write:
	./fuzz.sh write $(FUZZ_TIME)

fuzz-xmp:
	./fuzz.sh xmp $(FUZZ_TIME)

fuzz-mpf:
	./fuzz.sh mpf $(FUZZ_TIME)

fuzz-conform:
	./fuzz.sh conform $(FUZZ_TIME)

# Replay the saved crash artifacts against the current code.
fuzz-regress:
	@set -e; for t in fuzz_parse fuzz_write fuzz_xmp fuzz_mpf fuzz_conform; do \
		for f in fuzz/artifacts/$$t/*; do \
			if [ -f "$$f" ]; then cargo +nightly fuzz run $$t "$$f"; fi; \
		done; \
	done
