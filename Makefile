.PHONY: check build-release release

check:
	cargo clippy -- -D warnings
	cargo test
	cargo fmt -- --check

build-release:
	cargo build --release

release: check
	version=$$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/'); \
	git tag "v$$version" && git push origin main "v$$version" && \
	cargo install --path .
