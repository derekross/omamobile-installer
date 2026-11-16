// SPDX-License-Identifier: MIT
// A paragraph of the installer's text: plain (nothing in it is markup),
// wrapped, and selectable so it can be copied into an issue.
import QtQuick

TextEdit {
    readOnly: true
    selectByMouse: true
    activeFocusOnTab: false
    wrapMode: TextEdit.Wrap
    textFormat: TextEdit.PlainText
    color: "#e6e6ea"
    selectionColor: theme.accent
    selectedTextColor: theme.accentForeground
    font.pixelSize: 15
}
