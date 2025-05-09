.PHONY: format clippy test

format:
	cargo fmt --all -- --check

clippy:
	cargo clippy --all-features --workspace -- --deny warnings

test:
	cargo test --workspace -- --nocapture
