// SPDX-License-Identifier: MIT
#include "backend.h"

#include <QClipboard>
#include <QCoreApplication>
#include <QFileInfo>
#include <QGuiApplication>
#include <QJsonArray>
#include <QJsonDocument>
#include <QLocale>
#include <QStandardPaths>
#include <QTimeZone>

#include "filepicker.h"

namespace {
// A line longer than this is not a message of this protocol.
constexpr qint64 LongestLine = 4 << 20;

bool isText(const QJsonValue &value) { return value.isString(); }
bool isTextOrNull(const QJsonValue &value) { return value.isString() || value.isNull(); }
bool isTexts(const QJsonValue &value) {
    if (!value.isArray())
        return false;
    const QJsonArray array = value.toArray();
    for (const auto &item : array)
        if (!item.isString())
            return false;
    return true;
}
}

Backend::Backend(FilePicker *picker, QObject *parent) : QObject(parent), m_picker(picker) {
    m_helloTimer.setSingleShot(true);
    m_helloTimer.setInterval(10000);
    connect(&m_helloTimer, &QTimer::timeout, this, [this] {
        fail("silent", tr("The installer's tool started and did not say hello, so the app stopped it."));
    });
    connect(&m_process, &QProcess::readyReadStandardOutput, this, &Backend::readLines);
    connect(&m_process, &QProcess::readyReadStandardError, this, [this] {
        // Kept only to say what the tool wrote if it stops unexpectedly.
        m_stderr = (m_stderr + m_process.readAllStandardError()).right(2000);
    });
    connect(&m_process, &QProcess::errorOccurred, this, [this](QProcess::ProcessError error) {
        if (error == QProcess::FailedToStart)
            fail("start", tr("The installer's tool could not be started."), m_process.errorString());
    });
    connect(&m_process, &QProcess::finished, this, &Backend::exited);

    if (m_picker) {
        connect(m_picker, &FilePicker::picked, this, &Backend::takeFile);
        connect(m_picker, &FilePicker::failed, this, [this](const QString &reason) {
            setPickerError(tr("The file chooser could not be opened (%1). You can drop the file on this window instead.").arg(reason));
        });
    }
}

Backend::~Backend() {
    shutdown();
}

QString Backend::findTool(const QString &appDirectory) {
    auto program = [](const QString &path) {
        const QFileInfo info(path);
        return info.isFile() && info.isExecutable() ? info.absoluteFilePath() : QString();
    };
    const QString named = qEnvironmentVariable(ToolVariable);
    if (!named.isEmpty())
        return program(named);
    const QString beside = program(appDirectory + "/" + ToolName);
    return beside.isEmpty() ? QStandardPaths::findExecutable(ToolName) : beside;
}

QString Backend::checkScreen(const QJsonObject &screen) {
    static const char *const keys[] = {"state", "stop", "retry", "title", "notice", "body", "quote", "steps",
                                       "link", "file", "problem", "plan", "footer", "action"};
    for (const char *key : keys)
        if (!screen.contains(QLatin1String(key)))
            return QStringLiteral("the screen has no \"%1\"").arg(QLatin1String(key));
    for (const char *key : {"state", "title", "action"})
        if (!isText(screen.value(QLatin1String(key))))
            return QStringLiteral("\"%1\" is not text").arg(QLatin1String(key));
    for (const char *key : {"stop", "retry", "notice", "link", "problem"})
        if (!isTextOrNull(screen.value(QLatin1String(key))))
            return QStringLiteral("\"%1\" is neither text nor null").arg(QLatin1String(key));
    for (const char *key : {"body", "plan", "footer"})
        if (!isTexts(screen.value(QLatin1String(key))))
            return QStringLiteral("\"%1\" is not a list of texts").arg(QLatin1String(key));

    const QJsonValue quote = screen.value("quote");
    if (!quote.isNull()) {
        const QJsonObject q = quote.toObject();
        if (!quote.isObject() || !isText(q.value("intro")) || !isTexts(q.value("lines")) || !isTexts(q.value("after"))
            || !q.value("links").isArray())
            return QStringLiteral("\"quote\" is not a quote");
        const QJsonArray links = q.value("links").toArray();
        for (const auto &link : links)
            if (!isText(link.toObject().value("label")) || !isText(link.toObject().value("url")))
                return QStringLiteral("a link of the quote has no label or no url");
    }
    if (!screen.value("steps").isArray())
        return QStringLiteral("\"steps\" is not a list");
    const QJsonArray steps = screen.value("steps").toArray();
    for (const auto &step : steps) {
        const QJsonObject s = step.toObject();
        if (!isText(s.value("text")) || !s.contains("command") || !isTextOrNull(s.value("command")))
            return QStringLiteral("a step has no text or no command");
    }
    const QJsonValue file = screen.value("file");
    if (!file.isNull()) {
        const QJsonObject f = file.toObject();
        if (!file.isObject() || !isText(f.value("name")) || !f.value("size").isDouble() || !isText(f.value("build")))
            return QStringLiteral("\"file\" is not a file");
    }
    return {};
}

QString Backend::systemTimeZone() const {
    return QString::fromUtf8(QTimeZone::systemTimeZoneId());
}

void Backend::start() {
    launch(false);
}

void Backend::probe() {
    launch(true);
}

void Backend::restart() {
    // Not while a tool may still be writing: a new run would have to end it.
    if (m_writeInHand && m_process.state() != QProcess::NotRunning)
        return;
    shutdown();
    launch(false);
}

void Backend::reset() {
    m_stderr.clear();
    m_greeted = false;
    m_leaving = false;
    m_screen.clear();
    m_phones.clear();
    m_phonesTitle.clear();
    m_problems.clear();
    m_actionError.clear();
    m_pickerError.clear();
    m_toolErrorKind.clear();
    m_toolError.clear();
    m_toolErrorDetail.clear();
    m_toolVersion.clear();
    m_toolProtocol = 0;
    m_busy = false;
    m_writeInHand = false;
    m_inputClosed = false;
    m_withdrawn = false;
    m_unreadWrite = false;
    m_hasProgress = false;
    m_sentLines.clear();
    emit sentLinesChanged();
    emit screenChanged();
    emit phonesChanged();
    emit problemsChanged();
    emit actionErrorChanged();
    emit pickerErrorChanged();
    emit toolErrorChanged();
    emit toolChanged();
    emit busyChanged();
    emit progressChanged();
}

void Backend::launch(bool probeOnly) {
    if (m_process.state() != QProcess::NotRunning)
        return;
    reset();
    m_probeOnly = probeOnly;
    m_toolPath = findTool(QCoreApplication::applicationDirPath());
    emit toolChanged();
    if (m_toolPath.isEmpty()) {
        const QString named = qEnvironmentVariable(ToolVariable);
        fail("missing",
             tr("The installer's tool (%1) was not found. It belongs beside this app or on the PATH.").arg(QLatin1String(ToolName)),
             named.isEmpty() ? QString() : tr("%1 names %2, and that is not a program.").arg(QLatin1String(ToolVariable), named));
        return;
    }
    // These are all the arguments the app ever passes: the mode, the release,
    // and --write when the window itself was started with it, and only then.
    // A probe starts no flow and never passes it.
    QStringList arguments{"json"};
    if (m_write && !probeOnly)
        arguments << "--write";
    if (!m_releaseDirectory.isEmpty())
        arguments << "--release" << m_releaseDirectory;
    m_pending.clear();
    setBusy(true);
    m_helloTimer.start();
    m_process.start(m_toolPath, arguments);
}

void Backend::shutdown() {
    m_helloTimer.stop();
    if (m_process.state() == QProcess::NotRunning)
        return;
    // A tool that is writing is never ended from here: it takes the quit in
    // its turn, after the write, and the app waits for that.
    const bool wasWriting = m_writeInHand;
    m_leaving = true;
    if (m_process.state() == QProcess::Running) {
        // A tool the app stopped reading while it wrote was already told
        // that nothing more comes.
        if (!m_inputClosed) {
            const QByteArray line = "{\"type\":\"quit\"}\n";
            m_process.write(line);
            emit sent(line);
            m_process.closeWriteChannel();
            m_inputClosed = true;
        }
        if (m_process.waitForFinished(wasWriting ? m_writeWait : 1500))
            return;
    }
    m_process.kill();
    m_process.waitForFinished(1000);
}

void Backend::stopTool() {
    m_helloTimer.stop();
    m_leaving = true;
    if (m_process.state() == QProcess::NotRunning)
        return;
    // A tool that may be writing to a phone is not ended, whatever it sent:
    // it is told that nothing more comes, finishes the write, and leaves.
    if (m_writeInHand) {
        m_process.closeWriteChannel();
        m_inputClosed = true;
    } else {
        m_process.kill();
    }
}

void Backend::fail(const QString &kind, const QString &line, const QString &detail) {
    if (!m_toolErrorKind.isEmpty())
        return;
    m_toolErrorKind = kind;
    m_toolError = line;
    m_toolErrorDetail = detail;
    if (m_writeInHand && m_process.state() != QProcess::NotRunning) {
        const QString still = tr("The installer's tool was writing to the phone when this happened. The app has not ended it: it finishes the write and then leaves. Keep the phone connected.");
        m_toolErrorDetail = m_toolErrorDetail.isEmpty() ? still : m_toolErrorDetail + "\n" + still;
        m_unreadWrite = true;
    }
    stopTool();
    clearProgress();
    setBusy(false);
    emit toolErrorChanged();
}

void Backend::exited(int code, QProcess::ExitStatus status) {
    m_helloTimer.stop();
    if (m_writeInHand) {
        m_writeInHand = false;
        emit busyChanged();
    }
    // The tool the app could not read while it wrote has left. How its
    // write ended was in what the app did not read, and the app does not
    // make it up: it says that it does not know.
    if (m_unreadWrite) {
        m_unreadWrite = false;
        m_toolErrorDetail += "\n" + tr("The installer's tool has left now. The app could not read how the write ended, so it does not know what is on the phone.");
        emit toolErrorChanged();
    }
    if (m_leaving || !m_toolErrorKind.isEmpty())
        return;
    // What it wrote before it went still counts.
    readLines();
    if (m_leaving || !m_toolErrorKind.isEmpty())
        return;
    // After a last screen the tool may go without being asked.
    if (status == QProcess::NormalExit && code == 0 && action() == "none")
        return;
    const QString detail = QString::fromUtf8(m_stderr).trimmed();
    QString line = status == QProcess::CrashExit
                       ? tr("The installer's tool crashed.")
                       : tr("The installer's tool stopped unexpectedly (exit status %1).").arg(code);
    // A screen was up when it went: the app can't vouch for it any more.
    if (hasScreen())
        line += " " + tr("What this window showed before may no longer be true.");
    fail("crashed", line, detail);
}

void Backend::readLines() {
    while (m_toolErrorKind.isEmpty() && m_process.canReadLine())
        handle(m_process.readLine());
    if (m_toolErrorKind.isEmpty() && m_process.bytesAvailable() > LongestLine)
        fail("malformed", tr("The installer's tool sent something this app cannot read, so the app stopped it rather than guess."),
             tr("a line of more than %1 bytes").arg(LongestLine));
}

void Backend::handle(const QByteArray &raw) {
    const QByteArray line = raw.trimmed();
    if (line.isEmpty())
        return;
    auto malformed = [&](const QString &why) {
        fail("malformed", tr("The installer's tool sent something this app cannot read, so the app stopped it rather than guess."),
             why + ": " + QString::fromUtf8(line.left(200)));
    };
    QJsonParseError error;
    const QJsonDocument document = QJsonDocument::fromJson(line, &error);
    if (!document.isObject()) {
        malformed(error.error == QJsonParseError::NoError ? QStringLiteral("not a JSON object") : error.errorString());
        return;
    }
    const QJsonObject message = document.object();
    const QString type = message.value("type").toString();

    if (type == "fatal" && message.value("message").isString()) {
        fail("fatal", tr("The installer's tool reported that it failed:"), message.value("message").toString());
        return;
    }
    if (!m_greeted) {
        if (type != "hello") {
            malformed(QStringLiteral("the first message is not a hello"));
            return;
        }
        handleHello(message);
        return;
    }

    if (type == "screen") {
        const QJsonObject screen = message.value("screen").toObject();
        const QString wrong = message.value("screen").isObject() ? checkScreen(screen) : QStringLiteral("there is no screen in it");
        if (!wrong.isEmpty()) {
            malformed(wrong);
            return;
        }
        const QString answered = m_pending;
        // Whatever was asked for is answered: a write that was in hand is over.
        m_writeInHand = false;
        m_screen = screen.toVariantMap();
        clearProgress();
        emit screenChanged();
        setBusy(false);
        // The codename was taken and the core's screen now asks for the
        // writes: they follow at once, as on the command line. The owner's
        // go-ahead was the codename; the app adds no second question.
        if (answered == "confirm" && action() == "write" && !m_withdrawn)
            write();
    } else if (type == "sent") {
        if (!message.value("command").isString()) {
            malformed(QStringLiteral("a sent message without a command"));
            return;
        }
        m_sentLines << message.value("command").toString();
        emit sentLinesChanged();
    } else if (type == "piece") {
        if (!message.value("line").isString() || !message.value("number").isDouble()) {
            malformed(QStringLiteral("a piece message without a number or a line"));
            return;
        }
        m_sentLines << message.value("line").toString();
        emit sentLinesChanged();
    } else if (type == "account") {
        if (!isTexts(message.value("lines"))) {
            malformed(QStringLiteral("an account message without lines"));
            return;
        }
        const QJsonArray lines = message.value("lines").toArray();
        for (const auto &line : lines)
            m_sentLines << line.toString();
        emit sentLinesChanged();
    } else if (type == "progress") {
        if (!message.value("what").isString() || !message.value("done").isDouble() || !message.value("total").isDouble()) {
            malformed(QStringLiteral("a progress message without what, done or total"));
            return;
        }
        m_hasProgress = true;
        m_progressWhat = message.value("what").toString();
        m_progressDone = message.value("done").toDouble();
        m_progressTotal = message.value("total").toDouble();
        emit progressChanged();
    } else if (type == "phones") {
        const QJsonArray phones = message.value("phones").toArray();
        bool good = message.value("title").isString() && message.value("phones").isArray() && !phones.isEmpty();
        for (const auto &phone : phones)
            good = good && phone.toObject().value("id").isString() && phone.toObject().value("label").isString();
        if (!good) {
            malformed(QStringLiteral("a phones message without a title, or a phone without id or label"));
            return;
        }
        m_phonesTitle = message.value("title").toString();
        m_phones = phones.toVariantList();
        emit phonesChanged();
    } else if (type == "problems") {
        const QJsonArray problems = message.value("problems").toArray();
        bool good = message.value("problems").isArray();
        for (const auto &problem : problems)
            good = good && problem.toObject().value("field").isString() && problem.toObject().value("message").isString();
        if (!good) {
            malformed(QStringLiteral("a problems message whose problems have no field or no message"));
            return;
        }
        m_problems = problems.toVariantList();
        emit problemsChanged();
    } else if (type == "error") {
        if (!message.value("message").isString()) {
            malformed(QStringLiteral("an error without a message"));
            return;
        }
        setActionError(message.value("message").toString());
    } else if (type == "bye") {
        m_leaving = true;
    } else {
        malformed(QStringLiteral("a message of a kind this app does not know"));
    }
}

void Backend::handleHello(const QJsonObject &message) {
    const QJsonValue protocol = message.value("protocol");
    if (!protocol.isDouble() || protocol.toDouble() != protocol.toInt() || !message.value("version").isString()) {
        fail("malformed", tr("The installer's tool sent something this app cannot read, so the app stopped it rather than guess."),
             QStringLiteral("a hello without a protocol number or a version"));
        return;
    }
    m_helloTimer.stop();
    m_greeted = true;
    m_toolProtocol = protocol.toInt();
    m_toolVersion = message.value("version").toString();
    emit toolChanged();
    if (m_toolProtocol != Protocol) {
        fail("mismatch",
             tr("The installer's tool speaks protocol %1 and this app speaks protocol %2. Update whichever is older.")
                 .arg(m_toolProtocol).arg(Protocol));
        return;
    }
    // Whether the flow may write is the tool's to say, and it must be what
    // this window was started for: the app goes on with neither a tool that
    // writes when a dry run was asked for, nor the other way round.
    if (!message.value("writes").isBool()) {
        fail("malformed", tr("The installer's tool sent something this app cannot read, so the app stopped it rather than guess."),
             QStringLiteral("a hello that does not say whether the run writes"));
        return;
    }
    if (!m_probeOnly && message.value("writes").toBool() != m_write) {
        fail("mode", m_write
                         ? tr("This window was started to install, and the installer's tool started a run that cannot write. The app stopped it.")
                         : tr("This window was started for a dry run, and the installer's tool started a run that can write. The app stopped it."));
        return;
    }
    emit ready();
    if (m_probeOnly) {
        setBusy(false);
        shutdown();
        return;
    }
    send({{"type", "hello"}, {"protocol", Protocol}, {"app", "omainstall"}, {"version", QCoreApplication::applicationVersion()}}, false);
    send({{"type", "start"}, {"reinstall", m_reinstall}}, false);
}

bool Backend::send(const QJsonObject &message, bool makesBusy) {
    if (m_process.state() != QProcess::Running || !m_toolErrorKind.isEmpty() || m_leaving)
        return false;
    const QByteArray line = QJsonDocument(message).toJson(QJsonDocument::Compact) + '\n';
    m_process.write(line);
    emit sent(line);
    if (makesBusy) {
        // What the last answer said belongs to the last action.
        if (!m_phones.isEmpty()) {
            m_phones.clear();
            m_phonesTitle.clear();
            emit phonesChanged();
        }
        if (!m_problems.isEmpty()) {
            m_problems.clear();
            emit problemsChanged();
        }
        setActionError({});
        setPickerError({});
        m_pending = message.value("type").toString();
        setBusy(true);
    }
    return true;
}

bool Backend::sendFor(std::initializer_list<const char *> actions, const QJsonObject &message) {
    if (m_busy)
        return false;
    const QString current = action();
    for (const char *allowed : actions)
        if (current == QLatin1String(allowed))
            return send(message);
    return false;
}

bool Backend::acknowledge() { return sendFor({"acknowledge"}, {{"type", "acknowledge"}}); }
bool Backend::decline() { return sendFor({"acknowledge"}, {{"type", "decline"}}); }
bool Backend::acceptTerms() { return sendFor({"accept-terms"}, {{"type", "accept-terms"}}); }
bool Backend::declineTerms() { return sendFor({"accept-terms"}, {{"type", "decline-terms"}}); }
bool Backend::download() { return sendFor({"download"}, {{"type", "download"}, {"do", "start"}}); }
bool Backend::dryRun() { return sendFor({"dry-run"}, {{"type", "dry-run"}}); }
bool Backend::write() {
    if (m_busy || action() != "write")
        return false;
    // Set before the message leaves, so that nothing between here and the
    // tool's answer can take the tool for one that may be ended.
    m_writeInHand = true;
    if (send({{"type", "write"}}))
        return true;
    m_writeInHand = false;
    return false;
}
bool Backend::declineInstall() { return sendFor({"confirm"}, {{"type", "confirm"}, {"typed", ""}}); }

bool Backend::confirmInstall(const QString &typed) {
    // Nothing typed is not a go-ahead, and is not sent as one by accident:
    // declining is its own button.
    if (typed.isEmpty())
        return false;
    return sendFor({"confirm"}, {{"type", "confirm"}, {"typed", typed}});
}

bool Backend::readPhone(const QString &phone) {
    QJsonObject message{{"type", "read-phone"}};
    if (!phone.isEmpty())
        message.insert("phone", phone);
    return sendFor({"read-phone"}, message);
}

bool Backend::cancelDownload() {
    // The one message sent while an action is under way; the screen that
    // ends the download is still to come.
    if (pending() != "download")
        return false;
    return send({{"type", "download"}, {"do", "cancel"}}, false);
}

bool Backend::takeFile(const QString &path) {
    if (path.isEmpty())
        return false;
    return sendFor({"download", "take-file"}, {{"type", "take-file"}, {"path", QFileInfo(path).absoluteFilePath()}});
}

bool Backend::takeFileUrl(const QUrl &url) {
    return url.isLocalFile() && takeFile(url.toLocalFile());
}

bool Backend::submitAnswers(const QVariantMap &answers) {
    // The fields the flow takes today (core/src/flow.rs, AnswersInput), as
    // typed: every rule is the core's.
    QJsonObject message{{"type", "answers"}};
    for (const char *field : {"username", "password", "password_again", "hostname", "timezone", "wifi_ssid", "wifi_psk"})
        message.insert(QLatin1String(field), answers.value(QLatin1String(field)).toString());
    return sendFor({"answers"}, message);
}

bool Backend::primaryAction() {
    const QString current = action();
    if (current == "acknowledge")
        return acknowledge();
    if (current == "read-phone")
        return readPhone();
    if (current == "accept-terms")
        return acceptTerms();
    if (current == "download")
        return download();
    if (current == "dry-run")
        return dryRun();
    if (current == "write")
        return write();
    if (current == "take-file" && !m_busy) {
        pickFile();
        return true;
    }
    return false;
}

void Backend::pickFile() {
    if (m_busy || (action() != "download" && action() != "take-file"))
        return;
    setPickerError({});
    if (!m_picker) {
        setPickerError(tr("No file chooser is available. You can drop the file on this window instead."));
        return;
    }
    m_picker->open(tr("Choose the file you saved"));
}

void Backend::dismissPhones() {
    if (m_phones.isEmpty())
        return;
    m_phones.clear();
    m_phonesTitle.clear();
    emit phonesChanged();
}

QString Backend::problemFor(const QString &field) const {
    for (const auto &problem : m_problems)
        if (problem.toMap().value("field").toString() == field)
            return problem.toMap().value("message").toString();
    return {};
}

bool Backend::isWebLink(const QString &url) const {
    const QUrl parsed(url, QUrl::StrictMode);
    return parsed.isValid() && parsed.scheme() == "https" && !parsed.host().isEmpty();
}

void Backend::copy(const QString &text) {
    if (qobject_cast<QGuiApplication *>(QCoreApplication::instance()))
        QGuiApplication::clipboard()->setText(text);
}

QString Backend::formatBytes(double bytes) const {
    return QLocale().formattedDataSize(qint64(bytes), 1, QLocale::DataSizeSIFormat);
}

void Backend::setBusy(bool busy) {
    if (busy == m_busy)
        return;
    m_busy = busy;
    emit busyChanged();
}

void Backend::clearProgress() {
    if (!m_hasProgress)
        return;
    m_hasProgress = false;
    m_progressWhat.clear();
    m_progressDone = m_progressTotal = 0;
    emit progressChanged();
}

void Backend::setActionError(const QString &error) {
    if (error == m_actionError)
        return;
    m_actionError = error;
    emit actionErrorChanged();
}

void Backend::setPickerError(const QString &error) {
    if (error == m_pickerError)
        return;
    m_pickerError = error;
    emit pickerErrorChanged();
}
