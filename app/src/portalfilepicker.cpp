// SPDX-License-Identifier: MIT
#include "portalfilepicker.h"

#include <QCoreApplication>
#include <QDBusConnection>
#include <QDBusConnectionInterface>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QUrl>

namespace {
const QString Service = QStringLiteral("org.freedesktop.portal.Desktop");
const QString Path = QStringLiteral("/org/freedesktop/portal/desktop");
const QString Chooser = QStringLiteral("org.freedesktop.portal.FileChooser");
const QString Request = QStringLiteral("org.freedesktop.portal.Request");
}

PortalFilePicker::PortalFilePicker(QObject *parent) : FilePicker(parent) {}

void PortalFilePicker::listen(const QString &path) {
    auto bus = QDBusConnection::sessionBus();
    if (!m_requestPath.isEmpty())
        bus.disconnect(Service, m_requestPath, Request, "Response", this, SLOT(response(uint, QVariantMap)));
    m_requestPath = path;
    if (!path.isEmpty())
        bus.connect(Service, path, Request, "Response", this, SLOT(response(uint, QVariantMap)));
}

void PortalFilePicker::open(const QString &title) {
    ask(title, false);
}

// The portal's `directory` option (version 3 of the interface) makes the
// same chooser pick a folder.
void PortalFilePicker::openFolder(const QString &title) {
    ask(title, true);
}

void PortalFilePicker::ask(const QString &title, bool folder) {
    if (m_open)
        return;
    auto bus = QDBusConnection::sessionBus();
    if (!bus.isConnected()) {
        emit failed(tr("there is no session bus"));
        return;
    }

    // The portal answers on a request object whose path follows from our bus
    // name and a token we pick, so listen there before calling: an answer
    // can't then arrive before anyone listens.
    const QString token = QStringLiteral("omainstall_%1_%2").arg(QCoreApplication::applicationPid()).arg(++m_serial);
    QString sender = bus.baseService().mid(1);
    sender.replace('.', '_');
    listen(QStringLiteral("/org/freedesktop/portal/desktop/request/%1/%2").arg(sender, token));

    QVariantMap options{{"handle_token", token}, {"multiple", false}, {"modal", true}};
    if (folder)
        options.insert("directory", true);
    QDBusMessage call = QDBusMessage::createMethodCall(Service, Path, Chooser, "OpenFile");
    // No parent window handle: exporting one is Wayland-protocol work the
    // app does not need for a single chooser.
    call << QString() << title << options;

    m_open = true;
    auto *watcher = new QDBusPendingCallWatcher(bus.asyncCall(call), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, &PortalFilePicker::called);
}

void PortalFilePicker::called(QDBusPendingCallWatcher *watcher) {
    watcher->deleteLater();
    const QDBusPendingReply<QDBusObjectPath> reply = *watcher;
    if (reply.isError()) {
        const QString reason = reply.error().message();
        done();
        emit failed(reason.isEmpty() ? tr("xdg-desktop-portal did not answer") : reason);
        return;
    }
    // An older portal may hand back another path than the one worked out.
    const QString path = reply.value().path();
    if (path != m_requestPath)
        listen(path);
}

void PortalFilePicker::response(uint code, const QVariantMap &results) {
    if (!m_open)
        return;
    done();
    // 0: a file was chosen. 1: the person closed the chooser. 2: it ended
    // some other way.
    if (code == 1) {
        emit cancelled();
        return;
    }
    const QStringList uris = results.value("uris").toStringList();
    const QUrl url(uris.value(0));
    if (code != 0 || !url.isLocalFile()) {
        emit failed(tr("the file chooser gave no file on this computer"));
        return;
    }
    emit picked(url.toLocalFile());
}

void PortalFilePicker::done() {
    m_open = false;
    listen(QString());
}
