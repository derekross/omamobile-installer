#!/bin/bash
# SPDX-License-Identifier: MIT
# Build the web flasher spike into web/dist: the wasm core, its generated
# glue (wasm-bindgen, the version in Cargo.lock) and the static page.
#   web/build.sh                             the page as it would ship, into web/dist
#   web/build.sh selftest                    with the fake phone built in, for ?selftest, into web/dist
#   web/build.sh web-write-preview           the write preview (docs/WEB-WRITE.md): a page that can
#                                            write to a phone, for an attended test. Into
#                                            web/dist-preview, never into web/dist
#   web/build.sh web-write-preview selftest  the write preview with the test bundle and its test
#                                            key, for web/preview-test.sh. Into web/dist-preview-test
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$here/..
selftest=0
preview=0
for arg in "$@"; do
  case $arg in
    selftest) selftest=1 ;;
    web-write-preview) preview=1 ;;
    *) echo "usage: web/build.sh [web-write-preview] [selftest]" >&2; exit 2 ;;
  esac
done
names=()
(( selftest )) && names+=(selftest)
(( preview )) && names+=(web-write-preview)
features=()
(( ${#names[@]} )) && features=(--features "$(IFS=,; echo "${names[*]}")")
# A build that can write never lands where the page that ships is built
dist=$here/dist
(( preview )) && dist=$here/dist-preview
(( preview && selftest )) && dist=$here/dist-preview-test
# wasm-bindgen reads the target_features section, which strip = true removes
CARGO_PROFILE_RELEASE_STRIP=false cargo build --manifest-path "$root/Cargo.toml" --release --target wasm32-unknown-unknown -p omamobile-installer-web "${features[@]}"
rm -rf "$dist"
mkdir -p "$dist/release"
wasm-bindgen --target web --no-typescript --remove-name-section --remove-producers-section --out-dir "$dist/pkg" \
  "$root/target/wasm32-unknown-unknown/release/omamobile_installer_web.wasm"
cp "$here"/static/* "$dist/"
if (( preview )); then
  # Its own first page, which says what this build is, and its two scripts
  cp "$here"/preview/index.html "$here"/preview/*.js "$dist/"
fi
if (( preview && selftest )); then
  # For the page's run in a headless browser (web/preview-test.sh): the
  # pretend phone at the USB level, the script that clicks through the page
  # in place of the page's own, and the test bundle's files to pick
  cp "$here"/preview/test/fake-usb.mjs "$here"/preview/test/page-run.mjs "$dist/"
  sed 's|src="preview.js"|src="page-run.mjs"|' "$here/preview/index.html" >"$dist/index.html"
  grep -q 'src="page-run.mjs"' "$dist/index.html"
  mkdir -p "$dist/test-release"
  cp "$root"/fixtures/selftest/images/*.img "$root/fixtures/selftest/root.img.zst" "$dist/test-release/"
fi
# The published release's small files, same-origin for the demo. The real
# page fetches them from pkgs.omamobile.org (CORS is set there, RELEASES §6).
for f in SHA256SUMS SHA256SUMS.sig omamobile.gpg manifest.json MANIFEST device.conf; do
  cp "$root/fixtures/published/$f" "$dist/release/"
done
ls -l "$dist/pkg"
