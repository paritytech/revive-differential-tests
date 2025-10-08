.PHONY: format clippy test machete

format:
	cargo +nightly fmt --all -- --check

clippy:
	cargo clippy --all-features --workspace -- --deny warnings

machete:
	cargo install cargo-machete
	cargo machete crates

test: format clippy machete
	cargo test --workspace -- --nocapture

