// SPDX-License-Identifier: MIT
// The window: layout and keys, nothing else. Every sentence about the phone
// on it is the installer's core's and arrives in `backend.screen`. The app's
// own words are the labels of its buttons, the shortcuts, and what it says
// when the installer's tool itself fails.
import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts

ApplicationWindow {
    id: win
    objectName: "window"
    width: 900
    height: 720
    minimumWidth: 720
    minimumHeight: 560
    visible: true
    title: qsTr("Omarchy Mobile Installer")
    color: "#0e0e10"

    Material.theme: Material.Dark
    Material.accent: theme.accent
    Material.roundedScale: theme.radius

    property bool helpVisible: false
    property bool confirmQuit: false
    property bool quitting: false
    // Someone tried to close the window while the phone is being written.
    property bool writeHold: false

    readonly property string action: backend.action
    readonly property bool toolFailed: backend.toolError !== ""
    readonly property bool choosingPhone: backend.phones.length > 0
    readonly property bool overlayOpen: helpVisible || confirmQuit || choosingPhone || writeHold
    // A step is in hand: the tool was given something and has not answered.
    readonly property bool stepRunning: backend.busy && backend.hasScreen
    readonly property bool knownAction: ["acknowledge", "read-phone", "slot-b", "accept-terms", "download", "take-file",
                                         "answers", "dry-run", "confirm", "write", "none"].indexOf(action) >= 0
    readonly property bool wantsFile: action === "download" || action === "take-file"
    // The keys below must not take a letter meant for a field, or Space and
    // Enter meant for the button that has the focus.
    readonly property bool typing: activeFocusItem instanceof TextInput && !activeFocusItem.readOnly
    readonly property bool onControl: activeFocusItem instanceof AbstractButton
    readonly property bool keysFree: !typing && !overlayOpen

    // A run that may write has one stage more, after the dry run.
    readonly property var stages: [
        { label: qsTr("Warning"), states: ["warning"] },
        { label: qsTr("Phone"), states: ["phone", "slot-b", "unlock"] },
        { label: qsTr("Factory image"), states: ["google-terms", "factory-image"] },
        { label: qsTr("About you"), states: ["answers"] },
        { label: qsTr("Dry run"), states: ["dry-run", "done"] }
    ].concat(backend.writes ? [{ label: qsTr("Install"), states: ["confirm", "write", "installed"] }] : [])
    readonly property int stage: {
        for (let i = 0; i < stages.length; i++)
            if (stages[i].states.indexOf(backend.state) >= 0)
                return i
        return -1
    }

    readonly property string primaryLabel: {
        if (toolFailed)
            return qsTr("Try again")
        switch (action) {
        case "acknowledge": return qsTr("I have read this. Go on")
        case "read-phone": return qsTr("Look at the phone")
        case "accept-terms": return qsTr("I accept. Download the file")
        case "download": return qsTr("Download")
        case "take-file": return qsTr("Choose the file…")
        case "answers": return qsTr("Go on")
        case "dry-run": return qsTr("Run the dry run")
        case "confirm": return qsTr("Erase the phone and install")
        // Reached by hand only when the writes could not begin (no phone
        // answered): the app sends them itself once the codename is taken.
        case "write": return backend.busy ? "" : qsTr("Look for the phone again")
        case "none": return qsTr("Close")
        }
        return ""
    }
    readonly property string secondaryLabel: {
        if (toolFailed)
            return qsTr("Close")
        switch (action) {
        case "acknowledge": return qsTr("Stop here")
        case "accept-terms": return qsTr("I will download it myself")
        case "download": return backend.pending === "download" ? qsTr("Cancel") : qsTr("Choose a file I saved…")
        case "confirm": return qsTr("Do not install")
        }
        // Offered where the core says a new run can get past the stop, and
        // nowhere else: the app does not read the stop's sentences.
        if (backend.canStartAgain)
            return qsTr("Start again")
        return backend.hasScreen && !knownAction ? qsTr("Close") : ""
    }
    readonly property string progressLabel: {
        if (!backend.hasProgress)
            return qsTr("Waiting for the installer…")
        switch (backend.progressWhat) {
        case "download": return qsTr("Downloading")
        case "check": return qsTr("Checking the file")
        case "dry-run": return qsTr("Running the dry run")
        case "verify": return qsTr("Checking the release's files")
        case "write": return qsTr("Writing to the phone")
        }
        return qsTr("Working")
    }

    function primary() {
        if (toolFailed)
            backend.restart()
        else if (backend.busy)
            return
        else if (action === "answers")
            screenView.submitAnswers()
        else if (action === "confirm")
            backend.confirmInstall(codenameField.text)
        else if (action === "none")
            win.close()
        else
            backend.primaryAction()
    }
    function secondary() {
        if (toolFailed || (backend.hasScreen && !knownAction))
            win.close()
        else if (action === "acknowledge")
            backend.decline()
        else if (action === "accept-terms")
            backend.declineTerms()
        else if (action === "confirm")
            backend.declineInstall()
        else if (action === "download" && backend.pending === "download")
            backend.cancelDownload()
        else if (action === "download")
            backend.pickFile()
        else if (backend.canStartAgain)
            backend.restart()
    }

    // Closing asks first while a step is running: a download or a look at
    // the phone would be cut off. While the phone is being written the
    // window does not close at all: there is no asking that makes a write
    // that was cut off safe.
    onClosing: function(close) {
        if (backend.writing) {
            close.accepted = false
            confirmQuit = false
            writeHold = true
        } else if (stepRunning && !quitting) {
            close.accepted = false
            confirmQuit = true
        }
    }
    onConfirmQuitChanged: if (confirmQuit) keepGoing.forceActiveFocus()
    onWriteHoldChanged: if (writeHold) holdOk.forceActiveFocus()
    // The write is over: the notice has nothing left to say.
    Connections {
        target: backend
        function onBusyChanged() {
            if (!backend.writing)
                win.writeHold = false
        }
    }

    Shortcut { sequence: "?"; context: Qt.ApplicationShortcut; enabled: !win.typing; onActivated: win.helpVisible = !win.helpVisible }
    Shortcut { sequence: "Q"; context: Qt.ApplicationShortcut; enabled: win.keysFree; onActivated: win.close() }
    Shortcut {
        sequences: ["Space", "Return", "Enter"]
        context: Qt.ApplicationShortcut
        enabled: win.keysFree && !win.onControl
        onActivated: win.primary()
    }
    Shortcut {
        sequence: "Escape"
        context: Qt.ApplicationShortcut
        onActivated: {
            if (win.helpVisible)
                win.helpVisible = false
            else if (win.writeHold)
                win.writeHold = false
            else if (win.confirmQuit)
                win.confirmQuit = false
            else if (win.choosingPhone)
                backend.dismissPhones()
            else
                backend.cancelDownload()
        }
    }
    Shortcut { sequence: "F"; context: Qt.ApplicationShortcut; enabled: win.keysFree && win.wantsFile; onActivated: backend.pickFile() }
    Shortcut {
        sequence: "O"
        context: Qt.ApplicationShortcut
        enabled: win.keysFree && !!backend.screen.link
        onActivated: screenView.openLink(backend.screen.link)
    }
    Shortcut {
        sequence: "C"
        context: Qt.ApplicationShortcut
        enabled: win.keysFree && screenView.firstCommand !== ""
        onActivated: backend.copy(screenView.firstCommand)
    }
    Shortcut { sequence: "Up"; context: Qt.ApplicationShortcut; enabled: win.keysFree; onActivated: screenView.scrollBy(-60) }
    Shortcut { sequence: "Down"; context: Qt.ApplicationShortcut; enabled: win.keysFree; onActivated: screenView.scrollBy(60) }
    Shortcut { sequence: "PgUp"; context: Qt.ApplicationShortcut; enabled: !win.overlayOpen; onActivated: screenView.scrollBy(-screenView.height * 0.85) }
    Shortcut { sequence: "PgDown"; context: Qt.ApplicationShortcut; enabled: !win.overlayOpen; onActivated: screenView.scrollBy(screenView.height * 0.85) }

    // Right-to-left languages mirror the window; the Translator sets the
    // application's layout direction.
    Item {
        anchors.fill: parent
        LayoutMirroring.enabled: Qt.application.layoutDirection === Qt.RightToLeft
        LayoutMirroring.childrenInherit: true

        // A file saved from the browser can be dropped anywhere on the window.
        DropArea {
            anchors.fill: parent
            enabled: win.wantsFile && !backend.busy
            onDropped: function(drop) {
                if (drop.hasUrls && drop.urls.length === 1)
                    backend.takeFileUrl(drop.urls[0])
            }
        }

        ColumnLayout {
            anchors.fill: parent
            spacing: 0

            // What this is, before anything else: a preview that cannot write.
            RowLayout {
                Layout.fillWidth: true
                Layout.margins: 20
                Layout.bottomMargin: 14
                spacing: 12

                Text {
                    text: qsTr("Omarchy Mobile Installer")
                    color: "#e6e6ea"
                    font.pixelSize: 15
                    font.bold: true
                }
                Rectangle {
                    implicitWidth: previewText.implicitWidth + 16
                    implicitHeight: previewText.implicitHeight + 6
                    radius: Math.min(4, theme.radius)
                    color: backend.writes ? "#2b1614" : "transparent"
                    border.color: backend.writes ? "#ff7b72" : theme.accent
                    border.width: 1
                    Text {
                        id: previewText
                        objectName: "mode"
                        anchors.centerIn: parent
                        text: backend.writes ? qsTr("Install") : qsTr("Dry run")
                        color: backend.writes ? "#ff9a92" : theme.accent
                        font.pixelSize: 12
                        font.bold: true
                    }
                }
                Text {
                    objectName: "banner"
                    Layout.fillWidth: true
                    text: backend.writes
                          ? qsTr("This run can write to a phone. It writes only after it has listed what it will write and you have typed the phone's codename.")
                          : qsTr("This run cannot write to a phone. It ends at a dry run.")
                    color: "#a8a8b2"
                    font.pixelSize: 13
                    wrapMode: Text.Wrap
                }
                Text {
                    text: qsTr("? for shortcuts")
                    color: "#77777f"
                    font.pixelSize: 13
                }
            }

            // Where the install is, among its steps.
            RowLayout {
                objectName: "stages"
                visible: win.stage >= 0 && !win.toolFailed
                Layout.fillWidth: true
                Layout.leftMargin: 20
                Layout.rightMargin: 20
                Layout.bottomMargin: 14
                spacing: 6

                Repeater {
                    model: win.stages
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 6
                        Rectangle {
                            Layout.fillWidth: true
                            height: 3
                            radius: Math.min(2, theme.radius)
                            color: index <= win.stage ? theme.accent : "#2a2a30"
                            opacity: index < win.stage ? 0.45 : 1
                        }
                        Text {
                            Layout.fillWidth: true
                            text: modelData.label
                            elide: Text.ElideRight
                            color: index === win.stage ? "#e6e6ea" : "#77777f"
                            font.pixelSize: 12
                            font.bold: index === win.stage
                        }
                    }
                }
            }

            Rectangle { Layout.fillWidth: true; height: 1; color: "#232328" }

            Item {
                Layout.fillWidth: true
                Layout.fillHeight: true

                ScreenView {
                    id: screenView
                    objectName: "screenView"
                    anchors.fill: parent
                    visible: backend.hasScreen && !win.toolFailed
                    screen: backend.screen
                }

                // Before the first screen.
                Text {
                    anchors.centerIn: parent
                    visible: !backend.hasScreen && !win.toolFailed
                    text: qsTr("Starting the installer…")
                    color: "#a8a8b2"
                    font.pixelSize: 16
                }

                // The tool itself failed. Said here, in the app's words and
                // then the tool's; nothing of an install is guessed.
                ColumnLayout {
                    objectName: "toolError"
                    visible: win.toolFailed
                    anchors.centerIn: parent
                    width: Math.min(parent.width - 96, 640)
                    spacing: 14

                    Text {
                        text: qsTr("The installer did not run")
                        color: "#ff7b72"
                        font.pixelSize: 12
                        font.bold: true
                        font.letterSpacing: 1.5
                        font.capitalization: Font.AllUppercase
                    }
                    Para {
                        objectName: "toolErrorLine"
                        Layout.fillWidth: true
                        text: backend.toolError
                        font.pixelSize: 20
                        font.bold: true
                    }
                    Rectangle {
                        visible: backend.toolErrorDetail !== ""
                        Layout.fillWidth: true
                        implicitHeight: detail.implicitHeight + 24
                        radius: theme.radius
                        color: "#17171a"
                        border.color: "#34343a"
                        border.width: 1
                        Para {
                            id: detail
                            objectName: "toolErrorDetail"
                            anchors.fill: parent
                            anchors.margins: 12
                            text: backend.toolErrorDetail
                            font.family: "monospace"
                            font.pixelSize: 13
                            wrapMode: TextEdit.WrapAnywhere
                        }
                    }
                    Para {
                        Layout.fillWidth: true
                        visible: backend.toolPath !== ""
                        text: qsTr("The tool: %1").arg(backend.toolPath)
                        color: "#a8a8b2"
                        font.pixelSize: 13
                        wrapMode: TextEdit.WrapAnywhere
                    }
                }
            }

            Rectangle { Layout.fillWidth: true; height: 1; color: "#232328" }

            // The step's controls: what is running, what went wrong with the
            // last action, and the one main action.
            ColumnLayout {
                Layout.fillWidth: true
                Layout.margins: 20
                Layout.topMargin: 14
                Layout.bottomMargin: 16
                spacing: 10

                ColumnLayout {
                    objectName: "progress"
                    visible: backend.busy && !win.toolFailed
                    Layout.fillWidth: true
                    spacing: 6

                    RowLayout {
                        Layout.fillWidth: true
                        Text {
                            Layout.fillWidth: true
                            text: win.progressLabel
                            color: "#c4c4cc"
                            font.pixelSize: 13
                        }
                        Text {
                            objectName: "progressBytes"
                            visible: backend.hasProgress && backend.progressTotal > 0
                            // Reads the language so the sizes are written again on a switch.
                            text: (translator.language, qsTr("%1 of %2").arg(backend.formatBytes(backend.progressDone)).arg(backend.formatBytes(backend.progressTotal)))
                            color: "#c4c4cc"
                            font.pixelSize: 13
                        }
                    }
                    ProgressBar {
                        objectName: "progressBar"
                        Layout.fillWidth: true
                        indeterminate: !(backend.hasProgress && backend.progressTotal > 0)
                        from: 0
                        to: 1
                        value: backend.progressTotal > 0 ? backend.progressDone / backend.progressTotal : 0
                    }
                }

                Para {
                    objectName: "actionError"
                    visible: text !== ""
                    Layout.fillWidth: true
                    text: backend.actionError !== "" ? backend.actionError : backend.pickerError
                    color: "#ff9a92"
                    font.bold: true
                    font.pixelSize: 14
                }
                Para {
                    objectName: "unknownAction"
                    visible: backend.hasScreen && !win.knownAction && !win.toolFailed
                    Layout.fillWidth: true
                    text: qsTr("The installer asks for a step this app does not know (%1). Update the app to go on.").arg(win.action)
                    color: "#ff9a92"
                    font.bold: true
                    font.pixelSize: 14
                }

                RowLayout {
                    Layout.fillWidth: true
                    spacing: 12

                    ActionButton {
                        id: secondaryButton
                        objectName: "secondaryButton"
                        visible: win.secondaryLabel !== "" && (!backend.writing || win.toolFailed)
                        text: win.secondaryLabel
                        enabled: (!backend.busy || backend.pending === "download" || win.toolFailed) && !backend.writing
                        onClicked: win.secondary()
                    }
                    Item { Layout.fillWidth: true }
                    // Where the core asks for the phone's codename. Beside
                    // the button and always in sight, however long the list
                    // above it is. What is typed is handed in as typed: the
                    // core decides what it is worth.
                    TextField {
                        id: codenameField
                        objectName: "codename"
                        visible: win.action === "confirm" && !win.toolFailed
                        Layout.preferredWidth: 250
                        Material.roundedScale: Math.min(4, theme.radius)
                        placeholderText: qsTr("The phone's codename")
                        font.family: "monospace"
                        inputMethodHints: Qt.ImhNoAutoUppercase | Qt.ImhNoPredictiveText
                        enabled: !backend.busy
                        onAccepted: win.primary()
                        // Gone with the step, like the answers.
                        onVisibleChanged: text = ""
                    }
                    Text {
                        visible: primaryButton.visible && primaryButton.enabled && !codenameField.visible
                        text: "Space"
                        color: "#77777f"
                        font.family: "monospace"
                        font.pixelSize: 12
                    }
                    ActionButton {
                        id: primaryButton
                        objectName: "primaryButton"
                        visible: win.primaryLabel !== ""
                        primary: true
                        text: win.primaryLabel
                        // The codename is typed first: an empty field is no go-ahead.
                        // And no new run while a tool that failed may still be writing.
                        enabled: ((!backend.busy && (win.action !== "confirm" || codenameField.text !== "")) || win.toolFailed) && !backend.writing
                        onClicked: win.primary()
                    }
                }
            }
        }

        // More than one phone answered: the tool asks which, in its words.
        Rectangle {
            objectName: "phoneChooser"
            visible: win.choosingPhone
            anchors.fill: parent
            color: "#d90e0e10"

            MouseArea { anchors.fill: parent }
            Rectangle {
                anchors.centerIn: parent
                width: Math.min(parent.width - 64, 520)
                height: chooser.implicitHeight + 56
                radius: theme.radius
                color: "#17171a"
                border.color: "#34343a"
                border.width: 1

                ColumnLayout {
                    id: chooser
                    anchors.fill: parent
                    anchors.margins: 28
                    spacing: 10

                    Para {
                        objectName: "phonesTitle"
                        Layout.fillWidth: true
                        Layout.bottomMargin: 6
                        text: backend.phonesTitle
                        font.pixelSize: 18
                        font.bold: true
                    }
                    Repeater {
                        model: backend.phones
                        ActionButton {
                            objectName: "phoneButton"
                            Layout.fillWidth: true
                            text: modelData.label
                            onClicked: backend.readPhone(modelData.id)
                        }
                    }
                    ActionButton {
                        Layout.alignment: Qt.AlignRight
                        text: qsTr("Cancel")
                        onClicked: backend.dismissPhones()
                    }
                }
            }
        }

        Rectangle {
            objectName: "quitDialog"
            visible: win.confirmQuit
            anchors.fill: parent
            color: "#d90e0e10"

            MouseArea { anchors.fill: parent }
            Rectangle {
                anchors.centerIn: parent
                width: Math.min(parent.width - 64, 460)
                height: quitColumn.implicitHeight + 56
                radius: theme.radius
                color: "#17171a"
                border.color: "#34343a"
                border.width: 1

                ColumnLayout {
                    id: quitColumn
                    anchors.fill: parent
                    anchors.margins: 28
                    spacing: 18

                    Text {
                        Layout.fillWidth: true
                        text: qsTr("A step is still running. Quit anyway?")
                        color: "#e6e6ea"
                        font.pixelSize: 18
                        font.bold: true
                        wrapMode: Text.Wrap
                    }
                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 12
                        ActionButton {
                            objectName: "quitAnyway"
                            text: qsTr("Quit")
                            onClicked: {
                                win.quitting = true
                                backend.withdraw()
                                win.close()
                            }
                        }
                        Item { Layout.fillWidth: true }
                        ActionButton {
                            id: keepGoing
                            objectName: "keepGoing"
                            primary: true
                            text: qsTr("Keep going")
                            onClicked: win.confirmQuit = false
                        }
                    }
                }
            }
        }

        // The window was asked to close while the phone is being written.
        Rectangle {
            objectName: "writeHold"
            visible: win.writeHold
            anchors.fill: parent
            color: "#d90e0e10"

            MouseArea { anchors.fill: parent }
            Rectangle {
                anchors.centerIn: parent
                width: Math.min(parent.width - 64, 480)
                height: holdColumn.implicitHeight + 56
                radius: theme.radius
                color: "#17171a"
                border.color: "#ff7b72"
                border.width: 1

                ColumnLayout {
                    id: holdColumn
                    anchors.fill: parent
                    anchors.margins: 28
                    spacing: 18

                    Text {
                        objectName: "writeHoldText"
                        Layout.fillWidth: true
                        text: qsTr("The installer is writing to the phone. This window stays open until that has ended.")
                        color: "#e6e6ea"
                        font.pixelSize: 18
                        font.bold: true
                        wrapMode: Text.Wrap
                    }
                    ActionButton {
                        id: holdOk
                        objectName: "writeHoldOk"
                        Layout.alignment: Qt.AlignRight
                        primary: true
                        text: qsTr("Keep going")
                        onClicked: win.writeHold = false
                    }
                }
            }
        }

        HelpOverlay {
            objectName: "help"
            visible: win.helpVisible
            anchors.fill: parent
            onDismissed: win.helpVisible = false
        }
    }
}
