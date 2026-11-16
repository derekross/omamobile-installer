// SPDX-License-Identifier: MIT
// The app's button: a Material Button with a plain face, so the main action
// is filled with the theme's accent, its label is black or white as the
// accent needs, and the corners are the windows' own.
import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material

Button {
    id: control

    // The one main action of a step.
    property bool primary: false

    topInset: 0
    bottomInset: 0
    leftPadding: 18
    rightPadding: 18
    topPadding: 0
    bottomPadding: 0
    font.pixelSize: 14
    font.bold: primary

    contentItem: Text {
        text: control.text
        font: control.font
        color: !control.enabled ? "#6a6a72" : control.primary ? theme.accentForeground : "#e6e6ea"
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
        elide: Text.ElideRight
    }

    background: Rectangle {
        implicitWidth: 64
        implicitHeight: 40
        radius: theme.radius
        color: {
            if (control.primary)
                return !control.enabled ? "#26262b" : control.down ? Qt.darker(theme.accent, 1.2)
                     : control.hovered ? Qt.lighter(theme.accent, 1.1) : theme.accent
            return control.down ? "#2a2a30" : control.hovered ? "#1f1f24" : "transparent"
        }
        border.width: control.visualFocus ? 2 : control.primary ? 0 : 1
        border.color: control.visualFocus ? (control.primary ? "#ffffff" : theme.accent) : "#3a3a42"
    }
}
