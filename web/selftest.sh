#!/bin/bash
# SPDX-License-Identifier: MIT
# Run the page's self-test in a headless Chromium: the whole install flow and
# each way it stops, clicked through on the real page against the pretend
# phone built into a self-test build. Prints the result; fails unless every
# check passed.
#
# No USB device is involved, and the browser can't reach one: WebUSB is
# switched off for the run, and where bubblewrap is installed the browser
# gets an empty /dev and no /sys/bus/usb.
#
# Nothing is fetched from Google either: the downloads of the factory image
# go to a stand-in that web/serve.py runs on this computer. And the browser
# itself is held to this computer: it can resolve no host but 127.0.0.1, and
# its own background services (updates, sync, reports) are switched off, so
# neither the page nor the browser reaches Google or anyone else.
#
#   web/selftest.sh            build (web/build.sh selftest), serve, run
#   CHROMIUM=/path/to/chrome web/selftest.sh
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$here/..
browser=${CHROMIUM:-}
if [[ -z $browser ]]; then
  for b in chromium chromium-browser google-chrome google-chrome-stable; do
    if command -v "$b" >/dev/null; then browser=$b; break; fi
  done
fi
[[ -n $browser ]] || { echo "selftest: no Chromium found (set CHROMIUM)" >&2; exit 1; }

"$here/build.sh" selftest >/dev/null
work=$root/target/selftest
rm -rf "$work"; mkdir -p "$work/profile"
port=$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
# --stand-in: Google's server is a stand-in on this computer, and the page's
# policy lets it fetch from nowhere else, so the run reaches nothing of Google's
python3 "$here/serve.py" "$port" --stand-in >"$work/server.log" 2>&1 &
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
  "http://127.0.0.1:$port/?selftest" >"$work/browser.log" 2>&1 &
chrome=$!
for _ in $(seq 600); do grep -q "^selftest-result" "$work/server.log" && break; sleep 0.1; done
result=$(sed -n 's/^selftest-result //p' "$work/server.log" | head -1)
[[ -n $result ]] || {
  echo "selftest: the page sent no result in 60 s; see $work" >&2
  # Most often the browser, not the page: say what it said
  tail -n 20 "$work/browser.log" >&2 || true
  exit 1
}
python3 -c 'import json, sys
r = json.loads(sys.argv[1])
for k, v in r.items():
    if k not in ("pass", "failed"): print(f"  {v if isinstance(v, str) else str(v):6}  {k}")
print("self-test:", "passed" if r.get("pass") else "FAILED " + ", ".join(r.get("failed", [r.get("error", "")])))
sys.exit(0 if r.get("pass") else 1)' "$result"
