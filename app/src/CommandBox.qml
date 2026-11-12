// SPDX-License-Identifier: MIT
// A command for the person to type in their own terminal. Shown and copied,
// never run: the app has no way to run it.
import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts

Rectangle {
    id: box
    property string command: ""
    property bool copied: false

    function copy() {
        backend.copy(command)
        copied = true
        copiedTimer.restart()
    }

    implicitHeight: row.implicitHeight + 16
    radius: Math.min(4, theme.radius)
    color: "#17171a"
    border.color: "#34343a"
    border.width: 1

    Timer { id: copiedTimer; interval: 1600; onTriggered: box.copied = false }

    RowLayout {
        id: row
        anchors.fill: parent
        anchors.leftMargin: 14
        anchors.rightMargin: 6
        spacing: 10

        Para {
            objectName: "command"
            Layout.fillWidth: true
            Layout.alignment: Qt.AlignVCenter
            text: box.command
            font.family: "monospace"
            font.pixelSize: 14
        }
        ActionButton {
            objectName: "copyButton"
            text: box.copied ? qsTr("Copied") : qsTr("Copy")
            onClicked: box.copy()
        }
    }
}
