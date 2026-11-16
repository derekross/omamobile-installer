#!/bin/bash
# SPDX-License-Identifier: MIT
# The page's download of a release (web/static/release-fetch.js), in node:
# against a stand-in for the release server on 127.0.0.1 (CORS and Range as
# the real one sends them, a made-up release signed with a test key) and a
# stand-in for the browser's Origin Private File System, in a directory node
# makes and removes. A whole download, one cut and one that goes quiet, a tab
# closed in the middle, a changed file, a signature that does not verify, a
# server without Range, too little room, a saved release and its .tar, and a
# download carried through the write preview's walk to a pretend phone
# (web/test/release-fetch.test.mjs). The same download in a real browser
# engine, with its own file system and its cross-origin rules, is part of the
# page's self-test (web/selftest.sh).
#
# Nothing is fetched but from 127.0.0.1, and no USB device is involved: node
# has none.
#
#   web/fetch-test.sh     build (web/build.sh web-write-preview selftest), run
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
command -v node >/dev/null || { echo "fetch-test: node is needed" >&2; exit 1; }
"$here/build.sh" web-write-preview selftest >/dev/null
node --test "$here/test/release-fetch.test.mjs"
