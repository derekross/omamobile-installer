#!/bin/bash
# SPDX-License-Identifier: MIT
# Build the web flasher spike into web/dist: the wasm core, its generated
# glue (wasm-bindgen, the version in Cargo.lock) and the static page.
#   web/build.sh            the page as it would ship
#   web/build.sh selftest   with the fake phone built in, for ?selftest
#   web/build.sh preview    with the install link's screens and its WebUSB
#                           transport (never what ships)
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$here/..
features=()
[[ ${1:-} == selftest ]] && features=(--features selftest)
[[ ${1:-} == preview ]] && features=(--features link-preview)
# wasm-bindgen reads the target_features section, which strip = true removes
CARGO_PROFILE_RELEASE_STRIP=false cargo build --manifest-path "$root/Cargo.toml" --release --target wasm32-unknown-unknown -p omamobile-installer-web "${features[@]}"
rm -rf "$here/dist"
mkdir -p "$here/dist/release"
wasm-bindgen --target web --no-typescript --remove-name-section --remove-producers-section --out-dir "$here/dist/pkg" \
  "$root/target/wasm32-unknown-unknown/release/omamobile_installer_web.wasm"
cp "$here"/static/* "$here/dist/"
# The published release's small files, same-origin for the demo. The real
# page fetches them from pkgs.omamobile.org (CORS is set there, RELEASES §6).
for f in SHA256SUMS SHA256SUMS.sig omamobile.gpg manifest.json MANIFEST device.conf; do
  cp "$root/fixtures/published/$f" "$here/dist/release/"
done
ls -l "$here/dist/pkg"
