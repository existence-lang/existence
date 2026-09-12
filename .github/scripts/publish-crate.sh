#!/usr/bin/env bash
# Publish the package in the current working tree to crates.io, and say plainly
# when the registry credential is the thing that failed.
#
# Usage: publish-crate.sh
#
# Name and version come from the manifest, never from the tag. Tags are not a
# reliable name: `v0.1.0` of this repository packages `exkernel`, not
# `existence`, so a tag-derived lookup asks crates.io about a crate that has no
# such version, concludes it must publish, and only then discovers the package
# it is holding was published years ago under its old name.
#
# crates.io answers 403 `authentication failed` for a token that is absent,
# expired, regenerated, or scoped away from the crate, and cargo surfaces that
# as a bare registry error at the very end of the packaging build. Four
# consecutive releases (v0.8.0, v0.8.1, v0.9.0, v0.10.0) were lost to that
# message before anyone connected it to the repository secret.
#
# There is no cheap pre-flight for liveness: a token scoped to publish-update is
# itself rejected by /api/v1/me, so probing that endpoint would condemn a
# perfectly good credential. What IS cheap is checking that the secret exists
# and is shaped like a crates.io token, and turning cargo's 403 into a message
# that names the secret and the exact commands that replace it.
#
# Exit codes: 0 published or already there, 3 no readable manifest (a caller
# walking history should skip the commit), anything else a real failure.
set -uo pipefail

ua="existence-release (+https://github.com/existence-lang/existence)"

remedy() {
  cat >&2 <<'MSG'
Mint or regenerate a token at https://crates.io/settings/tokens
(scope publish-update, crate `existence`), then:
  passage edit btak/CARGO_REGISTRY_TOKEN
  passage show btak/CARGO_REGISTRY_TOKEN | gh secret set CARGO_REGISTRY_TOKEN -R existence-lang/existence
and re-run this workflow by hand (Actions -> Release -> Run workflow) to publish
every version crates.io is still missing, not just the next tag.
MSG
}

manifest="$(cargo metadata --no-deps --format-version 1 2>/dev/null \
  | python3 -c 'import json,sys; p = json.load(sys.stdin)["packages"][0]; print(p["name"], p["version"])' 2>/dev/null)"
if [ -z "$manifest" ]; then
  echo "::warning::no readable Cargo manifest here; skipping"
  exit 3
fi
name="${manifest%% *}"
version="${manifest##* }"

# Idempotent: a version already on crates.io (published by hand, or a re-run of
# this workflow) is a no-op, not a failure. A version crates.io does not have
# answers 404, the expected case on a fresh tag, so curl stays quiet about it.
if curl -fs -A "$ua" "https://crates.io/api/v1/crates/$name/$version" 2>/dev/null \
     | grep -q "\"num\":\"$version\""; then
  echo "$name $version is already on crates.io; nothing to publish"
  exit 0
fi

if [ -z "${CARGO_REGISTRY_TOKEN:-}" ]; then
  echo "::error::CARGO_REGISTRY_TOKEN is not set on this repository, so $name $version cannot be published."
  remedy
  exit 1
fi

# A crates.io token is `cio` followed by 32 characters; tokens minted before the
# prefix was introduced are a bare 32. Anything else is a different credential
# pasted into this secret — worth naming now rather than after a packaging build.
if ! printf '%s' "$CARGO_REGISTRY_TOKEN" | grep -Eq '^(cio)?[A-Za-z0-9]{32}$'; then
  echo "::warning::CARGO_REGISTRY_TOKEN is not shaped like a crates.io token (expected \`cio\` plus 32 characters); publishing anyway"
fi

log="$(mktemp)"
cargo publish --locked 2>&1 | tee "$log"
status=${PIPESTATUS[0]}

if [ "$status" -ne 0 ] && grep -qiE '403 Forbidden|authentication failed' "$log"; then
  echo "::error::crates.io rejected CARGO_REGISTRY_TOKEN for $name $version (403 authentication failed). The token is expired, revoked, regenerated, or scoped away from this crate — this is not a problem with the code."
  remedy
fi

exit "$status"
