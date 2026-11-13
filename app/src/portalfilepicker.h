// SPDX-License-Identifier: MIT
#pragma once

#include <QDBusMessage>
#include <QDBusObjectPath>
#include <QVariantMap>

#include "filepicker.h"

class QDBusPendingCallWatcher;

// The file chooser of xdg-desktop-portal, over D-Bus
// (org.freedesktop.portal.FileChooser.OpenFile). Never QFileDialog: the
// portal's chooser is the desktop's own, and it works from a sandbox.
// The pattern is Omacut's (MIT, David Heinemeier Hansson).
class PortalFilePicker : public FilePicker {
    Q_OBJECT

public:
    explicit PortalFilePicker(QObject *parent = nullptr);

    void open(const QString &title) override;

private slots:
    void response(uint code, const QVariantMap &results);

private:
    void called(QDBusPendingCallWatcher *watcher);
    void listen(const QString &path);
    void done();

    QString m_requestPath;
    bool m_open = false;
    int m_serial = 0;
};
