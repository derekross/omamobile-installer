// SPDX-License-Identifier: MIT
// One screen of the install flow, drawn as the core sent it (core/src/flow.rs,
// Screen): the title, the notice, the body, the quote, the steps, the
// choices, the link, the file, the plan, the problem and the footer, in that
// order, as the command-line tool prints them. Nothing here adds a sentence about the phone.
import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts

Flickable {
    id: view

    property var screen: ({})
    readonly property bool stopped: screen.state === "stopped"
    readonly property string firstCommand: {
        const steps = screen.steps || []
        for (let i = 0; i < steps.length; i++)
            if (steps[i].command)
                return steps[i].command
        return ""
    }
    property string shownState: ""

    function scrollBy(distance) {
        contentY = Math.max(0, Math.min(contentHeight - height, contentY + distance))
    }
    function submitAnswers() {
        if (formLoader.item)
            formLoader.item.submit()
    }
    // Brings an item of the screen into view: the field that has the focus,
    // or the core's line on why something was not taken.
    function reveal(item) {
        const top = item.mapToItem(view.contentItem, 0, 0).y
        const bottom = top + item.height
        if (top < contentY + 16)
            contentY = Math.max(0, top - 16)
        else if (bottom > contentY + height - 16)
            contentY = Math.max(0, Math.min(contentHeight - height, bottom - height + 16))
    }
    function openLink(url) {
        if (backend.isWebLink(url))
            Qt.openUrlExternally(url)
    }

    // A new step starts at its top; the same step again (a file that was not
    // taken, answers with a problem) stays where the person was.
    onScreenChanged: {
        const now = (screen.state || "") + "/" + (screen.stop || "")
        if (now !== shownState)
            contentY = 0
        shownState = now
        if (screen.problem)
            Qt.callLater(function() { view.reveal(problemBox) })
    }

    contentWidth: width
    contentHeight: column.implicitHeight + 56
    clip: true
    boundsBehavior: Flickable.StopAtBounds
    // Always there when the text is longer than the window, so nothing below
    // the edge goes unnoticed.
    ScrollBar.vertical: ScrollBar {
        id: bar
        policy: view.contentHeight > view.height ? ScrollBar.AlwaysOn : ScrollBar.AlwaysOff
        padding: 3
        contentItem: Rectangle {
            implicitWidth: 6
            radius: Math.min(3, theme.radius)
            color: bar.pressed ? theme.accent : "#4a4a52"
        }
        background: Item { implicitWidth: 12 }
    }

    ColumnLayout {
        id: column
        x: Math.max(32, (view.width - width) / 2)
        y: 28
        width: Math.min(view.width - 64, 760)
        spacing: 14

        Text {
            visible: view.stopped
            text: qsTr("Stopped")
            color: "#ff7b72"
            font.pixelSize: 12
            font.bold: true
            font.letterSpacing: 1.5
            font.capitalization: Font.AllUppercase
        }

        Para {
            objectName: "title"
            Layout.fillWidth: true
            text: view.screen.title || ""
            font.pixelSize: 26
            font.bold: true
        }

        // A check that was skipped: before everything else, and set apart.
        Rectangle {
            objectName: "notice"
            visible: !!view.screen.notice
            Layout.fillWidth: true
            implicitHeight: noticeRow.implicitHeight + 28
            radius: theme.radius
            color: "#2b1614"
            border.color: "#ff7b72"
            border.width: 2

            RowLayout {
                id: noticeRow
                anchors.fill: parent
                anchors.margins: 14
                spacing: 12

                Rectangle {
                    Layout.alignment: Qt.AlignTop
                    width: 24
                    height: 24
                    radius: 12
                    color: "#ff7b72"
                    Text { anchors.centerIn: parent; text: "!"; color: "#1a0d0c"; font.bold: true; font.pixelSize: 15 }
                }
                Para {
                    Layout.fillWidth: true
                    text: view.screen.notice || ""
                    font.bold: true
                    color: "#ffd9d6"
                }
            }
        }

        Repeater {
            model: view.screen.body || []
            Para { objectName: "body"; Layout.fillWidth: true; text: modelData }
        }

        // Someone else's words: set apart as theirs, with the pages they name.
        RowLayout {
            objectName: "quote"
            visible: !!view.screen.quote
            Layout.fillWidth: true
            Layout.topMargin: 4
            spacing: 16

            Rectangle { Layout.fillHeight: true; width: 3; color: theme.accent }

            ColumnLayout {
                Layout.fillWidth: true
                spacing: 10

                Para { Layout.fillWidth: true; text: view.screen.quote ? view.screen.quote.intro : ""; color: "#a8a8b2" }
                Repeater {
                    model: view.screen.quote ? view.screen.quote.lines : []
                    Para {
                        objectName: "quoteLine"
                        Layout.fillWidth: true
                        text: "“" + modelData + "”"
                        font.italic: true
                        font.pixelSize: 16
                    }
                }
                Repeater {
                    model: view.screen.quote ? view.screen.quote.after : []
                    Para { Layout.fillWidth: true; text: modelData; color: "#a8a8b2" }
                }
                Flow {
                    Layout.fillWidth: true
                    spacing: 8
                    Repeater {
                        model: view.screen.quote ? view.screen.quote.links : []
                        ActionButton {
                            objectName: "quoteLink"
                            enabled: backend.isWebLink(modelData.url)
                            text: modelData.label + "  ↗"
                            onClicked: view.openLink(modelData.url)
                            ToolTip.visible: hovered
                            ToolTip.text: modelData.url
                        }
                    }
                }
            }
        }

        Repeater {
            model: view.screen.steps || []
            RowLayout {
                objectName: "step"
                Layout.fillWidth: true
                Layout.topMargin: 2
                spacing: 14

                Rectangle {
                    Layout.alignment: Qt.AlignTop
                    width: 26
                    height: 26
                    radius: 13
                    color: theme.accent
                    Text {
                        anchors.centerIn: parent
                        text: (index + 1).toLocaleString(Qt.locale(), "f", 0)
                        color: theme.accentForeground
                        font.bold: true
                        font.pixelSize: 13
                    }
                }
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 8
                    Para { Layout.fillWidth: true; Layout.topMargin: 3; text: modelData.text }
                    CommandBox {
                        Layout.fillWidth: true
                        visible: !!modelData.command
                        command: modelData.command || ""
                    }
                }
            }
        }

        // The core's question, as its answers: one button each, in the core's
        // words and order. None of them is the main action.
        ColumnLayout {
            objectName: "choices"
            visible: (view.screen.choices || []).length > 0
            Layout.fillWidth: true
            Layout.topMargin: 4
            spacing: 10

            Repeater {
                model: view.screen.choices || []
                ActionButton {
                    objectName: "choiceButton"
                    Layout.fillWidth: true
                    text: modelData.label
                    enabled: !backend.busy
                    onClicked: backend.answerSlotB(modelData.answer)
                }
            }
        }

        // A page for the person to open themselves, in their browser.
        ColumnLayout {
            visible: !!view.screen.link
            Layout.fillWidth: true
            spacing: 0

            ActionButton {
                objectName: "linkButton"
                enabled: backend.isWebLink(view.screen.link || "")
                text: qsTr("Open the page in the browser") + "  ↗"
                onClicked: view.openLink(view.screen.link)
            }
            Para {
                Layout.fillWidth: true
                Layout.leftMargin: 4
                text: view.screen.link || ""
                color: "#a8a8b2"
                font.pixelSize: 13
            }
        }

        // The file the step asks for.
        Rectangle {
            objectName: "file"
            visible: !!view.screen.file
            Layout.fillWidth: true
            implicitHeight: fileColumn.implicitHeight + 28
            radius: theme.radius
            color: "#17171a"
            border.color: "#34343a"
            border.width: 1

            ColumnLayout {
                id: fileColumn
                anchors.fill: parent
                anchors.margins: 14
                spacing: 4

                Text {
                    text: qsTr("The file")
                    color: "#a8a8b2"
                    font.pixelSize: 12
                    font.bold: true
                    font.letterSpacing: 1.5
                    font.capitalization: Font.AllUppercase
                }
                Para {
                    objectName: "fileName"
                    Layout.fillWidth: true
                    text: view.screen.file ? view.screen.file.name : ""
                    font.family: "monospace"
                    wrapMode: TextEdit.WrapAnywhere
                }
                Para {
                    Layout.fillWidth: true
                    // Reads the language so the size is written again on a switch.
                    text: view.screen.file
                          ? (translator.language, qsTr("%1, build %2").arg(backend.formatBytes(view.screen.file.size)).arg(view.screen.file.build))
                          : ""
                    color: "#a8a8b2"
                    font.pixelSize: 13
                }
                Para {
                    Layout.fillWidth: true
                    visible: backend.action === "download" || backend.action === "take-file"
                    text: qsTr("You can also drop the saved file on this window.")
                    color: "#a8a8b2"
                    font.pixelSize: 13
                }
            }
        }

        // What an install would send, in order.
        Rectangle {
            objectName: "plan"
            visible: (view.screen.plan || []).length > 0
            Layout.fillWidth: true
            implicitHeight: planColumn.implicitHeight + 28
            radius: theme.radius
            color: "#17171a"
            border.color: "#34343a"
            border.width: 1

            ColumnLayout {
                id: planColumn
                anchors.fill: parent
                anchors.margins: 14
                spacing: 6

                Repeater {
                    model: view.screen.plan || []
                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 12
                        Text {
                            Layout.alignment: Qt.AlignTop
                            Layout.preferredWidth: 20
                            horizontalAlignment: Text.AlignRight
                            text: (index + 1).toLocaleString(Qt.locale(), "f", 0)
                            color: theme.accent
                            font.family: "monospace"
                            font.pixelSize: 13
                        }
                        Para {
                            objectName: "planLine"
                            Layout.fillWidth: true
                            text: modelData
                            font.family: "monospace"
                            font.pixelSize: 13
                            wrapMode: TextEdit.WrapAnywhere
                        }
                    }
                }
            }
        }

        // Why what was last handed in wasn't taken; the step is still open.
        Rectangle {
            id: problemBox
            visible: !!view.screen.problem
            Layout.fillWidth: true
            implicitHeight: problemText.implicitHeight + 24
            radius: theme.radius
            color: "#2b1614"

            Para {
                id: problemText
                objectName: "problem"
                anchors.fill: parent
                anchors.margins: 12
                text: view.screen.problem || ""
                color: "#ff9a92"
                font.bold: true
            }
        }

        Loader {
            id: formLoader
            objectName: "answersLoader"
            Layout.fillWidth: true
            Layout.topMargin: 10
            // Kept while the step is the same, so what was typed stays; gone
            // with the step, and the secrets with it.
            active: backend.action === "answers"
            visible: active
            source: "AnswersForm.qml"
            onLoaded: item.focused.connect(view.reveal)
        }

        // The closing lines: what is true of the phone, and what comes next.
        RowLayout {
            objectName: "footer"
            visible: (view.screen.footer || []).length > 0
            Layout.fillWidth: true
            Layout.topMargin: 6
            spacing: 16

            Rectangle { Layout.fillHeight: true; width: 3; color: "#4a4a52" }
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 6
                Repeater {
                    model: view.screen.footer || []
                    Para { objectName: "footerLine"; Layout.fillWidth: true; text: modelData; color: "#c4c4cc" }
                }
            }
        }

        // What the session that writes sent, as the tool reported it: the
        // commands themselves, in order. Not a sentence of the app's. While
        // the writes run, the newest few; afterwards the whole record, which
        // scrolls with the page.
        Rectangle {
            id: sentBox
            objectName: "sent"
            readonly property int newest: 8
            readonly property var lines: backend.writing ? backend.sentLines.slice(-newest) : backend.sentLines
            visible: backend.sentLines.length > 0
            Layout.fillWidth: true
            implicitHeight: sentColumn.implicitHeight + 28
            radius: theme.radius
            color: "#17171a"
            border.color: "#34343a"
            border.width: 1

            ColumnLayout {
                id: sentColumn
                anchors.fill: parent
                anchors.margins: 14
                spacing: 4

                RowLayout {
                    Layout.fillWidth: true
                    Layout.bottomMargin: 2
                    Text {
                        Layout.fillWidth: true
                        text: qsTr("Sent to the phone")
                        color: "#a8a8b2"
                        font.pixelSize: 12
                        font.bold: true
                        font.letterSpacing: 1.5
                        font.capitalization: Font.AllUppercase
                    }
                    Text {
                        objectName: "sentCount"
                        visible: backend.sentLines.length > sentBox.lines.length
                        text: qsTr("the last %1 of %2").arg(sentBox.lines.length).arg(backend.sentLines.length)
                        color: "#77777f"
                        font.pixelSize: 12
                    }
                }
                Repeater {
                    model: sentBox.lines
                    Para {
                        objectName: "sentLine"
                        Layout.fillWidth: true
                        text: modelData
                        font.family: "monospace"
                        font.pixelSize: 13
                        wrapMode: TextEdit.WrapAnywhere
                    }
                }
            }
        }
    }
}
