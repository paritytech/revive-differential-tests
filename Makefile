.PHONY: format clippy test machete

format:
	cargo fmt --all -- --check

clippy:
	cargo clippy --all-features --workspace -- --deny warnings

test:
	cargo test --workspace -- --nocapture

machete:
	cargo install cargo-machete
	cargo machete
