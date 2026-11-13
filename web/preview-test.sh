#!/bin/bash
# SPDX-License-Identifier: MIT
# The write preview's tests (docs/WEB-WRITE.md): the page's write path
# against a pretend phone behind a pretend WebUSB device. Two parts:
#
#   1. in node: the module's chooser and transport, the core's flow with the
#      write switch on, and the page's walk of the release's files, through
#      every way a transfer can go wrong (web/preview/test/write.test.mjs);
#   2. in a headless Chromium: the preview's page itself, clicked through an
#      install and its stops (web/preview/test/page-run.mjs).
#
# No USB device is involved, and neither part can reach one. Node has no USB.
# The browser is started as web/selftest.sh starts it: WebUSB switched off,
# and, where bubblewrap is installed, an empty /dev and no /sys/bus/usb; the
# page's test refuses to run in a browser that has WebUSB. The browser can
# resolve no host but 127.0.0.1.
#
#   web/preview-test.sh            build (web/build.sh web-write-preview selftest), run both
#   web/preview-test.sh --node     the first part only (no browser needed)
#   CHROMIUM=/path/to/chrome web/preview-test.sh
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$here/..
only_node=0
[[ ${1:-} == --node ]] && only_node=1
command -v node >/dev/null || { echo "preview-test: node is needed" >&2; exit 1; }

"$here/build.sh" web-write-preview selftest >/dev/null
node --test "$here/preview/test/write.test.mjs"
(( only_node )) && exit 0

browser=${CHROMIUM:-}
if [[ -z $browser ]]; then
  for b in chromium chromium-browser google-chrome google-chrome-stable; do
    if command -v "$b" >/dev/null; then browser=$b; break; fi
  done
fi
[[ -n $browser ]] || { echo "preview-test: no Chromium found (set CHROMIUM, or run with --node)" >&2; exit 1; }

work=$root/target/preview-test
rm -rf "$work"; mkdir -p "$work/profile"
port=$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
python3 "$here/serve.py" "$port" --dir dist-preview-test >"$work/server.log" 2>&1 &
server=$!
trap 'kill "$server" ${chrome:-} 2>/dev/null || true' EXIT
for _ in $(seq 50); do grep -q "^http://" "$work/server.log" && break; sleep 0.1; done

hide=()
if command -v bwrap >/dev/null && bwrap --dev-bind / / --dev /dev true 2>/dev/null; then
  hide=(bwrap --dev-bind / / --dev /dev --tmpfs /sys/bus/usb --tmpfs /run/udev --)
fi
"${hide[@]}" "$browser" --headless=new --no-first-run --disable-gpu --no-sandbox \
  --disable-blink-features=WebUSB --user-data-dir="$work/profile" \
  --host-resolver-rules='MAP * ~NOTFOUND, EXCLUDE 127.0.0.1' \
  --disable-background-networking --disable-component-update --disable-sync \
  --disable-domain-reliability --no-default-browser-check --no-pings \
  "http://127.0.0.1:$port/" >"$work/browser.log" 2>&1 &
chrome=$!
for _ in $(seq 600); do grep -q "^selftest-result" "$work/server.log" && break; sleep 0.1; done
result=$(sed -n 's/^selftest-result //p' "$work/server.log" | head -1)
[[ -n $result ]] || {
  echo "preview-test: the page sent no result in 60 s; see $work" >&2
  tail -n 20 "$work/browser.log" >&2 || true
  exit 1
}
python3 -c 'import json, sys
r = json.loads(sys.argv[1])
for k, v in r.items():
    if k not in ("pass", "failed"): print(f"  {v if isinstance(v, str) else str(v):6}  {k}")
print("the preview page, in a headless browser:", "passed" if r.get("pass") else "FAILED " + ", ".join(r.get("failed", [r.get("error", "")])))
sys.exit(0 if r.get("pass") else 1)' "$result"
