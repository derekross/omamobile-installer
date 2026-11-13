// SPDX-License-Identifier: MIT
// omainstall — guides a person through installing Omarchy Mobile on a phone
// plugged into this computer. A front end: the installer's core decides every
// step and every sentence, through its command-line tool (docs/APP.md).

#include <QGuiApplication>
#include <QIcon>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQuickStyle>
#include <QTextStream>
#include <QTimer>
#include <QUrl>

#include "backend.h"
#include "cli.h"
#include "portalfilepicker.h"
#include "theme.h"
#include "translator.h"

int main(int argc, char *argv[]) {
    // The commands are told apart before any application object exists, so
    // `check` never needs a display.
    QStringList arguments;
    for (int i = 1; i < argc; ++i)
        arguments << QString::fromLocal8Bit(argv[i]);
    const QString command = arguments.value(0, "open");
    QTextStream out(stdout), err(stderr);
    if (command == "--help" || command == "-h") {
        out << Cli::usage();
        return 0;
    }
    if (command == "--version") {
        out << "omainstall " << APP_VERSION << "\n";
        return 0;
    }
    if (command == "check") {
        const bool json = arguments.mid(1) == QStringList{"--json"};
        if (arguments.size() > 1 && !json) {
            err << Cli::usage();
            return 1;
        }
        return Cli::check(argc, argv, json);
    }
    if (command != "open") {
        err << Cli::usage();
        return 1;
    }
    bool reinstall = false;
    // The one switch that lets this window's run write to a phone. It is
    // never implied, and nothing in the window sets it.
    bool write = false;
    QString release;
    for (const QString &argument : arguments.mid(1)) {
        if (argument == "--reinstall") {
            reinstall = true;
        } else if (argument == "--write") {
            write = true;
        } else if (!argument.startsWith('-') && release.isEmpty()) {
            release = argument;
        } else {
            err << Cli::usage();
            return 1;
        }
    }

    QGuiApplication app(argc, argv);
    app.setApplicationName("omainstall");
    app.setApplicationVersion(APP_VERSION);

    // Matches the window to omainstall.desktop, so the compositor (Wayland
    // app_id) and the launcher pick up the installed icon.
    app.setDesktopFileName("omainstall");
    app.setWindowIcon(QIcon::fromTheme("omainstall"));

    QQuickStyle::setStyle("Material");

    // Installed before anything is translated, and every qsTr() binding is
    // re-evaluated when the Omarchy language changes.
    Translator translator;
    app.installTranslator(&translator);

    Theme theme;
    PortalFilePicker picker;
    Backend backend(&picker);
    backend.setReinstall(reinstall);
    backend.setWrite(write);
    backend.setReleaseDirectory(release);

    QQmlApplicationEngine engine;
    QObject::connect(&translator, &Translator::changed, &engine, &QQmlEngine::retranslate);
    engine.rootContext()->setContextProperty("translator", &translator);
    engine.rootContext()->setContextProperty("theme", &theme);
    engine.rootContext()->setContextProperty("backend", &backend);
    engine.load(QUrl("qrc:/Main.qml"));
    if (engine.rootObjects().isEmpty())
        return 1;

    // The tool is told to leave however the window goes.
    QObject::connect(&app, &QGuiApplication::aboutToQuit, &backend, &Backend::shutdown);
    QTimer::singleShot(0, &backend, &Backend::start);
    return app.exec();
}
