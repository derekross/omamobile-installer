// SPDX-License-Identifier: MIT
// The shape (commands with --json, 0 or 1, errors on stderr) follows Hype's
// src/cli.cpp (MIT, David Heinemeier Hansson).
#include "cli.h"

#include <QCoreApplication>
#include <QJsonDocument>
#include <QJsonObject>
#include <QTextStream>
#include <QTimer>

#include "backend.h"

QString Cli::usage() {
    return QStringLiteral(
        "usage: omainstall [open [--reinstall] [RELEASE_DIR]]           the window, for a dry run: nothing is written to a phone\n"
        "       omainstall open --write [--reinstall] RELEASE_DIR       the window, for an install: after the dry run it lists what\n"
        "                                                               it will write, and writes once the phone's codename is typed\n"
        "       omainstall check [--json]                               find the installer's tool; say its version and protocol\n"
        "       omainstall --version | --help\n");
}

int Cli::check(int &argc, char **argv, bool json) {
    QCoreApplication app(argc, argv);
    app.setApplicationName("omainstall");
    app.setApplicationVersion(APP_VERSION);

    Backend backend;
    backend.setHelloTimeout(5000);
    bool ok = false;
    QObject::connect(&backend, &Backend::ready, &app, [&] {
        ok = true;
        // Left on the next turn of the loop, once the tool was told to go.
        QTimer::singleShot(0, &app, &QCoreApplication::quit);
    });
    QObject::connect(&backend, &Backend::toolErrorChanged, &app, [&] {
        if (!backend.toolErrorKind().isEmpty())
            QTimer::singleShot(0, &app, &QCoreApplication::quit);
    });
    QTimer::singleShot(0, &backend, &Backend::probe);
    app.exec();

    QTextStream out(stdout), err(stderr);
    QString error = backend.toolError();
    if (!backend.toolErrorDetail().isEmpty())
        error += " " + backend.toolErrorDetail();
    if (json) {
        QJsonObject report{{"ok", ok}, {"app_protocol", Backend::Protocol}};
        if (!backend.toolPath().isEmpty())
            report.insert("tool", backend.toolPath());
        if (backend.toolProtocol() != 0) {
            report.insert("version", backend.toolVersion());
            report.insert("protocol", backend.toolProtocol());
        }
        if (!ok) {
            report.insert("error_kind", backend.toolErrorKind());
            report.insert("error", error);
        }
        out << QJsonDocument(report).toJson(QJsonDocument::Compact) << "\n";
    } else if (ok) {
        out << "tool: " << backend.toolPath() << "\n"
            << "version: " << backend.toolVersion() << "\n"
            << "protocol: " << backend.toolProtocol() << " (this app speaks " << Backend::Protocol << ")\n";
    }
    if (!ok)
        err << "error: " << error << "\n";
    return ok ? 0 : 1;
}
