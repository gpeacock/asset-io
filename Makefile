.PHONY: ci fmt clippy test

# Run the full CI preflight locally (mirrors .github/workflows/ci.yml)
ci: fmt clippy test

fmt:
	cargo fmt --check

clippy:
	cargo clippy --all-features -- -D warnings

test:
	cargo test --all-features
