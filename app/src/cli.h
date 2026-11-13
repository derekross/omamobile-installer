// SPDX-License-Identifier: MIT
#pragma once

#include <QString>

// The commands that need no display, for agents and scripts.
namespace Cli {
QString usage();
// `omainstall check [--json]`: find the installer's tool, start it, read its
// hello and stop it again. Nothing is asked of a phone: before the app's
// `start` message the tool opens no USB device (docs/APP.md). Returns the
// exit status: 0 when the tool is there and speaks this app's protocol.
int check(int &argc, char **argv, bool json);
}
