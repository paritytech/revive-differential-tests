.PHONY: format clippy test-workspace test-cli

format:
	cargo fmt --all -- --check

clippy:
	cargo clippy --all-features --workspace -- --deny warnings

test-workspace:
	cargo test --workspace -- --nocapture
