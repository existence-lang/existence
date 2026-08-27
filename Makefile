.PHONY: check build-release release

check:
	cargo clippy -- -D warnings
	cargo test
	cargo clippy --features sparql -- -D warnings
	cargo test --features sparql
	cargo fmt -- --check

build-release:
	cargo build --release

release: check
	version=$$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/'); \
	git tag "v$$version" && git push origin main "v$$version" && \
	cargo install --path .
