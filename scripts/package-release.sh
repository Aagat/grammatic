#!/usr/bin/env bash
set -euo pipefail
: "${RELEASE_TAG:?Set RELEASE_TAG to a version tag}"
[[ "$RELEASE_TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.-]+)?$ ]] || {
  echo 'Expected vMAJOR.MINOR.PATCH (optionally with a prerelease suffix)' >&2
  exit 1
}
version=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["package"]["version"])')
[[ "${RELEASE_TAG#v}" == "$version" ]] || {
  echo 'Release tag must match Cargo.toml version' >&2
  exit 1
}
[[ "$(uname -s)" == Linux ]]
arch=$(uname -m)
case "$arch" in x86_64|aarch64) ;; *) echo "Unsupported architecture: $arch" >&2; exit 1;; esac
name="grammatic-${RELEASE_TAG}-linux-${arch}"
mkdir -p "dist/$name/frontend/dist"
install -m 755 target/release/grammatic "dist/$name/grammatic"
cp -R frontend/dist/client "dist/$name/frontend/dist/client"
cp -R deploy docs "dist/$name/"
cp README.md "dist/$name/"
sed 's/^scale_mac = .*/scale_mac = "00:00:00:00:00:00"/' config.toml > "dist/$name/config.toml"
test -f "dist/$name/frontend/dist/client/index.html"
tar -czf "dist/$name.tar.gz" -C dist "$name"
(cd dist && sha256sum "$name.tar.gz" > SHA256SUMS)
