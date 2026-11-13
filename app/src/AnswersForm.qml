// SPDX-License-Identifier: MIT
// The owner's answers: the fields the flow takes today (core/src/flow.rs,
// AnswersInput). The form checks nothing itself. The core applies every rule
// and its objections come back per field, in its words.
import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts

ColumnLayout {
    id: form
    // Room for a field's label, which sits on its upper edge once the field
    // has text, and for the core's line under the field before it.
    spacing: 12

    // A field took the focus: the view scrolls it into sight.
    signal focused(Item field)

    // Read in problem(), so each field's line follows the core's answer.
    readonly property var problems: backend.problems

    function problem(field) {
        return problems.length > 0 ? backend.problemFor(field) : ""
    }

    function submit() {
        backend.submitAnswers({
            username: username.text,
            password: password.text,
            password_again: passwordAgain.text,
            hostname: hostname.text,
            timezone: timezone.text,
            wifi_ssid: wifiSsid.text,
            wifi_psk: wifiPsk.text
        })
    }

    Component.onCompleted: username.field.forceActiveFocus()

    component Field: ColumnLayout {
        id: one
        property alias text: input.text
        property alias field: input
        property alias label: input.placeholderText
        property bool secret: false
        property string name: ""
        readonly property string trouble: form.problem(name)

        Layout.fillWidth: true
        spacing: 2

        TextField {
            id: input
            objectName: "answer_" + one.name
            Layout.fillWidth: true
            Material.roundedScale: Math.min(4, theme.radius)
            echoMode: one.secret ? TextInput.Password : TextInput.Normal
            inputMethodHints: Qt.ImhNoAutoUppercase | Qt.ImhNoPredictiveText
            enabled: !backend.busy
            onAccepted: form.submit()
            onActiveFocusChanged: if (activeFocus) form.focused(one)
        }
        Para {
            objectName: "problem_" + one.name
            Layout.fillWidth: true
            Layout.bottomMargin: 6
            visible: one.trouble !== ""
            text: one.trouble
            color: "#ff7b72"
            font.pixelSize: 13
        }
    }

    Field { id: username; name: "username"; label: qsTr("Your user name on the phone") }
    Field { id: password; name: "password"; secret: true; label: qsTr("A PIN or password to unlock the phone") }
    Field { id: passwordAgain; name: "password_again"; secret: true; label: qsTr("The same again") }
    Field { id: hostname; name: "hostname"; label: qsTr("The phone's name (optional)") }
    Field { id: timezone; name: "timezone"; label: qsTr("Time zone"); text: backend.systemTimeZone }
    Field { id: wifiSsid; name: "wifi_ssid"; label: qsTr("Wi-Fi network (optional)") }
    Field { id: wifiPsk; name: "wifi_psk"; secret: true; label: qsTr("Wi-Fi password") }
}
