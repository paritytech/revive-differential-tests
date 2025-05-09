.PHONY: format clippy test-workspace test-cli

format:
	cargo fmt --all -- --check

clippy:
	cargo clippy --all -- -D warnings

test-workspace:
	cargo test --workspace -- --nocapture
