// SPDX-License-Identifier: MIT
// Every shortcut, on `?`.
import QtQuick
import QtQuick.Layouts

Rectangle {
    id: overlay
    signal dismissed()

    color: "#d90e0e10"

    readonly property var rows: [
        { keys: "Space, Enter", what: qsTr("The step's main action") },
        { keys: "Tab", what: qsTr("Move between the buttons and fields") },
        { keys: "F", what: qsTr("Choose the file you saved") },
        { keys: "O", what: qsTr("Open the step's page in the browser") },
        { keys: "C", what: qsTr("Copy the command shown") },
        { keys: "↑ ↓ PgUp PgDn", what: qsTr("Scroll") },
        { keys: "Esc", what: qsTr("Cancel a download, or close this") },
        { keys: "?", what: qsTr("Show or hide the shortcuts") },
        { keys: "Q", what: qsTr("Quit (asks first while a step is running; not while the phone is being written)") }
    ]

    MouseArea { anchors.fill: parent; onClicked: overlay.dismissed() }

    Rectangle {
        anchors.centerIn: parent
        width: Math.min(parent.width - 64, 520)
        height: grid.implicitHeight + 56
        radius: theme.radius
        color: "#17171a"
        border.color: "#34343a"
        border.width: 1

        MouseArea { anchors.fill: parent }

        ColumnLayout {
            id: grid
            anchors.fill: parent
            anchors.margins: 28
            spacing: 12

            Text {
                text: qsTr("Shortcuts")
                color: "#e6e6ea"
                font.pixelSize: 20
                font.bold: true
                Layout.bottomMargin: 4
            }
            Repeater {
                model: overlay.rows
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 16
                    Text {
                        Layout.preferredWidth: 150
                        text: modelData.keys
                        color: theme.accent
                        font.family: "monospace"
                        font.pixelSize: 14
                    }
                    Text {
                        Layout.fillWidth: true
                        text: modelData.what
                        color: "#e6e6ea"
                        font.pixelSize: 14
                        wrapMode: Text.Wrap
                    }
                }
            }
        }
    }
}
