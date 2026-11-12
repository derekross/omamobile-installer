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
    explicit RealTool(const QString &phones, const QString &google = {}) {
        const QString tool = qEnvironmentVariable("OMAINSTALL_REAL_TOOL");
        QFile script(directory.filePath("omamobile-install"));
        if (tool.isEmpty() || !script.open(QIODevice::WriteOnly))
            return;
        QString line = QStringLiteral("exec '%1' \"$@\" --fake-phone '%2' --work-dir '%3'").arg(tool, phones, directory.filePath("work"));
        if (!google.isEmpty())
            line += QStringLiteral(" --fake-google '%1'").arg(google);
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

    QTemporaryDir directory;
    bool ok = false;
};

// A stand-in for Google's server, for the real tool's downloads: a socket on
// 127.0.0.1 in this process. Nothing here reaches Google.
class Google : public QObject {
public:
    // The file; its first 20000 bytes and then nothing more; or a file of
    // the right size with one byte changed.
    enum Mode { Whole, Stalls, Wrong };
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
        const auto n = [](qsizetype number) { return QByteArray::number(number); };
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
        {"take-file", "Choose the file…"}, {"answers", "Go on"}, {"dry-run", "Run the dry run"}, {"none", "Close"}};
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
        for (const char *state : {"warning", "phone", "unlock", "google-terms", "factory-image", "answers", "dry-run", "done", "stopped"})
            QVERIFY2(states.contains(state), state);
        for (const char *stop : {"release-not-verified", "no-build-index", "build-index-refused", "warning-declined",
                                 "wrong-model", "phone-changed", "runs-from-slot-b", "bootloader-slots-unknown",
                                 "bootloader-slots-differ", "baseband-unknown", "build-not-listed", "build-not-tested",
                                 "factory-download-mismatch", "locked-at-dry-run", "build-changed", "dry-run-refused"})
            QVERIFY2(stops.contains(stop), stop);
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
                 (QJsonObject{{"type", "hello"}, {"protocol", 1}, {"app", "omainstall"}, {"version", QCoreApplication::applicationVersion()}}));
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
        };
        for (const char *screen : {"warning", "phone", "google-terms", "factory-image-take-file", "answers", "dry-run", "done",
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
        QTRY_VERIFY(backend.hasProgress());
        QCOMPARE(backend.progressWhat(), QString("download"));
        QCOMPARE(backend.progressTotal(), 49152.0);
        QVERIFY(next("answers"));
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
        // No message of the protocol can ask for a write, and the app passes
        // no argument but the mode.
        QVERIFY(!written.contains("write") && !written.contains("flash"));
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
        QTest::newRow("a protocol mismatch") << QByteArray("mismatch") << "mismatch" << "speaks protocol 99 and this app speaks protocol 1" << "";
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
        if (kind == "mismatch" || kind == "silent")
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
                     "echo '{\"type\":\"hello\",\"protocol\":1,\"tool\":\"t\",\"version\":\"0\"}'\n"
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
        QCOMPARE(w.shown("secondaryButton"), action == "acknowledge" || action == "accept-terms" || action == "download" || again);
        QCOMPARE(w.shown("answersLoader"), action == "answers");
        QVERIFY(!w.shown("toolError"));
        QVERIFY(!w.shown("unknownAction"));
        QVERIFY(w.text("banner").contains("cannot write to a phone"));
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
        screen.insert("action", "write");
        const QString tool = directory.filePath("tool");
        QFile script(tool);
        QVERIFY(script.open(QIODevice::WriteOnly));
        script.write("#!/bin/sh\n"
                     "echo '{\"type\":\"hello\",\"protocol\":1,\"tool\":\"t\",\"version\":\"0\"}'\n"
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
        QVERIFY(w.text("unknownAction").contains("(write)"));
        QVERIFY(!w.shown("primaryButton"));
        QCOMPARE(w.text("secondaryButton"), QString("Close"));
        QVERIFY(!backend.primaryAction());
        QVERIFY(!backend.dryRun());
        QCOMPARE(sent.count(), 0);
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
