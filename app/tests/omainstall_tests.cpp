// SPDX-License-Identifier: MIT
// The app against a stand-in for the installer's tool (tests/stand-in-tool),
// which replays screens recorded from the core's flow (tests/fixtures/screens),
// and, at the end of this file, against the real tool: `omamobile-install
// json` built with its pretend phone, which bin/test builds and names in
// OMAINSTALL_REAL_TOOL (without it those tests are skipped, and say so).
// No phone, no USB, nothing from the internet, and no window on the
// desktop: bin/test runs this offscreen.
#include <QDesktopServices>
#include <QDir>
#include <QFile>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQuickItem>
#include <QQuickStyle>
#include <QQuickWindow>
#include <QSignalSpy>
#include <QTcpServer>
#include <QTcpSocket>
#include <QTemporaryDir>
#include <QtTest>
#include <functional>

#include "backend.h"
#include "filepicker.h"
#include "theme.h"
#include "translator.h"

namespace {
const QString Tests = QStringLiteral(TESTS_DIR);
const QString StandIn = Tests + "/stand-in-tool";

// The picker the tests hand the backend: it opens nothing and answers when told.
class FakePicker : public FilePicker {
public:
    void open(const QString &title) override {
        ++opened;
        lastTitle = title;
    }
    int opened = 0;
    QString lastTitle;
};

// ---- The real tool, with its pretend phone --------------------------------

// The test bundle the tool's own tests use, and its stand-in factory image.
const QString Release = QDir(Tests + "/../../fixtures/selftest").absolutePath();
const QString ZipName = QStringLiteral("cheetah-test.260101.001-factory-31c2322a.zip");
const QString Zip = Release + "/factory/" + ZipName;

// `omamobile-install json`, the real one, as the app starts it: through a
// small script that adds what only a test may add, the pretend phones (and
// the stand-in for Google's server). The app itself passes `json` and the
// release, and nothing else. No USB device is opened: a tool built with the
// pretend phone and started with --fake-phone has no way to one.
struct RealTool {
    explicit RealTool(const QString &phones, const QString &google = {}, const QString &extra = {}) {
        const QString tool = qEnvironmentVariable("OMAINSTALL_REAL_TOOL");
        QFile script(directory.filePath("omamobile-install"));
        if (tool.isEmpty() || !script.open(QIODevice::WriteOnly))
            return;
        QString line = QStringLiteral("exec '%1' \"$@\" --fake-phone '%2' --work-dir '%3'").arg(tool, phones, directory.filePath("work"));
        if (!google.isEmpty())
            line += QStringLiteral(" --fake-google '%1'").arg(google);
        // What only a test adds besides: where the pretend phone's cable is pulled.
        if (!extra.isEmpty())
            line += " " + extra;
        // The tool's standard error holds the pretend phones' record.
        script.write(("#!/bin/sh\n" + line + " 2>>'" + directory.filePath("stderr") + "'\n").toUtf8());
        script.close();
        script.setPermissions(QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner);
        qputenv("OMAINSTALL_TOOL", script.fileName().toUtf8());
        ok = true;
    }
    static bool built() { return !qEnvironmentVariableIsEmpty("OMAINSTALL_REAL_TOOL"); }
    // What is wrong with what the pretend phones received, or nothing: read
    // from the tool's own record once it has left. Reads only: `getvar`
    // and, for a locked phone, the one question. No flash, no erase, no
    // set_active, no reboot, and no partition changed.
    QString nothingButReads(int *commands = nullptr) const {
        QFile file(directory.filePath("stderr"));
        if (!file.open(QIODevice::ReadOnly))
            return "the tool left no record";
        int runs = 0, received = 0;
        const QStringList lines = QString::fromUtf8(file.readAll()).split('\n');
        for (const QString &line : lines) {
            const qsizetype arrow = line.indexOf(" <- ");
            if (line.startsWith("pretend phone ") && arrow > 0) {
                const QString command = line.mid(arrow + 4);
                ++received;
                if (!command.startsWith("getvar:") && command != "flashing get_unlock_ability")
                    return "a pretend phone received: " + command;
            } else if (line.startsWith("pretend phone ") && line.contains("received ")) {
                ++runs;
                if (!line.endsWith(" 0 others; partitions written: 0"))
                    return line;
            }
        }
        if (commands)
            *commands = received;
        return runs > 0 ? QString() : "the tool's record has no count";
    }

    // Everything the pretend phones received that is not a question, in
    // order, over every run of the tool; and the partitions the last run's
    // phone had been written to, as its record counts them.
    QStringList notReads(int *partitions = nullptr) const {
        QFile file(directory.filePath("stderr"));
        QStringList out;
        if (!file.open(QIODevice::ReadOnly))
            return out;
        const QStringList lines = QString::fromUtf8(file.readAll()).split('\n');
        for (const QString &line : lines) {
            const qsizetype arrow = line.indexOf(" <- ");
            if (line.startsWith("pretend phone ") && arrow > 0) {
                const QString command = line.mid(arrow + 4);
                if (!command.startsWith("getvar:") && command != "flashing get_unlock_ability")
                    out << command;
            } else if (partitions && line.contains("partitions written: ")) {
                *partitions = line.section("partitions written: ", 1).toInt();
            }
        }
        return out;
    }

    QTemporaryDir directory;
    bool ok = false;
};

// A stand-in for Google's server, for the real tool's downloads: a socket on
// 127.0.0.1 in this process. Nothing here reaches Google.
class Google : public QObject {
public:
    // The file; its first 20000 bytes and then nothing more; a file of the
    // right size with one byte changed; its first 20000 bytes and then the
    // connection closed; or no such file.
    enum Mode { Whole, Stalls, Wrong, Breaks, Missing };
    explicit Google(Mode mode) : m_mode(mode) {
        QFile file(Zip);
        if (file.open(QIODevice::ReadOnly))
            m_file = file.readAll();
        if (mode == Wrong && !m_file.isEmpty())
            m_file[m_file.size() / 2] = char(m_file.at(m_file.size() / 2) ^ 1);
        connect(&m_server, &QTcpServer::newConnection, this, [this] {
            while (QTcpSocket *socket = m_server.nextPendingConnection()) {
                connect(socket, &QTcpSocket::readyRead, this, [this, socket] { answer(socket); });
                connect(socket, &QTcpSocket::disconnected, socket, &QObject::deleteLater);
            }
        });
        m_server.listen(QHostAddress::LocalHost);
    }
    QString address() const { return "127.0.0.1:" + QString::number(m_server.serverPort()); }
    qsizetype size() const { return m_file.size(); }
    // From the next request on, the whole file (or the rest that is asked for).
    void goOn() { m_mode = Whole; }
    int asked = 0;
    QStringList paths;
    // The first byte each request asked for (0: the whole file).
    QList<qsizetype> froms;

private:
    void answer(QTcpSocket *socket) {
        const QByteArray head = socket->property("head").toByteArray() + socket->readAll();
        socket->setProperty("head", head);
        if (!head.contains("\r\n\r\n") || socket->property("answered").toBool())
            return;
        socket->setProperty("answered", true);
        ++asked;
        paths << QString::fromLatin1(head.split(' ').value(1));
        static const QRegularExpression rest("\r\nrange: bytes=(\\d+)-\r\n", QRegularExpression::CaseInsensitiveOption);
        const QRegularExpressionMatch range = rest.match(QString::fromLatin1(head));
        const qsizetype from = range.hasMatch() ? range.captured(1).toLongLong() : 0;
        froms << from;
        const auto n = [](qsizetype number) { return QByteArray::number(number); };
        if (m_mode == Missing) {
            socket->write("HTTP/1.1 404 Stand-in\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
            socket->disconnectFromHost();
            return;
        }
        QByteArray reply = range.hasMatch()
                               ? "HTTP/1.1 206 Stand-in\r\nContent-Range: bytes " + n(from) + "-" + n(m_file.size() - 1) + "/" + n(m_file.size()) + "\r\n"
                               : QByteArray("HTTP/1.1 200 Stand-in\r\n");
        reply += "Connection: close\r\nContent-Length: " + n(m_file.size() - from) + "\r\n\r\n";
        socket->write(reply);
        if (m_mode == Stalls) {
            socket->write(m_file.mid(from, 20000));
            socket->flush();
            return;
        }
        if (m_mode == Breaks) {
            socket->write(m_file.mid(from, 20000));
            socket->disconnectFromHost();
            return;
        }
        socket->write(m_file.mid(from));
        socket->disconnectFromHost();
    }

    Mode m_mode;
    QByteArray m_file;
    QTcpServer m_server;
};

// Waits until the tool has answered and the flow is at `state`.
bool at(const Backend &backend, const char *state) {
    return QTest::qWaitFor([&] { return !backend.busy() && backend.state() == state; }, 20000);
}

const QVariantMap GoodAnswers{{"username", "alex"}, {"password", "481516"}, {"password_again", "481516"}, {"hostname", ""},
                              {"timezone", "Europe/Paris"}, {"wifi_ssid", ""}, {"wifi_psk", ""}};

void standIn(const QByteArray &mode) {
    qputenv("OMAINSTALL_TOOL", StandIn.toUtf8());
    qputenv("OMAINSTALL_STANDIN", mode);
    qunsetenv("OMAINSTALL_STANDIN_LOG");
    qunsetenv("OMAINSTALL_STANDIN_DONE");
}

// The straight path of a run to the step in `until`, as the backend takes
// it: for a stand-in or the real tool with a file handed in.
bool goTo(Backend &backend, const char *until, const QString &zip) {
    for (int step = 0; step < 12; ++step) {
        if (!QTest::qWaitFor([&] { return !backend.busy() && backend.hasScreen(); }, 20000))
            return false;
        if (backend.state() == until)
            return true;
        const QString action = backend.action();
        if (action == "acknowledge")
            backend.acknowledge();
        else if (action == "read-phone")
            backend.readPhone();
        else if (action == "accept-terms")
            backend.declineTerms();
        else if (action == "take-file")
            backend.takeFile(zip);
        else if (action == "answers")
            backend.submitAnswers(GoodAnswers);
        else if (action == "dry-run")
            backend.dryRun();
        else
            return false;
    }
    return false;
}

QStringList fixtureNames() {
    QStringList names;
    const auto files = QDir(Tests + "/fixtures/screens").entryList({"*.json"}, QDir::Files, QDir::Name);
    for (const QString &file : files)
        names << file.chopped(5);
    return names;
}

QJsonObject fixture(const QString &name) {
    QFile file(Tests + "/fixtures/screens/" + name + ".json");
    if (!file.open(QIODevice::ReadOnly))
        return {};
    return QJsonDocument::fromJson(file.readAll()).object();
}

QJsonObject json(const QByteArray &line) {
    return QJsonDocument::fromJson(line).object();
}

// One line on the wire: a single JSON object, then a newline, and no other.
bool oneLine(const QByteArray &line) {
    return line.endsWith('\n') && line.count('\n') == 1 && QJsonDocument::fromJson(line).isObject();
}

// The label the window gives each action's main button.
QString primaryLabel(const QString &action) {
    static const QHash<QString, QString> labels{
        {"acknowledge", "I have read this. Go on"}, {"read-phone", "Look at the phone"},
        {"accept-terms", "I accept. Download the file"}, {"download", "Download"},
        {"take-file", "Choose the file…"}, {"answers", "Go on"}, {"dry-run", "Run the dry run"},
        {"confirm", "Erase the phone and install"}, {"write", "Look for the phone again"}, {"none", "Close"}};
    return labels.value(action);
}

// What Qt said while the window was up. A QML warning fails the test.
QStringList *g_warnings = nullptr;
void collect(QtMsgType type, const QMessageLogContext &context, const QString &message) {
    const QString category = QString::fromLatin1(context.category ? context.category : "");
    // The offscreen platform says what it lacks; that is not the window's fault.
    if (g_warnings && type != QtDebugMsg && type != QtInfoMsg && !category.startsWith("qt.qpa"))
        g_warnings->append(category + ": " + message);
    fprintf(stderr, "%s\n", qPrintable(message));
}

// The window as the app builds it, around a backend the test owns.
struct Window {
    explicit Window(Backend *backend)
        : theme(directory.path()), translator(directory.path() + "/language", ":/i18n") {
        g_warnings = &warnings;
        previous = qInstallMessageHandler(collect);
        QObject::connect(&engine, &QQmlApplicationEngine::warnings, &engine, [this](const QList<QQmlError> &errors) {
            for (const auto &error : errors)
                warnings << error.toString();
        });
        engine.rootContext()->setContextProperty("translator", &translator);
        engine.rootContext()->setContextProperty("theme", &theme);
        engine.rootContext()->setContextProperty("backend", backend);
        engine.load(QUrl("qrc:/Main.qml"));
        window = engine.rootObjects().isEmpty() ? nullptr : qobject_cast<QQuickWindow *>(engine.rootObjects().first());
    }
    ~Window() {
        qInstallMessageHandler(previous);
        g_warnings = nullptr;
    }
    // By the tree of what is drawn: what a Repeater or a Loader made is in
    // it, and is not among the QObject children.
    QList<QQuickItem *> items(const QString &name) const {
        QList<QQuickItem *> found;
        std::function<void(QQuickItem *)> walk = [&](QQuickItem *item) {
            if (item->objectName() == name)
                found << item;
            for (QQuickItem *child : item->childItems())
                walk(child);
        };
        if (window)
            walk(window->contentItem());
        return found;
    }
    QQuickItem *find(const QString &name) const { return items(name).value(0); }
    QString text(const QString &name) const {
        QQuickItem *item = find(name);
        return item ? item->property("text").toString() : QStringLiteral("<no %1>").arg(name);
    }
    bool shown(const QString &name) const {
        QQuickItem *item = find(name);
        return item && item->isVisible();
    }
    // Every visible item with this name, in the order drawn.
    QStringList texts(const QString &name) const {
        QStringList out;
        for (QQuickItem *item : items(name))
            if (item->isVisible())
                out << item->property("text").toString();
        return out;
    }
    bool click(const QString &name) const {
        QQuickItem *item = find(name);
        return item && item->isVisible() && item->isEnabled() && QMetaObject::invokeMethod(item, "clicked");
    }
    void settle() const {
        for (int i = 0; i < 3; ++i)
            QCoreApplication::processEvents();
    }

    QTemporaryDir directory;
    Theme theme;
    Translator translator;
    QStringList warnings;
    QtMessageHandler previous = nullptr;
    QQmlApplicationEngine engine;
    QQuickWindow *window = nullptr;
};
}

class OmainstallTests : public QObject {
    Q_OBJECT

private slots:
    void initTestCase() {
        QQuickStyle::setStyle("Material");
        QGuiApplication::setQuitOnLastWindowClosed(false);
        QVERIFY2(QFileInfo(StandIn).isExecutable(), "tests/stand-in-tool must be executable");
        // No test may open the person's browser: links are counted instead.
        QDesktopServices::setUrlHandler("https", this, "linkOpened");
        QDesktopServices::setUrlHandler("http", this, "linkOpened");
    }

    void cleanupTestCase() {
        QDesktopServices::unsetUrlHandler("https");
        QDesktopServices::unsetUrlHandler("http");
    }

    // ---- The recorded screens ------------------------------------------

    void theFixturesCoverEveryStateAndEveryStop() {
        QSet<QString> states, stops;
        for (const QString &name : fixtureNames()) {
            const QJsonObject screen = fixture(name);
            QVERIFY2(Backend::checkScreen(screen).isEmpty(), qPrintable(name + ": " + Backend::checkScreen(screen)));
            states << screen.value("state").toString();
            if (screen.value("stop").isString())
                stops << screen.value("stop").toString();
        }
        // core/src/flow.rs: Screen::state, and Stop::id
        for (const char *state : {"warning", "phone", "unlock", "google-terms", "factory-image", "answers", "dry-run", "done",
                                  "confirm", "write", "installed", "stopped"})
            QVERIFY2(states.contains(state), state);
        // Every id of docs/DESIGN.md §4
        const QStringList ids{"release-not-verified", "no-build-index", "build-index-refused", "warning-declined",
                              "wrong-model", "phone-changed", "runs-from-slot-b", "bootloader-slots-unknown",
                              "bootloader-slots-differ", "bootloader-slots-stated-other", "baseband-unknown",
                              "build-not-listed", "build-not-tested", "factory-download-mismatch", "locked-at-dry-run",
                              "build-changed", "dry-run-refused", "override-in-an-install", "writes-not-for-this-device",
                              "images-not-checked", "not-confirmed", "locked-at-write", "write-refused", "write-interrupted"};
        for (const QString &stop : ids)
            QVERIFY2(stops.contains(stop), qPrintable(stop));
        // And none the list above does not know: a new stop in the core
        // must be added here, and looked at in the window.
        for (const QString &stop : std::as_const(stops))
            QVERIFY2(ids.contains(stop), qPrintable(stop));
    }

    void everyScreenReachesQmlAsItWasSent_data() {
        QTest::addColumn<QString>("name");
        for (const QString &name : fixtureNames())
            QTest::newRow(qPrintable(name)) << name;
    }
    void everyScreenReachesQmlAsItWasSent() {
        QFETCH(QString, name);
        standIn("screen:" + name.toUtf8());
        Backend backend;
        QVERIFY(backend.busy() == false);
        backend.start();
        QVERIFY(backend.busy());
        QTRY_VERIFY(backend.hasScreen());
        const QJsonObject sent = fixture(name);
        QCOMPARE(QJsonObject::fromVariantMap(backend.screen()), sent);
        QCOMPARE(backend.state(), sent.value("state").toString());
        QCOMPARE(backend.action(), sent.value("action").toString());
        QVERIFY(!backend.busy());
        QCOMPARE(backend.toolError(), QString());
        QCOMPARE(backend.toolProtocol(), Backend::Protocol);
        QCOMPARE(backend.toolVersion(), QString("0.0.0-stand-in"));
    }

    // ---- What the app sends ----------------------------------------------

    void theAppSaysHelloThenStartAndNothingElse() {
        standIn("screen:warning");
        Backend backend;
        QSignalSpy sent(&backend, &Backend::sent);
        backend.start();
        QTRY_VERIFY(backend.hasScreen());
        QCOMPARE(sent.count(), 2);
        QVERIFY(oneLine(sent.at(0).at(0).toByteArray()));
        QCOMPARE(json(sent.at(0).at(0).toByteArray()),
                 (QJsonObject{{"type", "hello"}, {"protocol", 2}, {"app", "omainstall"}, {"version", QCoreApplication::applicationVersion()}}));
        QCOMPARE(json(sent.at(1).at(0).toByteArray()), (QJsonObject{{"type", "start"}, {"reinstall", false}}));

        Backend again;
        again.setReinstall(true);
        QSignalSpy sentAgain(&again, &Backend::sent);
        again.start();
        QTRY_VERIFY(again.hasScreen());
        QCOMPARE(json(sentAgain.at(1).at(0).toByteArray()), (QJsonObject{{"type", "start"}, {"reinstall", true}}));
    }

    void eachActionSendsExactlyItsMessage() {
        const QVariantMap answers{{"username", "alex"}, {"password", "4812"}, {"password_again", "4812"}, {"hostname", ""},
                                  {"timezone", "Europe/Paris"}, {"wifi_ssid", "a network"}, {"wifi_psk", "its password"}};
        struct Case {
            const char *screen;
            std::function<bool(Backend &)> act;
            QJsonObject message;
        };
        const QList<Case> cases{
            {"warning", [](Backend &b) { return b.acknowledge(); }, {{"type", "acknowledge"}}},
            {"warning", [](Backend &b) { return b.primaryAction(); }, {{"type", "acknowledge"}}},
            {"warning", [](Backend &b) { return b.decline(); }, {{"type", "decline"}}},
            {"phone", [](Backend &b) { return b.readPhone(); }, {{"type", "read-phone"}}},
            {"phone", [](Backend &b) { return b.primaryAction(); }, {{"type", "read-phone"}}},
            {"phone-not-found", [](Backend &b) { return b.readPhone("2"); }, {{"type", "read-phone"}, {"phone", "2"}}},
            {"unlock", [](Backend &b) { return b.primaryAction(); }, {{"type", "read-phone"}}},
            {"google-terms", [](Backend &b) { return b.acceptTerms(); }, {{"type", "accept-terms"}}},
            {"google-terms", [](Backend &b) { return b.primaryAction(); }, {{"type", "accept-terms"}}},
            {"google-terms", [](Backend &b) { return b.declineTerms(); }, {{"type", "decline-terms"}}},
            {"factory-image-download", [](Backend &b) { return b.download(); }, {{"type", "download"}, {"do", "start"}}},
            {"factory-image-download", [](Backend &b) { return b.primaryAction(); }, {{"type", "download"}, {"do", "start"}}},
            {"factory-image-download", [](Backend &b) { return b.takeFile("/somewhere/saved.zip"); },
             {{"type", "take-file"}, {"path", "/somewhere/saved.zip"}}},
            {"factory-image-take-file", [](Backend &b) { return b.takeFile("/somewhere/saved.zip"); },
             {{"type", "take-file"}, {"path", "/somewhere/saved.zip"}}},
            {"factory-image-take-file", [](Backend &b) { return b.takeFileUrl(QUrl("file:///somewhere/saved.zip")); },
             {{"type", "take-file"}, {"path", "/somewhere/saved.zip"}}},
            {"answers", [&](Backend &b) { return b.submitAnswers(answers); },
             {{"type", "answers"}, {"username", "alex"}, {"password", "4812"}, {"password_again", "4812"}, {"hostname", ""},
              {"timezone", "Europe/Paris"}, {"wifi_ssid", "a network"}, {"wifi_psk", "its password"}}},
            {"dry-run", [](Backend &b) { return b.dryRun(); }, {{"type", "dry-run"}}},
            {"dry-run", [](Backend &b) { return b.primaryAction(); }, {{"type", "dry-run"}}},
            // What was typed, as typed: the core decides what it is worth.
            {"confirm", [](Backend &b) { return b.confirmInstall("cheetah"); }, {{"type", "confirm"}, {"typed", "cheetah"}}},
            {"confirm", [](Backend &b) { return b.confirmInstall(" Cheetah "); }, {{"type", "confirm"}, {"typed", " Cheetah "}}},
            {"confirm", [](Backend &b) { return b.declineInstall(); }, {{"type", "confirm"}, {"typed", ""}}},
            {"write", [](Backend &b) { return b.write(); }, {{"type", "write"}}},
            {"write", [](Backend &b) { return b.primaryAction(); }, {{"type", "write"}}},
        };
        for (const Case &c : cases) {
            standIn(QByteArray("screen:") + c.screen);
            Backend backend;
            backend.start();
            QTRY_VERIFY2(backend.hasScreen(), c.screen);
            QSignalSpy sent(&backend, &Backend::sent);
            QVERIFY2(c.act(backend), c.screen);
            QCOMPARE(sent.count(), 1);
            const QByteArray line = sent.at(0).at(0).toByteArray();
            QVERIFY2(oneLine(line), line.constData());
            QCOMPARE(json(line), c.message);
            QVERIFY(backend.busy());
            // A second action waits for the first one's screen.
            QVERIFY(!c.act(backend));
            QCOMPARE(sent.count(), 1);
            QTRY_VERIFY(!backend.busy());
        }
    }

    void anActionTheScreenDoesNotNameIsNotSent() {
        const QVariantMap answers{{"username", "alex"}};
        const QList<std::pair<const char *, std::function<bool(Backend &)>>> all{
            {"acknowledge", [](Backend &b) { return b.acknowledge(); }},
            {"acknowledge", [](Backend &b) { return b.decline(); }},
            {"read-phone", [](Backend &b) { return b.readPhone(); }},
            {"accept-terms", [](Backend &b) { return b.acceptTerms(); }},
            {"accept-terms", [](Backend &b) { return b.declineTerms(); }},
            {"download", [](Backend &b) { return b.download(); }},
            {"download", [](Backend &b) { return b.cancelDownload(); }},
            {"answers", [&](Backend &b) { return b.submitAnswers(answers); }},
            {"dry-run", [](Backend &b) { return b.dryRun(); }},
            {"confirm", [](Backend &b) { return b.confirmInstall("cheetah"); }},
            {"confirm", [](Backend &b) { return b.declineInstall(); }},
            {"write", [](Backend &b) { return b.write(); }},
        };
        for (const char *screen : {"warning", "phone", "google-terms", "factory-image-take-file", "answers", "dry-run", "done",
                                   "confirm", "write", "installed", "stop-not-confirmed", "stop-write-interrupted-userdata",
                                   "stop-bootloader-slots-unknown"}) {
            standIn(QByteArray("screen:") + screen);
            Backend backend;
            backend.start();
            QTRY_VERIFY(backend.hasScreen());
            QSignalSpy sent(&backend, &Backend::sent);
            for (const auto &[action, act] : all) {
                // The one the screen does name is the next test's.
                if (backend.action() == action)
                    continue;
                QVERIFY2(!act(backend), qPrintable(QString("%1 on %2").arg(action, screen)));
            }
            // At the confirmation nothing typed is no go-ahead, and the main
            // action alone sends nothing: the codename is typed first.
            if (backend.action() == "confirm") {
                QVERIFY(!backend.confirmInstall(""));
                QVERIFY(!backend.primaryAction());
            }
            // A stop and the last screen take nothing at all.
            if (backend.action() == "none") {
                QVERIFY(!backend.primaryAction());
                QVERIFY(!backend.takeFile("/somewhere/saved.zip"));
            }
            QCOMPARE(sent.count(), 0);
        }
    }

    // ---- The flow, as the stand-in replays it -----------------------------

    void theWholeFlowToTheDryRun() {
        QTemporaryDir directory;
        standIn("flow");
        qputenv("OMAINSTALL_STANDIN_LOG", directory.filePath("log").toUtf8());
        FakePicker picker;
        Backend backend(&picker);
        QSignalSpy sent(&backend, &Backend::sent);
        QSignalSpy progress(&backend, &Backend::progressChanged);
        QStringList states;
        auto next = [&](const char *state) {
            if (!QTest::qWaitFor([&] { return !backend.busy() && backend.state() == state; }))
                return false;
            states << backend.state();
            return true;
        };
        backend.start();
        QVERIFY(next("warning"));
        QVERIFY(backend.acknowledge());
        QVERIFY(next("phone"));
        QVERIFY(backend.readPhone());
        QVERIFY(next("google-terms"));
        QVERIFY(backend.acceptTerms());
        QVERIFY(next("factory-image"));
        QCOMPARE(backend.action(), QString("download"));
        progress.clear();
        QVERIFY(backend.download());
        QCOMPARE(backend.pending(), QString("download"));
        // Read where it changes: on a busy machine the progress and the
        // screen that ends it can arrive in one piece.
        QString what;
        double total = 0;
        const auto watching = connect(&backend, &Backend::progressChanged, this, [&] {
            if (backend.hasProgress()) {
                what = backend.progressWhat();
                total = backend.progressTotal();
            }
        });
        QVERIFY(next("answers"));
        disconnect(watching);
        QCOMPARE(what, QString("download"));
        QCOMPARE(total, 49152.0);
        // The screen that ends an action ends its progress.
        QVERIFY(!backend.hasProgress());
        QVERIFY(progress.count() >= 2);

        // Answers the core objects to: its words, per field, and the step stays.
        QVERIFY(backend.submitAnswers({{"username", "Root"}, {"password", "4812"}, {"password_again", "4813"}}));
        QTRY_VERIFY(!backend.busy());
        QCOMPARE(backend.state(), QString("answers"));
        QCOMPARE(backend.problems().size(), 2);
        QCOMPARE(backend.problemFor("username"), QString("Use lower-case letters, digits, - and _, starting with a letter (at most 32)."));
        QCOMPARE(backend.problemFor("password"), QString("The two don't match."));
        QCOMPARE(backend.problemFor("timezone"), QString());
        QVERIFY(backend.submitAnswers({{"username", "alex"}, {"password", "4812"}, {"password_again", "4812"}, {"timezone", "Europe/Paris"}}));
        // The next try starts clean.
        QCOMPARE(backend.problems().size(), 0);
        QVERIFY(next("dry-run"));
        QVERIFY(backend.dryRun());
        QVERIFY(next("done"));
        QCOMPARE(backend.action(), QString("none"));
        QCOMPARE(backend.screen().value("plan").toList().size(), 8);
        QCOMPARE(states.join(" "), QString("warning phone google-terms factory-image answers dry-run done"));
        QCOMPARE(backend.toolError(), QString());

        backend.shutdown();
        // What the tool was sent is what the app says it sent, line for line.
        QFile log(directory.filePath("log"));
        QVERIFY(log.open(QIODevice::ReadOnly));
        QByteArray written;
        for (const auto &call : sent)
            written += call.at(0).toByteArray();
        QCOMPARE(log.readAll(), written);
        QVERIFY(written.endsWith("{\"type\":\"quit\"}\n"));
        // A window that was not started to write sends no confirm and no
        // write, whatever was clicked on the way.
        QVERIFY(!backend.writes());
        QVERIFY(!written.contains("write") && !written.contains("confirm") && !written.contains("flash"));
    }

    void aLockedPhoneGetsTheUnlockStepsAndIsLookedAtAgain() {
        standIn("flow-locked");
        Backend backend;
        backend.start();
        QTRY_COMPARE(backend.state(), QString("warning"));
        QVERIFY(backend.acknowledge());
        QTRY_COMPARE(backend.state(), QString("phone"));
        QVERIFY(backend.readPhone());
        QTRY_COMPARE(backend.state(), QString("unlock"));
        QCOMPARE(backend.action(), QString("read-phone"));
        const QVariantList steps = backend.screen().value("steps").toList();
        QCOMPARE(steps.size(), 3);
        QCOMPARE(steps.at(0).toMap().value("command").toString(), QString("fastboot flashing unlock"));
        QVERIFY(steps.at(1).toMap().value("command").isNull());
        QTRY_VERIFY(!backend.busy());
        QVERIFY(backend.readPhone());
        QTRY_COMPARE(backend.state(), QString("google-terms"));
    }

    void decliningEndsAtTheCoresStop() {
        standIn("flow");
        Backend backend;
        backend.start();
        QTRY_COMPARE(backend.state(), QString("warning"));
        QVERIFY(backend.decline());
        QTRY_COMPARE(backend.state(), QString("stopped"));
        QCOMPARE(backend.screen().value("stop").toString(), QString("warning-declined"));
        QCOMPARE(backend.action(), QString("none"));
    }

    void aDownloadCanBeCancelled() {
        standIn("flow-slow");
        Backend backend;
        backend.start();
        QTRY_COMPARE(backend.state(), QString("warning"));
        backend.acknowledge();
        QTRY_COMPARE(backend.state(), QString("phone"));
        backend.readPhone();
        QTRY_COMPARE(backend.state(), QString("google-terms"));
        // Nothing to cancel before a download runs.
        QVERIFY(!backend.cancelDownload());
        backend.acceptTerms();
        QTRY_COMPARE(backend.action(), QString("download"));
        QTRY_VERIFY(!backend.busy());
        QVERIFY(backend.download());
        QTRY_VERIFY(backend.hasProgress());
        QCOMPARE(backend.progressDone(), 20000.0);
        QSignalSpy sent(&backend, &Backend::sent);
        QVERIFY(backend.cancelDownload());
        QCOMPARE(json(sent.at(0).at(0).toByteArray()), (QJsonObject{{"type", "download"}, {"do", "cancel"}}));
        // Still busy: the core's screen says how the download ended.
        QVERIFY(backend.busy());
        QTRY_VERIFY(!backend.busy());
        QVERIFY(backend.screen().value("problem").toString().contains("stopped before it was complete"));
        QCOMPARE(backend.action(), QString("download"));
        QVERIFY(!backend.hasProgress());
    }

    void withTwoPhonesThePersonChooses() {
        standIn("flow-phones");
        Backend backend;
        backend.start();
        QTRY_COMPARE(backend.state(), QString("warning"));
        backend.acknowledge();
        QTRY_COMPARE(backend.state(), QString("phone"));
        QTRY_VERIFY(!backend.busy());
        QVERIFY(backend.readPhone());
        QTRY_VERIFY(!backend.busy());
        QCOMPARE(backend.state(), QString("phone"));
        QCOMPARE(backend.phones().size(), 2);
        QCOMPARE(backend.phonesTitle(), QString("Two phones are connected. Which one is this install for?"));
        QCOMPARE(backend.phones().at(1).toMap().value("label").toString(), QString("Phone on USB port 2"));
        QSignalSpy sent(&backend, &Backend::sent);
        QVERIFY(backend.readPhone(backend.phones().at(1).toMap().value("id").toString()));
        QCOMPARE(json(sent.at(0).at(0).toByteArray()), (QJsonObject{{"type", "read-phone"}, {"phone", "2"}}));
        QCOMPARE(backend.phones().size(), 0);
        QTRY_COMPARE(backend.state(), QString("google-terms"));
    }

    void theFilePickerIsTheOneHandedIn() {
        standIn("flow");
        FakePicker picker;
        Backend backend(&picker);
        backend.start();
        QTRY_COMPARE(backend.state(), QString("warning"));
        // No file is asked for before a step wants one.
        backend.pickFile();
        QCOMPARE(picker.opened, 0);
        backend.acknowledge();
        QTRY_COMPARE(backend.state(), QString("phone"));
        backend.readPhone();
        QTRY_COMPARE(backend.state(), QString("google-terms"));
        backend.declineTerms();
        QTRY_COMPARE(backend.action(), QString("take-file"));
        QTRY_VERIFY(!backend.busy());

        QSignalSpy sent(&backend, &Backend::sent);
        QVERIFY(backend.primaryAction());
        QCOMPARE(picker.opened, 1);
        QCOMPARE(picker.lastTitle, QString("Choose the file you saved"));
        QCOMPARE(sent.count(), 0);
        // Closed without a file: nothing is sent, nothing is said.
        emit picker.cancelled();
        QCOMPARE(sent.count(), 0);
        QCOMPARE(backend.pickerError(), QString());
        // The chooser could not be shown: said, with the system's reason.
        emit picker.failed("no portal on this bus");
        QVERIFY(backend.pickerError().contains("no portal on this bus"));
        QVERIFY(backend.pickerError().contains("drop the file"));
        QCOMPARE(sent.count(), 0);

        // Another file than the one wanted: the core's problem line, and the step stays.
        emit picker.picked("/somewhere/some-other-file.zip");
        QCOMPARE(json(sent.at(0).at(0).toByteArray()), (QJsonObject{{"type", "take-file"}, {"path", "/somewhere/some-other-file.zip"}}));
        QCOMPARE(backend.pickerError(), QString());
        QTRY_VERIFY(!backend.busy());
        QCOMPARE(backend.state(), QString("factory-image"));
        QVERIFY(!backend.screen().value("problem").toString().isEmpty());

        const QString wanted = backend.screen().value("file").toMap().value("name").toString();
        emit picker.picked("/somewhere/" + wanted);
        QTRY_COMPARE(backend.state(), QString("answers"));

        // Without a picker the app says so; it opens no dialog of its own.
        standIn("screen:factory-image-take-file");
        Backend bare;
        bare.start();
        QTRY_VERIFY(bare.hasScreen());
        bare.pickFile();
        QVERIFY(bare.pickerError().startsWith("No file chooser is available."));
    }

    void anActionTheToolRefusesIsSaidInItsWords() {
        // A file that is not there: the tool says so and the step stays open.
        standIn("flow");
        Backend backend;
        backend.start();
        QTRY_COMPARE(backend.state(), QString("warning"));
        backend.acknowledge();
        QTRY_COMPARE(backend.state(), QString("phone"));
        backend.readPhone();
        QTRY_COMPARE(backend.state(), QString("google-terms"));
        backend.declineTerms();
        QTRY_COMPARE(backend.action(), QString("take-file"));
        QTRY_VERIFY(!backend.busy());
        QVERIFY(backend.takeFile("/somewhere/nowhere.zip"));
        QTRY_VERIFY(!backend.busy());
        QCOMPARE(backend.actionError(), QString("There is no file /somewhere/nowhere.zip"));
        QCOMPARE(backend.action(), QString("take-file"));
        QCOMPARE(backend.toolError(), QString());
        // It belongs to that action: the next one starts without it.
        QVERIFY(backend.takeFile("/somewhere/some-other-file.zip"));
        QCOMPARE(backend.actionError(), QString());
    }

    // ---- When the tool itself fails ---------------------------------------

    void aToolThatFailsIsSaidAndNothingIsGuessed_data() {
        QTest::addColumn<QByteArray>("mode");
        QTest::addColumn<QString>("kind");
        QTest::addColumn<QString>("says");
        QTest::addColumn<QString>("detail");
        QTest::newRow("malformed JSON") << QByteArray("malformed") << "malformed" << "sent something this app cannot read" << "this is not JSON";
        QTest::newRow("a screen that lacks a field") << QByteArray("odd-screen") << "malformed" << "sent something this app cannot read" << "the screen has no \"footer\"";
        QTest::newRow("an unknown message") << QByteArray("unknown") << "malformed" << "sent something this app cannot read" << "a kind this app does not know";
        QTest::newRow("an unexpected exit") << QByteArray("exit") << "crashed" << "stopped unexpectedly (exit status 3)" << "stand-in: leaving, as the test asked";
        QTest::newRow("a protocol mismatch") << QByteArray("mismatch") << "mismatch" << "speaks protocol 99 and this app speaks protocol 2" << "";
        QTest::newRow("another kind of run") << QByteArray("other-mode") << "mode" << "started a run that can write" << "";
        QTest::newRow("a fatal message") << QByteArray("fatal") << "fatal" << "reported that it failed" << "the release could not be read";
        QTest::newRow("no hello") << QByteArray("silent") << "silent" << "did not say hello" << "";
    }
    void aToolThatFailsIsSaidAndNothingIsGuessed() {
        QFETCH(QByteArray, mode);
        QFETCH(QString, kind);
        QFETCH(QString, says);
        QFETCH(QString, detail);
        standIn(mode);
        Backend backend;
        backend.setHelloTimeout(400);
        QSignalSpy sent(&backend, &Backend::sent);
        backend.start();
        QTRY_VERIFY(!backend.toolErrorKind().isEmpty());
        QCOMPARE(backend.toolErrorKind(), kind);
        QVERIFY2(backend.toolError().contains(says), qPrintable(backend.toolError()));
        QVERIFY2(backend.toolErrorDetail().contains(detail), qPrintable(backend.toolErrorDetail()));
        QVERIFY(!backend.busy());
        QVERIFY(!backend.hasScreen());
        // Nothing more is handed to a tool that failed.
        const int before = sent.count();
        QVERIFY(!backend.acknowledge());
        QVERIFY(!backend.primaryAction());
        QCOMPARE(sent.count(), before);
        if (kind == "mismatch" || kind == "silent" || kind == "mode")
            QCOMPARE(before, 0);

        // "Try again" starts the tool afresh.
        standIn("screen:warning");
        backend.restart();
        QTRY_VERIFY(backend.hasScreen());
        QCOMPARE(backend.toolError(), QString());
        QCOMPARE(backend.toolErrorKind(), QString());
    }

    void aToolThatCrashesAfterAScreenIsSaidToo() {
        // The screen it showed may no longer be true, and the app says that.
        QTemporaryDir directory;
        const QString tool = directory.filePath("tool");
        QFile script(tool);
        QVERIFY(script.open(QIODevice::WriteOnly));
        script.write("#!/bin/sh\n"
                     "echo '{\"type\":\"hello\",\"protocol\":2,\"tool\":\"t\",\"version\":\"0\",\"writes\":false}'\n"
                     "read hello; read start\n"
                     "printf '{\"type\":\"screen\",\"screen\":%s}\\n' \"$(tr -d '\\n' < '" + Tests.toUtf8() + "/fixtures/screens/phone.json')\"\n"
                     "read action\n"
                     "kill -SEGV $$\n");
        script.close();
        QVERIFY(QFile::setPermissions(tool, QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner));
        qputenv("OMAINSTALL_TOOL", tool.toUtf8());
        Backend backend;
        backend.start();
        QTRY_COMPARE(backend.state(), QString("phone"));
        QVERIFY(backend.readPhone());
        QTRY_COMPARE(backend.toolErrorKind(), QString("crashed"));
        QVERIFY(backend.toolError().contains("crashed"));
        QVERIFY(backend.toolError().contains("may no longer be true"));
        QVERIFY(!backend.busy());
    }

    void aMissingToolIsSaid() {
        qputenv("OMAINSTALL_TOOL", "/nonexistent/omamobile-install");
        Backend backend;
        backend.start();
        // Known at once: nothing was started.
        QCOMPARE(backend.toolErrorKind(), QString("missing"));
        QVERIFY(backend.toolError().contains("omamobile-install"));
        QVERIFY(backend.toolError().contains("was not found"));
        QVERIFY(backend.toolErrorDetail().contains("/nonexistent/omamobile-install"));
        QVERIFY(!backend.busy());
        QVERIFY(!backend.hasScreen());
        QVERIFY(!backend.acknowledge());

        // A file that is there and is not a program does not start either.
        QTemporaryDir directory;
        QFile plain(directory.filePath("omamobile-install"));
        QVERIFY(plain.open(QIODevice::WriteOnly));
        plain.close();
        qputenv("OMAINSTALL_TOOL", plain.fileName().toUtf8());
        backend.restart();
        QCOMPARE(backend.toolErrorKind(), QString("missing"));
    }

    void theToolIsFoundByVariableThenBesideTheAppThenOnThePath() {
        QTemporaryDir beside, onPath;
        auto program = [](const QString &path) {
            QFile file(path);
            return file.open(QIODevice::WriteOnly) && file.write("#!/bin/sh\n") > 0
                   && file.setPermissions(QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner);
        };
        const QByteArray path = qgetenv("PATH");
        qputenv("PATH", onPath.path().toUtf8());
        qunsetenv("OMAINSTALL_TOOL");
        // These only look for the tool. Nothing here starts what is found.
        QCOMPARE(Backend::findTool(beside.path()), QString());
        QVERIFY(program(onPath.filePath("omamobile-install")));
        QCOMPARE(Backend::findTool(beside.path()), onPath.filePath("omamobile-install"));
        QVERIFY(program(beside.filePath("omamobile-install")));
        QCOMPARE(Backend::findTool(beside.path()), beside.filePath("omamobile-install"));
        qputenv("OMAINSTALL_TOOL", StandIn.toUtf8());
        QCOMPARE(Backend::findTool(beside.path()), StandIn);
        // A variable that names nothing finds nothing, rather than another tool.
        qputenv("OMAINSTALL_TOOL", "/nonexistent/tool");
        QCOMPARE(Backend::findTool(beside.path()), QString());
        qputenv("PATH", path);
    }

    void aScreenMustHaveEveryFieldOfTheCores() {
        const QJsonObject good = fixture("google-terms");
        QCOMPARE(Backend::checkScreen(good), QString());
        for (const QString &key : good.keys()) {
            QJsonObject without = good;
            without.remove(key);
            QVERIFY2(!Backend::checkScreen(without).isEmpty(), qPrintable(key));
        }
        auto with = [&](const QString &key, const QJsonValue &value) {
            QJsonObject changed = good;
            changed.insert(key, value);
            return Backend::checkScreen(changed);
        };
        QVERIFY(!with("title", 7).isEmpty());
        QVERIFY(!with("body", "one paragraph").isEmpty());
        QVERIFY(!with("body", QJsonArray{1}).isEmpty());
        QVERIFY(!with("notice", QJsonArray{}).isEmpty());
        QVERIFY(!with("retry", true).isEmpty());
        // A word for `retry` that this version does not know is not refused
        // (and offers nothing).
        QCOMPARE(with("retry", "after-a-while"), QString());
        QVERIFY(!with("steps", QJsonArray{QJsonObject{{"text", "no command key"}}}).isEmpty());
        QVERIFY(!with("quote", QJsonObject{{"intro", "x"}}).isEmpty());
        QVERIFY(!with("file", QJsonObject{{"name", "x"}, {"size", "big"}, {"build", "b"}}).isEmpty());
        // A field this version does not know is carried along, not refused.
        QCOMPARE(with("something_new", true), QString());
    }

    void onlyWebPagesAreOpened() {
        Backend backend;
        QVERIFY(backend.isWebLink("https://developers.google.com/android/images#cheetah"));
        QVERIFY(!backend.isWebLink("http://example.org/"));
        QVERIFY(!backend.isWebLink("file:///etc/passwd"));
        QVERIFY(!backend.isWebLink("javascript:alert(1)"));
        QVERIFY(!backend.isWebLink(""));
    }

    // ---- The window --------------------------------------------------------

    void theWindowDrawsEveryScreenWithoutAWarning_data() {
        everyScreenReachesQmlAsItWasSent_data();
    }
    void theWindowDrawsEveryScreenWithoutAWarning() {
        QFETCH(QString, name);
        standIn("screen:" + name.toUtf8());
        FakePicker picker;
        Backend backend(&picker);
        Window w(&backend);
        QVERIFY2(w.window, qPrintable(w.warnings.join("\n")));
        QVERIFY(w.window->minimumWidth() <= 720 && w.window->minimumHeight() <= 560);
        backend.start();
        QTRY_VERIFY(backend.hasScreen());
        w.settle();

        // Everything the core sent is on the window, as sent.
        const QJsonObject screen = fixture(name);
        auto strings = [](const QJsonValue &value) {
            QStringList out;
            for (const auto &item : value.toArray())
                out << item.toString();
            return out;
        };
        QCOMPARE(w.text("title"), screen.value("title").toString());
        QCOMPARE(w.texts("body"), strings(screen.value("body")));
        QCOMPARE(w.texts("footerLine"), strings(screen.value("footer")));
        QCOMPARE(w.texts("planLine"), strings(screen.value("plan")));
        QCOMPARE(w.shown("notice"), screen.value("notice").isString());
        QCOMPARE(w.shown("quote"), screen.value("quote").isObject());
        QCOMPARE(w.shown("problem"), screen.value("problem").isString());
        if (screen.value("problem").isString())
            QCOMPARE(w.text("problem"), screen.value("problem").toString());
        QCOMPARE(w.shown("file"), screen.value("file").isObject());
        if (screen.value("file").isObject())
            QCOMPARE(w.text("fileName"), screen.value("file").toObject().value("name").toString());
        QCOMPARE(w.shown("linkButton"), screen.value("link").isString());
        QStringList commands, quoted;
        for (const auto &step : screen.value("steps").toArray())
            if (step.toObject().value("command").isString())
                commands << step.toObject().value("command").toString();
        QCOMPARE(w.texts("command"), commands);
        QCOMPARE(w.items("step").size(), screen.value("steps").toArray().size());
        for (const auto &line : screen.value("quote").toObject().value("lines").toArray())
            quoted << "“" + line.toString() + "”";
        QCOMPARE(w.texts("quoteLine"), quoted);
        QCOMPARE(w.items("quoteLink").size(), screen.value("quote").toObject().value("links").toArray().size());

        // One main action, labelled from the core's `action`.
        const QString action = screen.value("action").toString();
        QCOMPARE(w.text("primaryButton"), primaryLabel(action));
        QVERIFY(w.shown("primaryButton"));
        // "Start again" beside it exactly where the core says a new run can
        // get past the stop (Screen.retry), and on no other screen.
        const QString retry = screen.value("retry").toString();
        const bool again = action == "none" && (retry == "whenever" || retry == "after-fix");
        QCOMPARE(backend.canStartAgain(), again);
        QCOMPARE(w.shown("secondaryButton"),
                 action == "acknowledge" || action == "accept-terms" || action == "download" || action == "confirm" || again);
        QCOMPARE(w.shown("answersLoader"), action == "answers");
        // The codename is asked for where the core asks for it, and there
        // the main action waits for it.
        QCOMPARE(w.shown("codename"), action == "confirm");
        QCOMPARE(w.find("primaryButton")->isEnabled(), action != "confirm");
        QVERIFY(!w.shown("sent"));
        QVERIFY(!w.shown("toolError"));
        QVERIFY(!w.shown("unknownAction"));
        QVERIFY(w.text("banner").contains("cannot write to a phone"));
        QCOMPARE(w.text("mode"), QString("Dry run"));
        // A stop and the last screen: the core's text and Close. A stop
        // that starting again can't pass, and the last screen, have no other.
        if (action == "none") {
            QCOMPARE(w.text("primaryButton"), QString("Close"));
            if (again)
                QCOMPARE(w.text("secondaryButton"), QString("Start again"));
            else
                QVERIFY(!w.shown("secondaryButton"));
            QCOMPARE(screen.value("state").toString() == "stopped", !screen.value("retry").isNull());
        }

        const QString shots = qEnvironmentVariable("OMAINSTALL_SHOTS");
        if (!shots.isEmpty()) {
            QVERIFY(QDir().mkpath(shots));
            QVERIFY(w.window->grabWindow().save(shots + "/" + name + ".png"));
        }
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
    }

    void theWindowFitsItsSmallestSize() {
        // The longest labels, at the smallest size the window allows.
        standIn("screen:google-terms-notice");
        Backend backend;
        Window w(&backend);
        QVERIFY(w.window);
        w.window->resize(w.window->minimumSize());
        backend.start();
        QTRY_VERIFY(backend.hasScreen());
        w.settle();
        QCOMPARE(w.window->size(), QSize(720, 560));
        const QQuickItem *primary = w.find("primaryButton"), *secondary = w.find("secondaryButton");
        QVERIFY(primary && secondary);
        const QRectF first = secondary->mapRectToScene(secondary->boundingRect());
        const QRectF second = primary->mapRectToScene(primary->boundingRect());
        QVERIFY(first.left() >= 0 && second.right() <= 720 && second.bottom() <= 560);
        QVERIFY2(first.right() < second.left(), "the two buttons overlap");
        // The text keeps to the window's width: it wraps, and scrolls down.
        for (const QQuickItem *paragraph : w.items("body"))
            QVERIFY(paragraph->mapRectToScene(paragraph->boundingRect()).right() <= 720);
        const QString shots = qEnvironmentVariable("OMAINSTALL_SHOTS");
        if (!shots.isEmpty())
            QVERIFY(w.window->grabWindow().save(shots + "/smallest.png"));
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
    }

    void theConfirmationFitsTheSmallestSize() {
        // The two buttons and the field between them, at the smallest size.
        standIn("screen:confirm");
        Backend backend;
        Window w(&backend);
        QVERIFY(w.window);
        w.window->resize(w.window->minimumSize());
        backend.start();
        QTRY_VERIFY(backend.hasScreen());
        w.settle();
        // Drawn once, so that the row is laid out with the field in it.
        QVERIFY(!w.window->grabWindow().isNull());
        const QString shots = qEnvironmentVariable("OMAINSTALL_SHOTS");
        if (!shots.isEmpty())
            QVERIFY(w.window->grabWindow().save(shots + "/smallest-confirm.png"));
        const QQuickItem *primary = w.find("primaryButton"), *secondary = w.find("secondaryButton"), *field = w.find("codename");
        QVERIFY(primary && secondary && field && field->isVisible());
        const QRectF left = secondary->mapRectToScene(secondary->boundingRect());
        // A text field's own bounding rectangle is its text's: its size is asked for.
        const QRectF middle = field->mapRectToScene(QRectF(0, 0, field->width(), field->height()));
        const QRectF right = primary->mapRectToScene(primary->boundingRect());
        QVERIFY(left.left() >= 0 && right.right() <= 720 && right.bottom() <= 560 && middle.bottom() <= 560);
        QVERIFY2(left.right() < middle.left() && middle.right() < right.left(), "the buttons and the field overlap");
        // Wide enough for a codename and for its own label.
        QVERIFY(middle.width() >= 200);
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
    }

    void eachButtonSendsExactlyItsMessage() {
        struct Case {
            const char *screen;
            const char *button;
            QJsonObject message;
        };
        const QList<Case> cases{
            {"warning", "primaryButton", {{"type", "acknowledge"}}},
            {"warning", "secondaryButton", {{"type", "decline"}}},
            {"phone", "primaryButton", {{"type", "read-phone"}}},
            {"unlock", "primaryButton", {{"type", "read-phone"}}},
            {"google-terms", "primaryButton", {{"type", "accept-terms"}}},
            {"google-terms", "secondaryButton", {{"type", "decline-terms"}}},
            {"factory-image-download", "primaryButton", {{"type", "download"}, {"do", "start"}}},
            {"dry-run", "primaryButton", {{"type", "dry-run"}}},
            // Not going ahead hands in nothing typed; the core stops for it.
            {"confirm", "secondaryButton", {{"type", "confirm"}, {"typed", ""}}},
            {"write", "primaryButton", {{"type", "write"}}},
        };
        for (const Case &c : cases) {
            standIn(QByteArray("screen:") + c.screen);
            FakePicker picker;
            Backend backend(&picker);
            Window w(&backend);
            QVERIFY(w.window);
            backend.start();
            QTRY_VERIFY(backend.hasScreen());
            w.settle();
            QSignalSpy sent(&backend, &Backend::sent);
            QVERIFY2(w.click(c.button), c.screen);
            QCOMPARE(sent.count(), 1);
            QCOMPARE(json(sent.at(0).at(0).toByteArray()), c.message);
            QCOMPARE(picker.opened, 0);
            QTRY_VERIFY(!backend.busy());
            QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
        }
        QCOMPARE(m_linksOpened, 0);
    }

    void theFileButtonsOpenThePickerAndSendTheFilePicked() {
        for (const auto &[screen, button] : {std::pair{"factory-image-take-file", "primaryButton"},
                                             std::pair{"factory-image-download", "secondaryButton"}}) {
            standIn(QByteArray("screen:") + screen);
            FakePicker picker;
            Backend backend(&picker);
            Window w(&backend);
            backend.start();
            QTRY_VERIFY(backend.hasScreen());
            w.settle();
            QSignalSpy sent(&backend, &Backend::sent);
            QVERIFY(w.click(button));
            QCOMPARE(picker.opened, 1);
            QCOMPARE(sent.count(), 0);
            emit picker.failed("the portal is not running");
            w.settle();
            QVERIFY(w.shown("actionError"));
            QVERIFY(w.text("actionError").contains("the portal is not running"));
            emit picker.picked("/somewhere/saved.zip");
            QCOMPARE(json(sent.at(0).at(0).toByteArray()), (QJsonObject{{"type", "take-file"}, {"path", "/somewhere/saved.zip"}}));
            QTRY_VERIFY(!backend.busy());
            w.settle();
            QVERIFY(!w.shown("actionError"));
            QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
        }
    }

    void theAnswersFormSendsWhatWasTypedAndShowsTheCoresObjections() {
        standIn("flow");
        Backend backend;
        Window w(&backend);
        backend.start();
        QTRY_COMPARE(backend.state(), QString("warning"));
        backend.acknowledge();
        QTRY_COMPARE(backend.state(), QString("phone"));
        backend.readPhone();
        QTRY_COMPARE(backend.state(), QString("google-terms"));
        backend.acceptTerms();
        QTRY_COMPARE(backend.action(), QString("download"));
        QTRY_VERIFY(!backend.busy());
        backend.download();
        QTRY_COMPARE(backend.state(), QString("answers"));
        QTRY_VERIFY(w.find("answer_username"));
        w.settle();

        auto type = [&](const char *field, const QString &text) {
            QQuickItem *input = w.find(QString("answer_") + field);
            return input && input->setProperty("text", text);
        };
        // The time zone starts as this computer's; the core checks it like the rest.
        QCOMPARE(w.text("answer_timezone"), backend.systemTimeZone());
        QVERIFY(type("username", "Root"));
        QVERIFY(type("password", "4812"));
        QVERIFY(type("password_again", "4813"));
        QVERIFY(type("timezone", "Europe/Paris"));
        QSignalSpy sent(&backend, &Backend::sent);
        QVERIFY(w.click("primaryButton"));
        QCOMPARE(json(sent.at(0).at(0).toByteArray()),
                 (QJsonObject{{"type", "answers"}, {"username", "Root"}, {"password", "4812"}, {"password_again", "4813"},
                              {"hostname", ""}, {"timezone", "Europe/Paris"}, {"wifi_ssid", ""}, {"wifi_psk", ""}}));
        QTRY_VERIFY(!backend.busy());
        w.settle();
        // The core's words under the fields they are about, and what was typed is kept.
        QCOMPARE(backend.state(), QString("answers"));
        QVERIFY(w.shown("problem_username"));
        QCOMPARE(w.text("problem_username"), QString("Use lower-case letters, digits, - and _, starting with a letter (at most 32)."));
        QCOMPARE(w.text("problem_password"), QString("The two don't match."));
        QVERIFY(!w.shown("problem_timezone"));
        QCOMPARE(w.text("answer_username"), QString("Root"));

        QVERIFY(type("username", "alex"));
        QVERIFY(type("password_again", "4812"));
        QVERIFY(w.click("primaryButton"));
        QTRY_COMPARE(backend.state(), QString("dry-run"));
        w.settle();
        // The form goes with its step, and what was typed with it.
        QVERIFY(!w.find("answer_password"));
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
    }

    void aToolFailureIsShownInTheWindow() {
        standIn("exit");
        Backend backend;
        Window w(&backend);
        backend.start();
        QTRY_VERIFY(!backend.toolError().isEmpty());
        w.settle();
        QVERIFY(w.shown("toolError"));
        QVERIFY(!w.shown("screenView"));
        QVERIFY(w.text("toolErrorLine").contains("stopped unexpectedly"));
        QVERIFY(w.text("toolErrorDetail").contains("stand-in: leaving"));
        QCOMPARE(w.text("primaryButton"), QString("Try again"));
        QCOMPARE(w.text("secondaryButton"), QString("Close"));
        const QString shots = qEnvironmentVariable("OMAINSTALL_SHOTS");
        if (!shots.isEmpty())
            QVERIFY(w.window->grabWindow().save(shots + "/tool-error.png"));
        // "Try again" starts the tool afresh.
        standIn("screen:warning");
        QVERIFY(w.click("primaryButton"));
        QTRY_VERIFY(backend.hasScreen());
        w.settle();
        QVERIFY(!w.shown("toolError"));
        QVERIFY(w.shown("screenView"));
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
    }

    void aDownloadShowsItsBytesAndACancel() {
        standIn("flow-slow");
        Backend backend;
        Window w(&backend);
        backend.start();
        QTRY_COMPARE(backend.state(), QString("warning"));
        backend.acknowledge();
        QTRY_COMPARE(backend.state(), QString("phone"));
        backend.readPhone();
        QTRY_COMPARE(backend.state(), QString("google-terms"));
        backend.acceptTerms();
        QTRY_COMPARE(backend.action(), QString("download"));
        QTRY_VERIFY(!backend.busy());
        w.settle();
        QVERIFY(!w.shown("progress"));
        QCOMPARE(w.text("secondaryButton"), QString("Choose a file I saved…"));
        QVERIFY(w.click("primaryButton"));
        QTRY_VERIFY(backend.hasProgress());
        w.settle();
        QVERIFY(w.shown("progress"));
        QCOMPARE(w.text("progressBytes"), QString("%1 of %2").arg(backend.formatBytes(20000), backend.formatBytes(49152)));
        QCOMPARE(w.text("secondaryButton"), QString("Cancel"));
        // Nothing else can be started while it runs.
        QVERIFY(!w.click("primaryButton"));

        // Closing now asks first.
        QVERIFY(!w.shown("quitDialog"));
        w.window->close();
        w.settle();
        QVERIFY(w.window->isVisible());
        QVERIFY(w.shown("quitDialog"));
        QVERIFY(w.click("keepGoing"));
        w.settle();
        QVERIFY(!w.shown("quitDialog"));

        QSignalSpy sent(&backend, &Backend::sent);
        QVERIFY(w.click("secondaryButton"));
        QCOMPARE(json(sent.at(0).at(0).toByteArray()), (QJsonObject{{"type", "download"}, {"do", "cancel"}}));
        QTRY_VERIFY(!backend.busy());
        w.settle();
        QVERIFY(!w.shown("progress"));
        QVERIFY(w.text("problem").contains("stopped before it was complete"));
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
    }

    void twoPhonesAreChosenInTheWindow() {
        standIn("flow-phones");
        Backend backend;
        Window w(&backend);
        backend.start();
        QTRY_COMPARE(backend.state(), QString("warning"));
        backend.acknowledge();
        QTRY_COMPARE(backend.state(), QString("phone"));
        QTRY_VERIFY(!backend.busy());
        w.settle();
        QVERIFY(!w.shown("phoneChooser"));
        QVERIFY(w.click("primaryButton"));
        QTRY_VERIFY(!backend.busy());
        w.settle();
        QVERIFY(w.shown("phoneChooser"));
        QCOMPARE(w.text("phonesTitle"), QString("Two phones are connected. Which one is this install for?"));
        const auto buttons = w.items("phoneButton");
        QCOMPARE(buttons.size(), 2);
        QCOMPARE(buttons.at(0)->property("text").toString(), QString("Phone on USB port 1"));
        QSignalSpy sent(&backend, &Backend::sent);
        QVERIFY(QMetaObject::invokeMethod(buttons.at(0), "clicked"));
        QCOMPARE(json(sent.at(0).at(0).toByteArray()), (QJsonObject{{"type", "read-phone"}, {"phone", "1"}}));
        QTRY_COMPARE(backend.state(), QString("google-terms"));
        w.settle();
        QVERIFY(!w.shown("phoneChooser"));
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
    }

    void theKeysWork() {
        standIn("flow");
        Backend backend;
        Window w(&backend);
        backend.start();
        QTRY_COMPARE(backend.state(), QString("warning"));
        w.settle();
        if (!QTest::qWaitForWindowActive(w.window, 2000))
            QSKIP("this platform gives the window no focus, so no key reaches it");
        QVERIFY(!w.shown("help"));
        QTest::keyClick(w.window, Qt::Key_Question);
        QTRY_VERIFY(w.shown("help"));
        // The main action waits while the shortcuts are up.
        QSignalSpy sent(&backend, &Backend::sent);
        QTest::keyClick(w.window, Qt::Key_Space);
        w.settle();
        QCOMPARE(sent.count(), 0);
        QTest::keyClick(w.window, Qt::Key_Escape);
        QTRY_VERIFY(!w.shown("help"));
        QTest::keyClick(w.window, Qt::Key_Space);
        QTRY_COMPARE(sent.count(), 1);
        QCOMPARE(json(sent.at(0).at(0).toByteArray()), (QJsonObject{{"type", "acknowledge"}}));
        QTRY_COMPARE(backend.state(), QString("phone"));
        QTRY_VERIFY(!backend.busy());
        QTest::keyClick(w.window, Qt::Key_Return);
        QTRY_COMPARE(backend.state(), QString("google-terms"));
        // Q closes when nothing is running.
        QTRY_VERIFY(!backend.busy());
        QTest::keyClick(w.window, Qt::Key_Q);
        QTRY_VERIFY(!w.window->isVisible());
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
    }

    void startAgainIsOfferedWhereTheCoreSaysItCanHelp() {
        // The stops, by what the core says of each (Screen.retry).
        int offered = 0, withheld = 0;
        for (const QString &name : fixtureNames()) {
            const QJsonObject screen = fixture(name);
            if (screen.value("state").toString() != "stopped")
                continue;
            const QString retry = screen.value("retry").toString();
            QVERIFY2(retry == "whenever" || retry == "after-fix" || retry == "stops-again", qPrintable(name));
            (retry == "stops-again" ? withheld : offered)++;
        }
        QVERIFY(offered >= 5 && withheld >= 5);

        // Declined: the core says a new run can start at any time. The
        // button starts one: a new run of the tool, and so a new flow.
        standIn("flow");
        Backend backend;
        Window w(&backend);
        QVERIFY(w.window);
        backend.start();
        QTRY_COMPARE(backend.state(), QString("warning"));
        QVERIFY(!backend.canStartAgain());
        QVERIFY(w.click("secondaryButton"));
        QTRY_COMPARE(backend.state(), QString("stopped"));
        QTRY_VERIFY(!backend.busy());
        w.settle();
        QVERIFY(backend.canStartAgain());
        QCOMPARE(w.text("secondaryButton"), QString("Start again"));
        QCOMPARE(w.text("primaryButton"), QString("Close"));
        QSignalSpy sent(&backend, &Backend::sent);
        QVERIFY(w.click("secondaryButton"));
        QTRY_COMPARE(backend.state(), QString("warning"));
        QTRY_VERIFY(!backend.busy());
        // The old run was told to leave, and the new one greeted and started.
        QCOMPARE(sent.count(), 3);
        QCOMPARE(json(sent.at(0).at(0).toByteArray()).value("type").toString(), QString("quit"));
        QCOMPARE(json(sent.at(1).at(0).toByteArray()).value("type").toString(), QString("hello"));
        QCOMPARE(json(sent.at(2).at(0).toByteArray()), (QJsonObject{{"type", "start"}, {"reinstall", false}}));
        w.settle();
        QCOMPARE(w.text("secondaryButton"), QString("Stop here"));

        // A stop that starting again can't pass: nothing to press but Close,
        // and Space does not restart.
        standIn("screen:stop-bootloader-slots-unknown");
        Backend stuck;
        Window v(&stuck);
        stuck.start();
        QTRY_VERIFY(stuck.hasScreen());
        v.settle();
        QVERIFY(!stuck.canStartAgain());
        QVERIFY(!v.shown("secondaryButton"));
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
        QVERIFY2(v.warnings.isEmpty(), qPrintable(v.warnings.join("\n")));
    }

    void aStepTheAppDoesNotKnowIsSaid() {
        // A screen with an action from a later core: shown, with no main
        // action, and the app says why.
        QTemporaryDir directory;
        QJsonObject screen = fixture("dry-run");
        screen.insert("action", "restore-android");
        const QString tool = directory.filePath("tool");
        QFile script(tool);
        QVERIFY(script.open(QIODevice::WriteOnly));
        script.write("#!/bin/sh\n"
                     "echo '{\"type\":\"hello\",\"protocol\":2,\"tool\":\"t\",\"version\":\"0\",\"writes\":false}'\n"
                     "read hello; read start\n"
                     "cat <<'END'\n"
                     + QJsonDocument(QJsonObject{{"type", "screen"}, {"screen", screen}}).toJson(QJsonDocument::Compact) + "\nEND\n"
                     "read quit\n");
        script.close();
        QVERIFY(QFile::setPermissions(tool, QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner));
        qputenv("OMAINSTALL_TOOL", tool.toUtf8());
        Backend backend;
        Window w(&backend);
        backend.start();
        QTRY_VERIFY(backend.hasScreen());
        w.settle();
        QSignalSpy sent(&backend, &Backend::sent);
        QVERIFY(w.shown("unknownAction"));
        QVERIFY(w.text("unknownAction").contains("(restore-android)"));
        QVERIFY(!w.shown("primaryButton"));
        QCOMPARE(w.text("secondaryButton"), QString("Close"));
        QVERIFY(!backend.primaryAction());
        QVERIFY(!backend.dryRun());
        QCOMPARE(sent.count(), 0);
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
    }

    // ---- A run that writes, as the stand-in replays it ----------------------

    void aRunWritesOnlyWhenTheWindowWasStartedForIt() {
        // The app's one switch is passed on as the tool's own argument, and
        // the tool's hello says which run it started.
        standIn("screen:warning");
        Backend dry;
        dry.start();
        QTRY_VERIFY(dry.hasScreen());
        QVERIFY(!dry.writes());

        Backend installs;
        installs.setWrite(true);
        installs.start();
        QTRY_VERIFY(installs.hasScreen());
        QVERIFY(installs.writes());
        QCOMPARE(installs.toolError(), QString());

        // A tool that started the other kind of run is stopped, and nothing
        // is sent to it: neither a dry run that could write, nor an install
        // that could not.
        for (const bool write : {false, true}) {
            standIn("other-mode");
            Backend backend;
            backend.setWrite(write);
            QSignalSpy sent(&backend, &Backend::sent);
            backend.start();
            QTRY_COMPARE(backend.toolErrorKind(), QString("mode"));
            QVERIFY2(backend.toolError().contains(write ? "cannot write" : "can write"), qPrintable(backend.toolError()));
            QCOMPARE(sent.count(), 0);
            QVERIFY(!backend.hasScreen());
        }

        // `check` only reads the hello: it never starts a tool for a write.
        standIn("flow");
        Backend probe;
        probe.setWrite(true);
        QSignalSpy ready(&probe, &Backend::ready);
        probe.probe();
        QTRY_COMPARE(ready.count(), 1);
        QCOMPARE(probe.toolError(), QString());
    }

    void theWholeFlowThroughTheWrite() {
        QTemporaryDir directory;
        standIn("flow");
        qputenv("OMAINSTALL_STANDIN_LOG", directory.filePath("log").toUtf8());
        Backend backend;
        backend.setWrite(true);
        QSignalSpy sent(&backend, &Backend::sent);
        QStringList whats;
        connect(&backend, &Backend::progressChanged, this, [&] {
            if (backend.hasProgress() && !whats.contains(backend.progressWhat()))
                whats << backend.progressWhat();
        });
        backend.start();
        // The dry run of a run that writes ends at the list of what will be
        // written, not at "done".
        QVERIFY(goTo(backend, "confirm", "/somewhere/cheetah-test.260101.001-factory-31c2322a.zip"));
        QCOMPARE(backend.action(), QString("confirm"));
        QVERIFY(backend.sentLines().isEmpty());
        // Nothing typed is no go-ahead, and nothing is sent for it.
        const int before = sent.count();
        QVERIFY(!backend.confirmInstall(""));
        QVERIFY(!backend.write());
        QCOMPARE(sent.count(), before);

        QVERIFY(backend.confirmInstall("cheetah"));
        // The codename was taken: the writes follow by themselves, and the
        // app is busy from the codename to the last screen.
        QVERIFY(at(backend, "installed"));
        QCOMPARE(json(sent.at(before).at(0).toByteArray()), (QJsonObject{{"type", "confirm"}, {"typed", "cheetah"}}));
        QCOMPARE(json(sent.at(before + 1).at(0).toByteArray()), (QJsonObject{{"type", "write"}}));
        QCOMPARE(sent.count(), before + 2);
        QCOMPARE(backend.action(), QString("none"));
        QVERIFY(!backend.writing());
        QVERIFY(whats.contains("verify") && whats.contains("write"));
        // What the tool said it sent, in its order and its words.
        const QStringList lines = backend.sentLines();
        QCOMPARE(lines.first(), QString("download:00004000"));
        QCOMPARE(lines.at(1), QString("flash:boot_b"));
        QCOMPARE(lines.filter("flash:userdata (piece ").size(), 3);
        QCOMPARE(lines.filter("userdata, ").size(), 2);
        QCOMPARE(lines.mid(lines.size() - 2), (QStringList{"set_active:b", "reboot"}));
        // The last screen is the end, not a stop: nothing to start again.
        QVERIFY(!backend.canStartAgain());

        backend.shutdown();
        QFile log(directory.filePath("log"));
        QVERIFY(log.open(QIODevice::ReadOnly));
        QByteArray written;
        for (const auto &call : sent)
            written += call.at(0).toByteArray();
        QCOMPARE(log.readAll(), written);
    }

    void anythingButTheCodenameIsTheCoresStop() {
        for (const QString &typed : {QString("yes"), QString()}) {
            standIn("flow");
            Backend backend;
            backend.setWrite(true);
            backend.start();
            QVERIFY(goTo(backend, "confirm", "/somewhere/cheetah-test.260101.001-factory-31c2322a.zip"));
            QSignalSpy sent(&backend, &Backend::sent);
            // Typed and handed in as typed, or not going ahead at all.
            QVERIFY(typed.isEmpty() ? backend.declineInstall() : backend.confirmInstall(typed));
            QVERIFY(at(backend, "stopped"));
            QCOMPARE(backend.screen().value("stop").toString(), QString("not-confirmed"));
            // No write followed, and none can be asked for now.
            QCOMPARE(sent.count(), 1);
            QCOMPARE(json(sent.at(0).at(0).toByteArray()), (QJsonObject{{"type", "confirm"}, {"typed", typed}}));
            QVERIFY(!backend.write());
            QVERIFY(backend.sentLines().isEmpty());
            QVERIFY(backend.canStartAgain());
        }
    }

    void theConfirmationAndTheWriteInTheWindow() {
        standIn("flow");
        Backend backend;
        backend.setWrite(true);
        Window w(&backend);
        QVERIFY(w.window);
        backend.start();
        QVERIFY(at(backend, "warning"));
        w.settle();
        // What the window is, before anything else, and one stage more.
        QCOMPARE(w.text("mode"), QString("Install"));
        QVERIFY(w.text("banner").contains("can write to a phone"));
        QVERIFY(!w.text("banner").contains("cannot"));
        QVERIFY(goTo(backend, "confirm", "/somewhere/cheetah-test.260101.001-factory-31c2322a.zip"));
        w.settle();
        const QJsonObject confirm = fixture("confirm");
        QCOMPARE(w.text("title"), confirm.value("title").toString());
        QCOMPARE(w.texts("planLine").size(), confirm.value("plan").toArray().size());
        QCOMPARE(w.texts("footerLine").size(), confirm.value("footer").toArray().size());
        // The main action waits for the codename; the other one is always there.
        QQuickItem *field = w.find("codename");
        QVERIFY(field && field->isVisible());
        QCOMPARE(w.text("primaryButton"), QString("Erase the phone and install"));
        QCOMPARE(w.text("secondaryButton"), QString("Do not install"));
        QVERIFY(!w.find("primaryButton")->isEnabled());
        QVERIFY(!w.click("primaryButton"));
        QVERIFY(!w.shown("sent"));
        QSignalSpy sent(&backend, &Backend::sent);
        field->setProperty("text", "cheetah");
        w.settle();
        QVERIFY(w.find("primaryButton")->isEnabled());
        QCOMPARE(sent.count(), 0);
        QVERIFY(w.click("primaryButton"));
        QCOMPARE(json(sent.at(0).at(0).toByteArray()), (QJsonObject{{"type", "confirm"}, {"typed", "cheetah"}}));
        QVERIFY(at(backend, "installed"));
        w.settle();
        QCOMPARE(w.text("title"), fixture("installed").value("title").toString());
        QCOMPARE(w.text("primaryButton"), QString("Close"));
        QVERIFY(!w.shown("secondaryButton"));
        QVERIFY(!w.shown("codename"));
        // The field is emptied with its step.
        QCOMPARE(field->property("text").toString(), QString());
        // What was sent stays on show, as the tool reported it.
        QVERIFY(w.shown("sent"));
        // After the writes: the whole record, line for line.
        QCOMPARE(w.texts("sentLine"), backend.sentLines());
        QVERIFY(!w.shown("sentCount"));
        QVERIFY(backend.sentLines().contains("reboot"));
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
    }

    void theWindowStaysWhileThePhoneIsWritten() {
        QTemporaryDir directory;
        standIn("flow-long-write");
        qputenv("OMAINSTALL_STANDIN_DONE", directory.filePath("done").toUtf8());
        auto *backend = new Backend;
        backend->setWrite(true);
        {
            Window w(backend);
            QVERIFY(w.window);
            backend->start();
            QVERIFY(goTo(*backend, "confirm", "/somewhere/cheetah-test.260101.001-factory-31c2322a.zip"));
            QVERIFY(backend->confirmInstall("cheetah"));
            QTRY_VERIFY(backend->writing());
            QTRY_VERIFY(backend->sentLines().size() >= 2);
            w.settle();
            QVERIFY(w.shown("progress"));
            QVERIFY(w.shown("sent"));
            // While it writes: the newest lines, and how many there are.
            const QStringList sentSoFar = backend->sentLines();
            QCOMPARE(w.texts("sentLine"), sentSoFar.mid(qMax(0, sentSoFar.size() - 8)));
            QCOMPARE(w.shown("sentCount"), sentSoFar.size() > 8);
            // Nothing to press while it writes: no cancel, no second action.
            QVERIFY(!w.shown("secondaryButton"));
            QVERIFY(!w.shown("primaryButton") || !w.find("primaryButton")->isEnabled());
            // Closing does not ask: the window stays, and says why.
            w.window->close();
            w.settle();
            QVERIFY(w.window->isVisible());
            QVERIFY(w.shown("writeHold"));
            QVERIFY(!w.shown("quitDialog"));
            QVERIFY(w.text("writeHoldText").contains("writing to the phone"));
            QVERIFY(backend->writing());
            QVERIFY(w.click("writeHoldOk"));
            w.settle();
            QVERIFY(!w.shown("writeHold"));
            QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
        }
        // And if the app itself is ended meanwhile, the tool is not: the
        // app waits for the write to run to its end before it lets go.
        QVERIFY(backend->writing());
        QVERIFY(!QFile::exists(directory.filePath("done")));
        delete backend;
        QVERIFY2(QFile::exists(directory.filePath("done")), "the tool was ended while it was writing");
    }

    void aWriteThatFindsNoPhoneIsTriedAgainByHand() {
        standIn("flow-no-phone");
        Backend backend;
        backend.setWrite(true);
        Window w(&backend);
        backend.start();
        QVERIFY(goTo(backend, "confirm", "/somewhere/cheetah-test.260101.001-factory-31c2322a.zip"));
        QSignalSpy sent(&backend, &Backend::sent);
        QVERIFY(backend.confirmInstall("cheetah"));
        // The writes were asked for once, by the app, and found no phone:
        // the step stays, with the tool's words, and is not asked again
        // without the person.
        QVERIFY(QTest::qWaitFor([&] { return !backend.busy() && backend.actionError() != ""; }, 20000));
        QCOMPARE(backend.state(), QString("write"));
        QCOMPARE(sent.count(), 2);
        w.settle();
        QCOMPARE(w.text("actionError"), QString("No phone in Fastboot Mode answered."));
        QCOMPARE(w.text("primaryButton"), QString("Look for the phone again"));
        QVERIFY(!w.shown("secondaryButton"));
        QTest::qWait(150);
        QCOMPARE(sent.count(), 2);
        QVERIFY(w.click("primaryButton"));
        QVERIFY(at(backend, "installed"));
        QCOMPARE(sent.count(), 3);
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
    }

    void aWriteThatStopsPartwayIsTheCoresStopWithWhatWasSent() {
        standIn("flow-unplugged");
        Backend backend;
        backend.setWrite(true);
        Window w(&backend);
        backend.start();
        QVERIFY(goTo(backend, "confirm", "/somewhere/cheetah-test.260101.001-factory-31c2322a.zip"));
        QVERIFY(backend.confirmInstall("cheetah"));
        QVERIFY(at(backend, "stopped"));
        w.settle();
        const QJsonObject stop = fixture("stop-write-interrupted-userdata");
        QCOMPARE(backend.screen().value("stop").toString(), QString("write-interrupted"));
        QCOMPARE(w.text("title"), stop.value("title").toString());
        QStringList footer;
        for (const auto &line : stop.value("footer").toArray())
            footer << line.toString();
        QCOMPARE(w.texts("footerLine"), footer);
        // The record of what was sent stays under the core's account.
        QVERIFY(w.shown("sent"));
        QVERIFY(backend.sentLines().last().startsWith("userdata, sent and not confirmed: piece 2"));
        QVERIFY(!backend.sentLines().contains("set_active:b"));
        // The core says a new run can start; it begins with an empty record.
        QCOMPARE(w.text("secondaryButton"), QString("Start again"));
        standIn("screen:warning");
        QVERIFY(w.click("secondaryButton"));
        QVERIFY(at(backend, "warning"));
        QVERIFY(backend.sentLines().isEmpty());
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
    }

    // ---- The real tool -------------------------------------------------------
    //
    // The app's backend and window against `omamobile-install json` itself,
    // built with the pretend phone: the core's flow behind the protocol, not
    // recorded screens. Each test ends by reading the pretend phones' own
    // record of what they received.

#define REAL_TOOL(tool) \
    if (!RealTool::built()) \
        QSKIP("SKIPPED: no real tool (bin/test found no cargo to build omamobile-install with)."); \
    QVERIFY2((tool).ok, "the script around the real tool could not be written")

    void theRealToolSaysHelloAndIsAskedNothingMore() {
        // What `omainstall check` does: the hello, then quit.
        RealTool tool("ready");
        REAL_TOOL(tool);
        Backend backend;
        backend.setReleaseDirectory(Release);
        QSignalSpy ready(&backend, &Backend::ready);
        QSignalSpy sent(&backend, &Backend::sent);
        backend.probe();
        QTRY_COMPARE(ready.count(), 1);
        QCOMPARE(backend.toolProtocol(), Backend::Protocol);
        QVERIFY(QRegularExpression("^\\d+\\.\\d+\\.\\d+$").match(backend.toolVersion()).hasMatch());
        QCOMPARE(backend.toolError(), QString());
        QVERIFY(!backend.hasScreen());
        QCOMPARE(sent.count(), 1);
        QCOMPARE(json(sent.at(0).at(0).toByteArray()), (QJsonObject{{"type", "quit"}}));
        backend.shutdown();
        int commands = -1;
        QCOMPARE(tool.nothingButReads(&commands), QString());
        QCOMPARE(commands, 0);
    }

    void theRealToolTheStraightPathToTheDryRun() {
        Google google(Google::Whole);
        RealTool tool("ready", google.address());
        REAL_TOOL(tool);
        Backend backend;
        backend.setReleaseDirectory(Release);
        Window w(&backend);
        QVERIFY(w.window);
        QSignalSpy progress(&backend, &Backend::progressChanged);
        QStringList whats;
        connect(&backend, &Backend::progressChanged, this, [&] {
            if (backend.hasProgress() && !whats.contains(backend.progressWhat()))
                whats << backend.progressWhat();
        });
        backend.start();
        QVERIFY(at(backend, "warning"));
        QCOMPARE(backend.screen().value("title").toString(), QString("Read this once"));
        w.settle();
        QCOMPARE(w.text("title"), QString("Read this once"));
        QVERIFY(w.click("primaryButton"));
        QVERIFY(at(backend, "phone"));
        QVERIFY(w.click("primaryButton"));
        QVERIFY(at(backend, "google-terms"));
        // Google's words arrive as a quote, and nothing was fetched yet.
        QVERIFY(backend.screen().value("quote").toMap().value("lines").toList().size() >= 2);
        QCOMPARE(google.asked, 0);
        QVERIFY(w.click("primaryButton"));
        QVERIFY(at(backend, "factory-image"));
        QCOMPARE(backend.action(), QString("download"));
        QCOMPARE(backend.screen().value("file").toMap().value("name").toString(), ZipName);
        QCOMPARE(google.asked, 0);
        QVERIFY(w.click("primaryButton"));
        QCOMPARE(backend.pending(), QString("download"));
        QVERIFY(at(backend, "answers"));
        QCOMPARE(google.asked, 1);
        QVERIFY(google.paths.at(0).endsWith("/" + ZipName));
        QCOMPARE(whats, QStringList{"download"});
        QVERIFY(QFile::exists(tool.directory.filePath("work/factory/" + ZipName)));

        // The real core's objections, per field, and the step stays.
        QVERIFY(backend.submitAnswers({{"username", "Root"}, {"password", "481516"}, {"password_again", "481517"},
                                       {"timezone", "Europe/Paris"}}));
        QTRY_VERIFY(!backend.busy());
        QCOMPARE(backend.state(), QString("answers"));
        QCOMPARE(backend.problemFor("username"), QString("Use lower-case letters, digits, - and _, starting with a letter (at most 32)."));
        QCOMPARE(backend.problemFor("password"), QString("The two don't match."));
        QVERIFY(backend.submitAnswers(GoodAnswers));
        QVERIFY(at(backend, "dry-run"));
        QVERIFY(w.click("primaryButton"));
        QVERIFY(at(backend, "done"));
        QVERIFY(whats.contains("dry-run"));
        QCOMPARE(backend.action(), QString("none"));
        QCOMPARE(backend.screen().value("title").toString(), QString("The dry run passed. Nothing was written"));
        // The release's images are beside the tool's release: they were walked.
        const QVariantList plan = backend.screen().value("plan").toList();
        QVERIFY(plan.size() >= 7);
        QVERIFY(plan.contains(QString("set_active:b")));
        w.settle();
        QCOMPARE(w.texts("planLine").size(), plan.size());
        QCOMPARE(w.text("primaryButton"), QString("Close"));
        // The last screen is no stop: nothing to start again.
        QVERIFY(!backend.canStartAgain());
        QVERIFY(!w.shown("secondaryButton"));
        QCOMPARE(backend.toolError(), QString());
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));

        backend.shutdown();
        // The plan names the writes of an install; the phone received reads.
        int commands = 0;
        QCOMPARE(tool.nothingButReads(&commands), QString());
        QVERIFY(commands > 10);
    }

    void theRealToolsStops_data() {
        QTest::addColumn<QString>("phones");
        QTest::addColumn<bool>("declines");
        QTest::addColumn<QString>("stop");
        QTest::addColumn<QString>("retry");
        QTest::newRow("the warning declined") << "ready" << true << "warning-declined" << "whenever";
        QTest::newRow("another model") << "other-model" << false << "wrong-model" << "after-fix";
        QTest::newRow("slot B") << "slot-b" << false << "runs-from-slot-b" << "stops-again";
        QTest::newRow("one bootloader slot reported") << "one-slot" << false << "bootloader-slots-unknown" << "stops-again";
        QTest::newRow("the bootloader slots differ") << "slots-differ" << false << "bootloader-slots-differ" << "stops-again";
        QTest::newRow("a build that is not listed") << "unknown-build" << false << "build-not-listed" << "after-fix";
        QTest::newRow("a build not tested yet") << "untested-build" << false << "build-not-tested" << "stops-again";
        QTest::newRow("a build not tested, a newer one is") << "untested-old-build" << false << "build-not-tested" << "after-fix";
    }
    void theRealToolsStops() {
        QFETCH(QString, phones);
        QFETCH(bool, declines);
        QFETCH(QString, stop);
        QFETCH(QString, retry);
        RealTool tool(phones);
        REAL_TOOL(tool);
        Backend backend;
        backend.setReleaseDirectory(Release);
        Window w(&backend);
        QVERIFY(w.window);
        backend.start();
        QVERIFY(at(backend, "warning"));
        if (declines) {
            QVERIFY(backend.decline());
        } else {
            QVERIFY(backend.acknowledge());
            QVERIFY(at(backend, "phone"));
            QVERIFY(backend.readPhone());
        }
        QVERIFY(at(backend, "stopped"));
        const QVariantMap screen = backend.screen();
        QCOMPARE(screen.value("stop").toString(), stop);
        QCOMPARE(screen.value("retry").toString(), retry);
        QCOMPARE(backend.action(), QString("none"));
        // The core's words, drawn as sent; the stages are hidden on a stop.
        w.settle();
        QCOMPARE(w.text("title"), screen.value("title").toString());
        QCOMPARE(w.texts("body"), screen.value("body").toStringList());
        QCOMPARE(w.texts("footerLine"), screen.value("footer").toStringList());
        QVERIFY(!w.shown("stages"));
        QCOMPARE(w.text("primaryButton"), QString("Close"));
        // "Start again" exactly where the core says a new run can help, and
        // the core's closing line says the same in words.
        const bool again = retry != "stops-again";
        QCOMPARE(backend.canStartAgain(), again);
        QCOMPARE(w.shown("secondaryButton"), again);
        QCOMPARE(screen.value("footer").toStringList().last().contains("will stop here again"), !again);
        // A stop takes nothing more.
        QVERIFY(!backend.readPhone());
        QVERIFY(!backend.primaryAction());
        if (again) {
            QCOMPARE(w.text("secondaryButton"), QString("Start again"));
            QVERIFY(w.click("secondaryButton"));
            // A new run of the tool, and the flow from its start.
            QVERIFY(at(backend, "warning"));
            QVERIFY(!backend.canStartAgain());
        }
        QCOMPARE(backend.toolError(), QString());
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
        backend.shutdown();
        QCOMPARE(tool.nothingButReads(), QString());
    }

    void theRealToolStopsForADownloadThatIsNotTheListedFile() {
        Google google(Google::Wrong);
        RealTool tool("ready", google.address());
        REAL_TOOL(tool);
        Backend backend;
        backend.setReleaseDirectory(Release);
        backend.start();
        QVERIFY(at(backend, "warning"));
        QVERIFY(backend.acknowledge());
        QVERIFY(at(backend, "phone"));
        QVERIFY(backend.readPhone());
        QVERIFY(at(backend, "google-terms"));
        QVERIFY(backend.acceptTerms());
        QVERIFY(at(backend, "factory-image"));
        QVERIFY(backend.download());
        QVERIFY(at(backend, "stopped"));
        QCOMPARE(backend.screen().value("stop").toString(), QString("factory-download-mismatch"));
        // The download is fetched afresh by a new run: the core says so.
        QVERIFY(backend.canStartAgain());
        QVERIFY(QDir(tool.directory.filePath("work/factory")).isEmpty());
        backend.shutdown();
        QCOMPARE(tool.nothingButReads(), QString());
    }

    void theRealToolALockedPhoneAndAReinstall() {
        {
            RealTool tool("locked");
            REAL_TOOL(tool);
            Backend backend;
            backend.setReleaseDirectory(Release);
            backend.start();
            QVERIFY(at(backend, "warning"));
            QVERIFY(backend.acknowledge());
            QVERIFY(at(backend, "phone"));
            QVERIFY(backend.readPhone());
            QVERIFY(at(backend, "unlock"));
            QCOMPARE(backend.action(), QString("read-phone"));
            QStringList commands;
            for (const QVariant &step : backend.screen().value("steps").toList())
                if (!step.toMap().value("command").isNull())
                    commands << step.toMap().value("command").toString();
            QCOMPARE(commands, QStringList{"fastboot flashing unlock"});
            // The pretend owner unlocked it; the app only looks again.
            QVERIFY(backend.readPhone());
            QVERIFY(at(backend, "google-terms"));
            backend.shutdown();
            // The command was shown. It was never sent: reads only.
            QCOMPARE(tool.nothingButReads(), QString());
        }
        // Slot B is a stop, unless the person said up front that this is a
        // reinstall: `omainstall open --reinstall`, in `start`.
        RealTool tool("slot-b");
        REAL_TOOL(tool);
        Backend backend;
        backend.setReleaseDirectory(Release);
        backend.setReinstall(true);
        backend.start();
        QVERIFY(at(backend, "warning"));
        QVERIFY(backend.acknowledge());
        QVERIFY(at(backend, "phone"));
        QVERIFY(backend.readPhone());
        QVERIFY(at(backend, "google-terms"));
        backend.shutdown();
        QCOMPARE(tool.nothingButReads(), QString());
    }

    void theRealToolAsksWhichOfTwoPhones() {
        RealTool tool("ready,slot-b");
        REAL_TOOL(tool);
        Backend backend;
        backend.setReleaseDirectory(Release);
        Window w(&backend);
        QVERIFY(w.window);
        backend.start();
        QVERIFY(at(backend, "warning"));
        QVERIFY(backend.acknowledge());
        QVERIFY(at(backend, "phone"));
        QVERIFY(backend.readPhone());
        QTRY_VERIFY(!backend.busy());
        // The flow has not moved; the tool asks, in its words.
        QCOMPARE(backend.state(), QString("phone"));
        QCOMPARE(backend.phones().size(), 2);
        QVERIFY(backend.phonesTitle().endsWith("Which one is this install for?"));
        w.settle();
        QVERIFY(w.shown("phoneChooser"));
        QCOMPARE(w.text("phonesTitle"), backend.phonesTitle());
        QCOMPARE(w.texts("phoneButton"), (QStringList{"Pretend phone 1 (ready)", "Pretend phone 2 (slot-b)"}));
        // Neither an id nor a label is a serial number.
        for (const QVariant &phone : backend.phones())
            QVERIFY(!phone.toMap().value("id").toString().contains("SERIAL") && !phone.toMap().value("label").toString().contains("SERIAL"));
        QVERIFY(QMetaObject::invokeMethod(w.items("phoneButton").at(0), "clicked"));
        QVERIFY(at(backend, "google-terms"));
        QCOMPARE(backend.phones().size(), 0);
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
        backend.shutdown();
        QCOMPARE(tool.nothingButReads(), QString());
    }

    void theRealToolTakesAFileHandedIn() {
        RealTool tool("ready");
        REAL_TOOL(tool);
        FakePicker picker;
        Backend backend(&picker);
        backend.setReleaseDirectory(Release);
        backend.start();
        QVERIFY(at(backend, "warning"));
        QVERIFY(backend.acknowledge());
        QVERIFY(at(backend, "phone"));
        QVERIFY(backend.readPhone());
        QVERIFY(at(backend, "google-terms"));
        QVERIFY(backend.declineTerms());
        QVERIFY(at(backend, "factory-image"));
        QCOMPARE(backend.action(), QString("take-file"));
        // A file that is not there: the tool's words, and the step stays.
        emit picker.picked(Release + "/nothing-here.zip");
        QTRY_VERIFY(!backend.busy());
        QCOMPARE(backend.actionError(), "There is no file " + Release + "/nothing-here.zip");
        QCOMPARE(backend.action(), QString("take-file"));
        // Not the file: no error, the core's problem on the same step.
        emit picker.picked(Release + "/MANIFEST");
        QTRY_VERIFY(!backend.busy());
        QCOMPARE(backend.actionError(), QString());
        QVERIFY(backend.screen().value("problem").toString().startsWith("MANIFEST is not the file for this phone"));
        // The file: checked by the core, and the flow goes on.
        QStringList whats;
        connect(&backend, &Backend::progressChanged, this, [&] {
            if (backend.hasProgress() && !whats.contains(backend.progressWhat()))
                whats << backend.progressWhat();
        });
        QVERIFY(backend.takeFileUrl(QUrl::fromLocalFile(Zip)));
        QVERIFY(at(backend, "answers"));
        QCOMPARE(whats, QStringList{"check"});
        backend.shutdown();
        QCOMPARE(tool.nothingButReads(), QString());
    }

    void theRealToolCancelsADownloadAndGoesOnFromWhatArrived() {
        Google google(Google::Stalls);
        RealTool tool("ready", google.address());
        REAL_TOOL(tool);
        Backend backend;
        backend.setReleaseDirectory(Release);
        Window w(&backend);
        QVERIFY(w.window);
        backend.start();
        QVERIFY(at(backend, "warning"));
        QVERIFY(backend.acknowledge());
        QVERIFY(at(backend, "phone"));
        QVERIFY(backend.readPhone());
        QVERIFY(at(backend, "google-terms"));
        QVERIFY(backend.acceptTerms());
        QVERIFY(at(backend, "factory-image"));
        QVERIFY(backend.download());
        // 20000 bytes arrive, and then the transfer stands still.
        QTRY_VERIFY(backend.hasProgress() && backend.progressDone() == 20000.0);
        QCOMPARE(backend.progressTotal(), double(google.size()));
        w.settle();
        QCOMPARE(w.text("secondaryButton"), QString("Cancel"));
        QVERIFY(w.click("secondaryButton"));
        // The core's screen says how far it came, and the step stays open.
        QVERIFY(at(backend, "factory-image"));
        QVERIFY(backend.screen().value("problem").toString().contains("20000 of 49152 bytes had arrived"));
        QCOMPARE(backend.action(), QString("download"));
        QVERIFY(!backend.hasProgress());
        // The next try asks Google for the rest only, and ends with the file.
        google.goOn();
        QVERIFY(backend.download());
        QVERIFY(at(backend, "answers"));
        QCOMPARE(google.asked, 2);
        QFile got(tool.directory.filePath("work/factory/" + ZipName)), wanted(Zip);
        QVERIFY(got.open(QIODevice::ReadOnly) && wanted.open(QIODevice::ReadOnly));
        QCOMPARE(got.readAll(), wanted.readAll());
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
        backend.shutdown();
        QCOMPARE(tool.nothingButReads(), QString());
    }

    // ---- The real tool, in a run that writes ---------------------------------
    //
    // `omamobile-install json --write`, against the pretend phone only: the
    // tool is built with it and started with --fake-phone, so there is no
    // way from here to USB. Each test reads the pretend phone's own record.

    void theRealToolACompleteInstall() {
        Google google(Google::Whole);
        RealTool tool("ready", google.address());
        REAL_TOOL(tool);
        Backend backend;
        backend.setReleaseDirectory(Release);
        backend.setWrite(true);
        Window w(&backend);
        QVERIFY(w.window);
        QStringList whats;
        connect(&backend, &Backend::progressChanged, this, [&] {
            if (backend.hasProgress() && !whats.contains(backend.progressWhat()))
                whats << backend.progressWhat();
        });
        backend.start();
        QVERIFY(at(backend, "warning"));
        QVERIFY(backend.writes());
        w.settle();
        QCOMPARE(w.text("mode"), QString("Install"));
        QVERIFY(w.click("primaryButton"));
        QVERIFY(at(backend, "phone"));
        QVERIFY(w.click("primaryButton"));
        QVERIFY(at(backend, "google-terms"));
        // Google's terms are the owner's to accept: nothing was fetched
        // before this click, which here is the test's.
        QCOMPARE(google.asked, 0);
        QVERIFY(w.click("primaryButton"));
        QVERIFY(at(backend, "factory-image"));
        QVERIFY(w.click("primaryButton"));
        QVERIFY(at(backend, "answers"));
        QCOMPARE(google.asked, 1);
        QVERIFY(backend.submitAnswers(GoodAnswers));
        QVERIFY(at(backend, "dry-run"));
        QVERIFY(w.click("primaryButton"));
        // The dry run of a run that writes: the list, and the question.
        QVERIFY(at(backend, "confirm"));
        QCOMPARE(backend.screen().value("title").toString(), QString("This is what the installer will write"));
        const QStringList plan = backend.screen().value("plan").toStringList();
        QCOMPARE(plan.size(), 8);
        QVERIFY(plan.at(5).contains("ERASES userdata"));
        QVERIFY(backend.screen().value("footer").toStringList().filter("userdata will be ERASED").size() == 1);
        // Up to here the phone was only asked questions.
        QVERIFY(tool.notReads().isEmpty());
        w.settle();
        QCOMPARE(w.texts("planLine"), plan);
        QVERIFY(!w.find("primaryButton")->isEnabled());
        w.find("codename")->setProperty("text", "cheetah");
        w.settle();
        QVERIFY(w.click("primaryButton"));
        QVERIFY(at(backend, "installed"));
        QCOMPARE(backend.screen().value("title").toString(), QString("Omarchy Mobile is written, and the phone is restarting"));
        QVERIFY(whats.contains("verify") && whats.contains("write"));
        QVERIFY(whats.indexOf("verify") < whats.indexOf("write"));
        w.settle();
        QCOMPARE(w.text("primaryButton"), QString("Close"));
        QVERIFY(!w.shown("secondaryButton"));
        QVERIFY(w.shown("sent"));
        QCOMPARE(backend.toolError(), QString());
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
        const QStringList said = backend.sentLines();

        backend.shutdown();
        // What the pretend phone received: slot B's five boot images, the
        // pieces of userdata, the slot and the restart, and nothing else.
        int partitions = 0;
        const QStringList received = tool.notReads(&partitions);
        QCOMPARE(partitions, 6);
        QStringList flashed;
        for (const QString &command : received)
            if (!command.startsWith("download:"))
                flashed << command;
        const qsizetype pieces = flashed.count("flash:userdata");
        QVERIFY(pieces >= 1);
        QStringList expected{"flash:boot_b", "flash:init_boot_b", "flash:vendor_boot_b", "flash:vendor_kernel_boot_b", "flash:dtbo_b"};
        for (qsizetype i = 0; i < pieces; ++i)
            expected << "flash:userdata";
        expected << "set_active:b" << "reboot";
        QCOMPARE(flashed, expected);
        // And the window's record of it is the tool's: one line for each
        // command of the boot images, one for each piece, the slot, the restart.
        QCOMPARE(said.filter("flash:userdata (piece ").size(), pieces);
        QCOMPARE(said.mid(0, 10), received.mid(0, 10));
        QCOMPARE(said.mid(said.size() - 2), (QStringList{"set_active:b", "reboot"}));
    }

    void theRealToolAnInstallTheOwnerDoesNotGoAheadWith() {
        // Cancelled at the last question, by the button and by a wrong word.
        for (const QString &typed : {QString(), QString("Cheetah")}) {
            RealTool tool("ready");
            REAL_TOOL(tool);
            Backend backend;
            backend.setReleaseDirectory(Release);
            backend.setWrite(true);
            Window w(&backend);
            QVERIFY(w.window);
            backend.start();
            QVERIFY(goTo(backend, "confirm", Zip));
            w.settle();
            if (typed.isEmpty()) {
                QCOMPARE(w.text("secondaryButton"), QString("Do not install"));
                QVERIFY(w.click("secondaryButton"));
            } else {
                w.find("codename")->setProperty("text", typed);
                w.settle();
                QVERIFY(w.click("primaryButton"));
            }
            QVERIFY(at(backend, "stopped"));
            QCOMPARE(backend.screen().value("stop").toString(), QString("not-confirmed"));
            QCOMPARE(backend.screen().value("retry").toString(), QString("whenever"));
            w.settle();
            QCOMPARE(w.text("secondaryButton"), QString("Start again"));
            QVERIFY(!w.shown("sent"));
            QVERIFY(!w.shown("codename"));
            QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
            backend.shutdown();
            // Nothing was written: the phone was only asked questions.
            QCOMPARE(tool.nothingButReads(), QString());
        }
    }

    void theRealToolAPhoneUnpluggedMidWrite() {
        // The pretend phone's cable is pulled as the second piece of
        // userdata is sent.
        RealTool tool("ready", {}, "--fake-unplug flash:userdata:2");
        REAL_TOOL(tool);
        Backend backend;
        backend.setReleaseDirectory(Release);
        backend.setWrite(true);
        Window w(&backend);
        QVERIFY(w.window);
        backend.start();
        QVERIFY(goTo(backend, "confirm", Zip));
        QVERIFY(backend.confirmInstall("cheetah"));
        QVERIFY(at(backend, "stopped"));
        const QVariantMap screen = backend.screen();
        QCOMPARE(screen.value("stop").toString(), QString("write-interrupted"));
        QCOMPARE(screen.value("title").toString(), QString("The install stopped partway"));
        // The core's account of how far the writes got, drawn as sent.
        const QStringList footer = screen.value("footer").toStringList();
        QVERIFY2(footer.first().contains("while it was writing userdata"), qPrintable(footer.first()));
        w.settle();
        QCOMPARE(w.texts("footerLine"), footer);
        QCOMPARE(w.texts("body"), screen.value("body").toStringList());
        // And the tool's record: the piece that was sent and not confirmed.
        QVERIFY(w.shown("sent"));
        QVERIFY2(backend.sentLines().last().startsWith("userdata, sent and not confirmed: piece 2"), qPrintable(backend.sentLines().last()));
        QVERIFY(!backend.sentLines().contains("set_active:b") && !backend.sentLines().contains("reboot"));
        QCOMPARE(backend.toolError(), QString());
        // A new run can start at any time, the core says.
        QCOMPARE(screen.value("retry").toString(), QString("whenever"));
        QCOMPARE(w.text("secondaryButton"), QString("Start again"));
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
        backend.shutdown();
        const QStringList received = tool.notReads();
        QCOMPARE(received.count("flash:userdata"), 1);
        QVERIFY(received.contains("flash:dtbo_b"));
        QVERIFY(!received.contains("set_active:b") && !received.contains("reboot"));
    }

    void theRealToolADownloadThatFailsAndOneThatGoesOn() {
        // The connection breaks after 20000 bytes: the step stays open, and
        // the next try asks for the rest only.
        {
            Google google(Google::Breaks);
            RealTool tool("ready", google.address());
            REAL_TOOL(tool);
            Backend backend;
            backend.setReleaseDirectory(Release);
            backend.setWrite(true);
            Window w(&backend);
            backend.start();
            QVERIFY(at(backend, "warning"));
            QVERIFY(backend.acknowledge());
            QVERIFY(at(backend, "phone"));
            QVERIFY(backend.readPhone());
            QVERIFY(at(backend, "google-terms"));
            QVERIFY(backend.acceptTerms());
            QVERIFY(at(backend, "factory-image"));
            QVERIFY(backend.download());
            QVERIFY(at(backend, "factory-image"));
            QVERIFY2(backend.screen().value("problem").toString().contains("20000 of 49152 bytes had arrived"),
                     qPrintable(backend.screen().value("problem").toString()));
            QCOMPARE(backend.action(), QString("download"));
            w.settle();
            QVERIFY(w.shown("problem"));
            QCOMPARE(w.text("primaryButton"), QString("Download"));
            google.goOn();
            QVERIFY(w.click("primaryButton"));
            QVERIFY(at(backend, "answers"));
            QCOMPARE(google.froms, (QList<qsizetype>{0, 20000}));
            QFile got(tool.directory.filePath("work/factory/" + ZipName)), wanted(Zip);
            QVERIFY(got.open(QIODevice::ReadOnly) && wanted.open(QIODevice::ReadOnly));
            QCOMPARE(got.readAll(), wanted.readAll());
            QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
            backend.shutdown();
            QVERIFY(tool.notReads().isEmpty());
        }
        // The server has no such file: the download can't be made, the core
        // says why, and the step asks for the owner's own file instead.
        Google google(Google::Missing);
        RealTool tool("ready", google.address());
        REAL_TOOL(tool);
        FakePicker picker;
        Backend backend(&picker);
        backend.setReleaseDirectory(Release);
        backend.setWrite(true);
        Window w(&backend);
        backend.start();
        QVERIFY(at(backend, "warning"));
        QVERIFY(backend.acknowledge());
        QVERIFY(at(backend, "phone"));
        QVERIFY(backend.readPhone());
        QVERIFY(at(backend, "google-terms"));
        QVERIFY(backend.acceptTerms());
        QVERIFY(at(backend, "factory-image"));
        QVERIFY(backend.download());
        QVERIFY(at(backend, "factory-image"));
        QCOMPARE(backend.action(), QString("take-file"));
        w.settle();
        QCOMPARE(w.text("primaryButton"), QString("Choose the file…"));
        QCOMPARE(w.texts("body"), backend.screen().value("body").toStringList());
        // The installer downloads no more in this run: a download is refused.
        QVERIFY(!backend.download());
        QVERIFY(w.click("primaryButton"));
        QCOMPARE(picker.opened, 1);
        emit picker.picked(Zip);
        QVERIFY(at(backend, "answers"));
        QCOMPARE(google.asked, 1);
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
        backend.shutdown();
        QVERIFY(tool.notReads().isEmpty());
    }

    void theRealToolStopsForAPhoneThatTellsOneBootloaderSlotAlsoWhenItWrites() {
        // The Pixel 7 Pro's case. What passes this stop is the owner's
        // statement on the tool's command line (docs/DESIGN.md §6): the app
        // has no control for it, hands out no way round, and shows the stop.
        RealTool tool("one-slot");
        REAL_TOOL(tool);
        Backend backend;
        backend.setReleaseDirectory(Release);
        backend.setWrite(true);
        Window w(&backend);
        QVERIFY(w.window);
        QSignalSpy sent(&backend, &Backend::sent);
        backend.start();
        QVERIFY(at(backend, "warning"));
        QVERIFY(backend.acknowledge());
        QVERIFY(at(backend, "phone"));
        QVERIFY(backend.readPhone());
        QVERIFY(at(backend, "stopped"));
        const QVariantMap screen = backend.screen();
        QCOMPARE(screen.value("stop").toString(), QString("bootloader-slots-unknown"));
        QCOMPARE(screen.value("retry").toString(), QString("stops-again"));
        w.settle();
        QCOMPARE(w.text("title"), screen.value("title").toString());
        QCOMPARE(w.texts("body"), screen.value("body").toStringList());
        QCOMPARE(w.texts("footerLine"), screen.value("footer").toStringList());
        QCOMPARE(w.text("primaryButton"), QString("Close"));
        QVERIFY(!w.shown("secondaryButton"));
        QVERIFY(!w.shown("codename"));
        // Nothing the app sent, in the whole run, names a slot or a version.
        for (const auto &call : sent) {
            const QByteArray line = call.at(0).toByteArray();
            QVERIFY2(!line.contains("slot") && !line.contains("bootloader") && !line.contains("fake-1.0"), line.constData());
        }
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
        backend.shutdown();
        QCOMPARE(tool.nothingButReads(), QString());
    }

    // ---- The screens of a pretend install, as pictures ------------------------
    //
    // The window walked through a whole install against the real tool and
    // its pretend phone, with one picture for each screen it shows on the
    // way, and for the stops a person is most likely to meet. bin/screens
    // saves them under docs/screens (OMAINSTALL_SCREENS); a plain test run
    // makes them in a scratch directory, so that the walk itself is tested.
    // The accent is the fallback one, and everything typed is made up.

    void theScreensOfAPretendInstall() {
        if (!RealTool::built())
            QSKIP("SKIPPED: no real tool (bin/test found no cargo to build omamobile-install with).");
        QTemporaryDir scratch;
        // The form starts with this computer's time zone: the pictures get a
        // made-up one, as everything else in them is.
        const QByteArray zone = qgetenv("TZ");
        qputenv("TZ", "Europe/Paris");
        const auto restore = qScopeGuard([&] { zone.isNull() ? qunsetenv("TZ") : qputenv("TZ", zone); });
        const QString wanted = qEnvironmentVariable("OMAINSTALL_SCREENS");
        const QString out = wanted.isEmpty() ? scratch.path() : wanted;
        QVERIFY(QDir().mkpath(out));
        QStringList saved;
        // One picture of the window as it is; a screen longer than the
        // window gets one more for each further page of it.
        // `midway`: taken as it is this instant, while a step runs. Nothing
        // more is read from the tool first, or the step would be over.
        auto shoot = [&](const Window &w, const QString &name, bool midway = false) {
            if (midway) {
                if (!w.window->grabWindow().save(out + "/" + name + ".png"))
                    return false;
                saved << name;
                return true;
            }
            // Whatever still moves (a label that floats up, a bar) comes to rest.
            QTest::qWait(400);
            QQuickItem *view = w.find("screenView");
            if (view && view->isVisible())
                view->setProperty("contentY", 0);
            w.settle();
            if (!w.window->grabWindow().save(out + "/" + name + ".png"))
                return false;
            saved << name;
            if (!view || !view->isVisible())
                return true;
            const qreal page = view->height(), whole = view->property("contentHeight").toReal();
            for (int n = 2; (n - 2) * page * 0.85 < whole - page - 1 && n < 6; ++n) {
                view->setProperty("contentY", qMin(whole - page, (n - 1) * page * 0.85));
                w.settle();
                if (!w.window->grabWindow().save(out + "/" + name + "-" + QString::number(n) + ".png"))
                    return false;
                saved << name + "-" + QString::number(n);
            }
            view->setProperty("contentY", 0);
            return true;
        };

        // The install: a download that breaks off and goes on, answers the
        // core objects to and then takes, the dry run, the codename, the
        // writes, the end.
        {
            Google google(Google::Breaks);
            RealTool tool("ready", google.address());
            QVERIFY(tool.ok);
            Backend backend;
            backend.setReleaseDirectory(Release);
            backend.setWrite(true);
            Window w(&backend);
            QVERIFY(w.window);
            backend.start();
            QVERIFY(at(backend, "warning"));
            QVERIFY(shoot(w, "01-warning"));
            QVERIFY(w.click("primaryButton"));
            QVERIFY(at(backend, "phone"));
            QVERIFY(shoot(w, "02-phone"));
            QVERIFY(w.click("primaryButton"));
            QVERIFY(at(backend, "google-terms"));
            QVERIFY(shoot(w, "03-google-terms"));
            QVERIFY(w.click("primaryButton"));
            QVERIFY(at(backend, "factory-image"));
            QVERIFY(shoot(w, "04-factory-image"));
            // While it downloads: the picture is taken as the bytes arrive.
            bool downloading = false;
            const auto during = connect(&backend, &Backend::progressChanged, this, [&] {
                if (!downloading && backend.hasProgress() && backend.progressDone() > 0) {
                    downloading = true;
                    shoot(w, "05-factory-image-downloading", true);
                }
            });
            QVERIFY(w.click("primaryButton"));
            QVERIFY(at(backend, "factory-image"));
            disconnect(during);
            QVERIFY(downloading);
            QVERIFY(shoot(w, "06-factory-image-download-broke-off"));
            google.goOn();
            QVERIFY(w.click("primaryButton"));
            QVERIFY(at(backend, "answers"));
            QVERIFY(shoot(w, "07-answers"));
            const QList<std::pair<const char *, const char *>> typed{
                {"answer_username", "Alex"}, {"answer_password", "481516"}, {"answer_password_again", "481517"},
                {"answer_hostname", "alex-phone"}, {"answer_wifi_ssid", "a made-up network"}, {"answer_wifi_psk", "not a real password"}};
            for (const auto &[field, text] : typed)
                w.find(field)->setProperty("text", text);
            w.find("answer_timezone")->setProperty("text", "Europe/Paris");
            QVERIFY(w.click("primaryButton"));
            QTRY_VERIFY(!backend.busy());
            QCOMPARE(backend.state(), QString("answers"));
            QVERIFY(backend.problems().size() >= 2);
            QVERIFY(shoot(w, "08-answers-the-cores-objections"));
            w.find("answer_username")->setProperty("text", "alex");
            w.find("answer_password_again")->setProperty("text", "481516");
            QVERIFY(w.click("primaryButton"));
            QVERIFY(at(backend, "dry-run"));
            QVERIFY(shoot(w, "09-dry-run"));
            QVERIFY(w.click("primaryButton"));
            QVERIFY(at(backend, "confirm"));
            QVERIFY(shoot(w, "10-confirm"));
            w.find("codename")->setProperty("text", "cheetah");
            QVERIFY(shoot(w, "11-confirm-codename-typed"));
            // While it writes: as the last boot image has been sent.
            bool writing = false;
            const auto meanwhile = connect(&backend, &Backend::sentLinesChanged, this, [&] {
                if (!writing && backend.sentLines().size() == 10) {
                    writing = true;
                    shoot(w, "12-write", true);
                }
            });
            QVERIFY(w.click("primaryButton"));
            QVERIFY(at(backend, "installed"));
            disconnect(meanwhile);
            QVERIFY(writing);
            QVERIFY(shoot(w, "13-installed"));
            QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
            backend.shutdown();
            int partitions = 0;
            tool.notReads(&partitions);
            QCOMPARE(partitions, 6);
        }
        // The stops, each from its own run.
        struct Stop {
            const char *name;
            const char *phones;
            const char *extra;
            const char *typed;
            const char *stop;
        };
        const QList<Stop> stops{
            {"20-stop-one-bootloader-slot", "one-slot", "", "", "bootloader-slots-unknown"},
            {"21-stop-not-confirmed", "ready", "", "", "not-confirmed"},
            {"22-stop-write-interrupted", "ready", "--fake-unplug flash:userdata:2", "cheetah", "write-interrupted"},
            {"23-stop-runs-from-slot-b", "slot-b", "", "", "runs-from-slot-b"},
            {"24-stop-build-not-tested", "untested-build", "", "", "build-not-tested"},
        };
        for (const Stop &stop : stops) {
            RealTool tool(stop.phones, {}, stop.extra);
            QVERIFY(tool.ok);
            Backend backend;
            backend.setReleaseDirectory(Release);
            backend.setWrite(true);
            Window w(&backend);
            backend.start();
            if (goTo(backend, "confirm", Zip))
                QVERIFY(*stop.typed ? backend.confirmInstall(stop.typed) : backend.declineInstall());
            QVERIFY2(at(backend, "stopped"), stop.name);
            QCOMPARE(backend.screen().value("stop").toString(), QString(stop.stop));
            QVERIFY(shoot(w, stop.name));
            QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
        }
        // A locked phone, in a run that installs; a download that can't be
        // made; two phones; and the shortcuts.
        {
            RealTool tool("locked");
            Backend backend;
            backend.setReleaseDirectory(Release);
            backend.setWrite(true);
            Window w(&backend);
            backend.start();
            QVERIFY(goTo(backend, "unlock", Zip));
            QVERIFY(shoot(w, "30-unlock"));
        }
        {
            Google google(Google::Missing);
            RealTool tool("ready", google.address());
            Backend backend;
            backend.setReleaseDirectory(Release);
            backend.setWrite(true);
            Window w(&backend);
            backend.start();
            QVERIFY(at(backend, "warning"));
            QVERIFY(backend.acknowledge());
            QVERIFY(at(backend, "phone"));
            QVERIFY(backend.readPhone());
            QVERIFY(at(backend, "google-terms"));
            QVERIFY(backend.acceptTerms());
            QVERIFY(at(backend, "factory-image"));
            QVERIFY(backend.download());
            QVERIFY(at(backend, "factory-image"));
            QVERIFY(shoot(w, "31-factory-image-download-not-possible"));
        }
        {
            RealTool tool("ready,slot-b");
            Backend backend;
            backend.setReleaseDirectory(Release);
            Window w(&backend);
            backend.start();
            QVERIFY(goTo(backend, "phone", Zip));
            QVERIFY(backend.readPhone());
            QTRY_VERIFY(!backend.busy() && backend.phones().size() == 2);
            QVERIFY(shoot(w, "32-which-phone"));
            backend.dismissPhones();
            w.window->setProperty("helpVisible", true);
            QVERIFY(shoot(w, "33-shortcuts"));
            w.window->setProperty("helpVisible", false);
        }
        // A run that was not started to write, to its last screen.
        {
            RealTool tool("ready");
            Backend backend;
            backend.setReleaseDirectory(Release);
            Window w(&backend);
            backend.start();
            QVERIFY(goTo(backend, "done", Zip));
            QVERIFY(shoot(w, "34-dry-run-only-done"));
        }
        // Closing while the phone is written: the stand-in holds a write
        // open long enough to ask.
        {
            standIn("flow-long-write");
            Backend backend;
            backend.setWrite(true);
            Window w(&backend);
            backend.start();
            QVERIFY(goTo(backend, "confirm", "/somewhere/cheetah-test.260101.001-factory-31c2322a.zip"));
            QVERIFY(backend.confirmInstall("cheetah"));
            QTRY_VERIFY(backend.writing() && backend.sentLines().size() >= 2);
            w.window->close();
            QVERIFY(shoot(w, "35-closing-while-the-phone-is-written"));
            QVERIFY(at(backend, "installed"));
        }
        QVERIFY2(saved.size() >= 24, qPrintable(saved.join(" ")));
        // No picture is empty.
        for (const QString &name : std::as_const(saved))
            QVERIFY2(QFileInfo(out + "/" + name + ".png").size() > 4000, qPrintable(name));
    }

    // ---- Theme and language ------------------------------------------------

    void themeFallsBack() {
        QTemporaryDir dir;
        Theme theme(dir.path());
        QCOMPARE(theme.accent(), QString("#FFD60A"));
        QCOMPARE(theme.accentForeground(), QString("black"));
    }

    void themeReadsAccent() {
        QTemporaryDir dir;
        QDir(dir.path()).mkpath("theme");
        write(dir.path() + "/theme/colors.toml", "accent = \"#112233\"\n");
        Theme theme(dir.path());
        QCOMPARE(theme.accent(), QString("#112233"));
        QCOMPARE(theme.accentForeground(), QString("white"));
    }

    void themeFollowsHyprlandRounding() {
        QTemporaryDir dir;
        const QString response = dir.filePath("response.json"), command = dir.filePath("hyprctl");
        write(command, "#!/bin/sh\ncat '" + response.toUtf8() + "'\n");
        QVERIFY(QFile::setPermissions(command, QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner));
        write(response, "{\"int\":0}");

        Theme theme(dir.path(), nullptr, command);
        QCOMPARE(theme.radius(), 0);
        write(response, "{\"int\":6}");
        QTRY_COMPARE(theme.radius(), 6);
    }

    void translatorFallsBackToEnglish() {
        QTemporaryDir dir;
        Translator translator(dir.path() + "/language", dir.path());
        QCOMPARE(translator.language(), QString("en"));
        QVERIFY(translator.translate("Main", "Close").isNull());
    }

    void translatorFollowsTheLanguageFile() {
        QTemporaryDir dir;
        write(dir.path() + "/language", "pt-BR\n");
        write(dir.path() + "/pt.json", R"({"Save": "Guardar", "Open": "Abrir"})");
        write(dir.path() + "/pt-BR.json", R"({"Save": "Salvar"})");

        Translator translator(dir.path() + "/language", dir.path());
        QCOMPARE(translator.language(), QString("pt-BR"));
        QCOMPARE(translator.translate("Main", "Save"), QString("Salvar"));
        QCOMPARE(translator.translate("Main", "Open"), QString("Abrir"));
        QVERIFY(translator.translate("Main", "Quit").isNull());
    }

    void theGermanCatalogTranslatesEveryStringOfTheApp() {
        QFile catalog(":/i18n/de.json");
        QVERIFY(catalog.open(QIODevice::ReadOnly));
        const QJsonObject german = QJsonDocument::fromJson(catalog.readAll()).object();
        QVERIFY(!german.isEmpty());

        // Every qsTr() and tr() of the sources has a line in the catalog, and
        // the catalog has no line the sources lost.
        QSet<QString> used;
        const QRegularExpression call("\\b(?:qsTr|tr)\\(\"((?:[^\"\\\\]|\\\\.)*)\"\\)");
        const QDir sources(Tests + "/../src");
        for (const QString &name : sources.entryList({"*.qml", "*.cpp"}, QDir::Files)) {
            if (name == "translator.cpp" || name == "cli.cpp")
                continue;
            QFile file(sources.filePath(name));
            QVERIFY(file.open(QIODevice::ReadOnly));
            auto matches = call.globalMatch(QString::fromUtf8(file.readAll()));
            while (matches.hasNext())
                used << matches.next().captured(1).replace("\\\"", "\"");
        }
        QVERIFY(used.size() > 30);
        for (const QString &text : used)
            QVERIFY2(german.value(text).isString() && !german.value(text).toString().isEmpty(), qPrintable("not in de.json: " + text));
        for (const QString &key : german.keys())
            QVERIFY2(used.contains(key), qPrintable("in de.json and nowhere in the sources: " + key));
        // Placeholders survive the translation.
        for (const QString &key : german.keys())
            for (const char *placeholder : {"%1", "%2"})
                QCOMPARE(german.value(key).toString().contains(placeholder), key.contains(placeholder));
    }

    void theWindowSpeaksTheLanguageAndLeavesTheCoresWordsAlone() {
        standIn("screen:warning");
        Backend backend;
        Window w(&backend);
        backend.start();
        QTRY_VERIFY(backend.hasScreen());
        w.settle();
        QCOMPARE(w.text("primaryButton"), QString("I have read this. Go on"));
        // Omarchy's language file changes: the app's own strings follow, live.
        QGuiApplication::installTranslator(&w.translator);
        write(w.directory.path() + "/language", "de\n");
        QTRY_COMPARE(w.translator.language(), QString("de"));
        w.engine.retranslate();
        w.settle();
        QCOMPARE(w.text("primaryButton"), QString("Gelesen. Weiter"));
        QCOMPARE(w.text("secondaryButton"), QString("Hier aufhören"));
        // The core's sentences arrive in English and are shown as they are.
        QCOMPARE(w.text("title"), QString("Read this once"));
        const QString shots = qEnvironmentVariable("OMAINSTALL_SHOTS");
        if (!shots.isEmpty())
            QVERIFY(w.window->grabWindow().save(shots + "/warning-de.png"));
        QGuiApplication::removeTranslator(&w.translator);
        QLocale::setDefault(QLocale::c());
        QGuiApplication::setLayoutDirection(Qt::LeftToRight);
        QVERIFY2(w.warnings.isEmpty(), qPrintable(w.warnings.join("\n")));
    }

public slots:
    void linkOpened(const QUrl &) { ++m_linksOpened; }

private:
    static void write(const QString &path, const QByteArray &contents) {
        QFile file(path);
        QVERIFY(file.open(QIODevice::WriteOnly));
        file.write(contents);
    }

    int m_linksOpened = 0;
};

QTEST_MAIN(OmainstallTests)
#include "omainstall_tests.moc"
