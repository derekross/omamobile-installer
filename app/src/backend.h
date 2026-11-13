// SPDX-License-Identifier: MIT
#pragma once

#include <QJsonObject>
#include <QObject>
#include <QProcess>
#include <QStringList>
#include <QTimer>
#include <QUrl>
#include <QVariantList>
#include <QVariantMap>

class FilePicker;

// The app's whole job on the C++ side: start the installer's command-line
// tool, exchange JSON lines with it (docs/APP.md), and hand QML the screen
// the installer's core sent, as it was sent.
//
// Nothing here decides anything about an install or words anything about a
// phone. The order of the steps, every refusal and every sentence a person
// reads about the phone are the core's (core/src/flow.rs). What is the
// app's own: finding the tool, saying when the tool itself fails, and the
// labels of its buttons.
class Backend : public QObject {
    Q_OBJECT
    // The core's Screen, key for key. Empty until the first one arrives.
    Q_PROPERTY(QVariantMap screen READ screen NOTIFY screenChanged)
    Q_PROPERTY(bool hasScreen READ hasScreen NOTIFY screenChanged)
    Q_PROPERTY(QString state READ state NOTIFY screenChanged)
    Q_PROPERTY(QString action READ action NOTIFY screenChanged)
    // The core says that starting the installer again can get past the stop
    // on show (Screen.retry): at any time, or once its message's fix is done.
    Q_PROPERTY(bool canStartAgain READ canStartAgain NOTIFY screenChanged)
    // An action was handed in and the tool has not answered with a screen.
    Q_PROPERTY(bool busy READ busy NOTIFY busyChanged)
    // The message that made it busy ("download", "take-file", ...), or nothing.
    Q_PROPERTY(QString pending READ pending NOTIFY busyChanged)
    // The flow of this run may write to a phone: the window was started with
    // --write, and the tool was started with it and said so in its hello.
    Q_PROPERTY(bool writes READ writes NOTIFY toolChanged)
    // The writes are under way: the one action that is never cut off.
    Q_PROPERTY(bool writing READ writing NOTIFY busyChanged)
    // What the session that writes said it sent, as the tool put it: each
    // command the phone received, a line for each piece of userdata, and at
    // the end the tool's account of the pieces. Kept until a new run.
    Q_PROPERTY(QStringList sentLines READ sentLines NOTIFY sentLinesChanged)
    Q_PROPERTY(bool hasProgress READ hasProgress NOTIFY progressChanged)
    Q_PROPERTY(QString progressWhat READ progressWhat NOTIFY progressChanged)
    Q_PROPERTY(double progressDone READ progressDone NOTIFY progressChanged)
    Q_PROPERTY(double progressTotal READ progressTotal NOTIFY progressChanged)
    // More than one phone answered: the tool's question and its choices.
    Q_PROPERTY(QString phonesTitle READ phonesTitle NOTIFY phonesChanged)
    Q_PROPERTY(QVariantList phones READ phones NOTIFY phonesChanged)
    // The core's objections to the owner's answers: [{field, message}].
    Q_PROPERTY(QVariantList problems READ problems NOTIFY problemsChanged)
    // The tool's words on an action that did not go through.
    Q_PROPERTY(QString actionError READ actionError NOTIFY actionErrorChanged)
    Q_PROPERTY(QString pickerError READ pickerError NOTIFY pickerErrorChanged)
    // The tool itself failed: which way, the app's line, the tool's words.
    Q_PROPERTY(QString toolErrorKind READ toolErrorKind NOTIFY toolErrorChanged)
    Q_PROPERTY(QString toolError READ toolError NOTIFY toolErrorChanged)
    Q_PROPERTY(QString toolErrorDetail READ toolErrorDetail NOTIFY toolErrorChanged)
    Q_PROPERTY(QString toolPath READ toolPath NOTIFY toolChanged)
    Q_PROPERTY(QString toolVersion READ toolVersion NOTIFY toolChanged)
    Q_PROPERTY(int toolProtocol READ toolProtocol NOTIFY toolChanged)
    Q_PROPERTY(QString systemTimeZone READ systemTimeZone CONSTANT)
    // After the write: the phone works by itself (the core's action `wait`),
    // and the tool sends each new screen. The app hands in nothing then.
    Q_PROPERTY(bool phoneWorking READ phoneWorking NOTIFY screenChanged)
    // What a pretend phone's screen shows, in a pretend build: for the tests.
    Q_PROPERTY(QString pretendShows READ pretendShows NOTIFY screenChanged)

public:
    // The protocol this app speaks (docs/APP.md).
    static constexpr int Protocol = 5;
    // The tool's name, beside the app's binary or on PATH.
    static constexpr const char *ToolName = "omamobile-install";
    // Names another tool, for the tests.
    static constexpr const char *ToolVariable = "OMAINSTALL_TOOL";

    // `picker` stays the caller's. Without one, picking a file says that no
    // chooser is available.
    explicit Backend(FilePicker *picker = nullptr, QObject *parent = nullptr);
    ~Backend() override;

    // Where the tool is: the variable, then beside the app, then PATH. Empty
    // when there is none. A variable that names no program finds nothing:
    // falling through to another tool would hide the mistake.
    static QString findTool(const QString &appDirectory);
    // A Screen as the core serialises it: every key, each of its type.
    // Returns what is wrong, or nothing.
    static QString checkScreen(const QJsonObject &screen);

    // What the person said up front, before start().
    void setReinstall(bool reinstall) { m_reinstall = reinstall; }
    // The flow may write. Said once, when the window is started (`omainstall
    // open --write`), and passed on as the tool's own --write: there is no
    // message for it, and nothing in the window sets it.
    void setWrite(bool write) { m_write = write; }
    // How long shutdown() waits for a tool that is writing before it gives
    // up on it. Long: a write that is cut off leaves a phone half written.
    void setWriteWait(int milliseconds) { m_writeWait = milliseconds; }
    void setReleaseDirectory(const QString &directory) { m_releaseDirectory = directory; }
    void setHelloTimeout(int milliseconds) { m_helloTimer.setInterval(milliseconds); }

    QVariantMap screen() const { return m_screen; }
    bool hasScreen() const { return !m_screen.isEmpty(); }
    QString state() const { return m_screen.value("state").toString(); }
    QString action() const { return m_screen.value("action").toString(); }
    bool canStartAgain() const {
        const QString retry = m_screen.value("retry").toString();
        return action() == "none" && (retry == "whenever" || retry == "after-fix");
    }
    bool writes() const { return m_write; }
    // From the moment the writes were asked for until the tool answered
    // them with a screen, or left. Also true after the app stopped reading
    // the tool (a line it could not read): the tool may still be writing.
    bool writing() const { return m_writeInHand; }
    QStringList sentLines() const { return m_sentLines; }
    bool busy() const { return m_busy; }
    QString pending() const { return m_busy ? m_pending : QString(); }
    bool hasProgress() const { return m_hasProgress; }
    QString progressWhat() const { return m_progressWhat; }
    double progressDone() const { return m_progressDone; }
    double progressTotal() const { return m_progressTotal; }
    QString phonesTitle() const { return m_phonesTitle; }
    QVariantList phones() const { return m_phones; }
    QVariantList problems() const { return m_problems; }
    QString actionError() const { return m_actionError; }
    QString pickerError() const { return m_pickerError; }
    QString toolErrorKind() const { return m_toolErrorKind; }
    QString toolError() const { return m_toolError; }
    QString toolErrorDetail() const { return m_toolErrorDetail; }
    QString toolPath() const { return m_toolPath; }
    QString toolVersion() const { return m_toolVersion; }
    int toolProtocol() const { return m_toolProtocol; }
    QString systemTimeZone() const;
    bool phoneWorking() const { return action() == "wait"; }
    QString pretendShows() const { return m_pretendShows; }

    // Starts the tool and the install flow. Call once the event loop runs.
    Q_INVOKABLE void start();
    // Starts the tool, reads its hello, and leaves again: for `check`.
    void probe();
    // A new flow, with a new run of the tool.
    Q_INVOKABLE void restart();
    // Tells the tool to leave and waits a moment for it. Safe to call twice.
    Q_INVOKABLE void shutdown();

    // One per message of the protocol. Each returns whether it was sent: an
    // action the current screen does not name is not sent at all.
    Q_INVOKABLE bool acknowledge();
    Q_INVOKABLE bool decline();
    Q_INVOKABLE bool readPhone(const QString &phone = {});
    // The owner's answer to the core's question of what slot B holds: one
    // of the screen's `choices`, by its `answer`. Anything else is not sent.
    Q_INVOKABLE bool answerSlotB(const QString &answer);
    // One of the choices of a screen whose action is `slot-b` or `after`
    // (the steps after the write): sent as that action with its answer.
    Q_INVOKABLE bool answerChoice(const QString &answer);
    Q_INVOKABLE bool acceptTerms();
    Q_INVOKABLE bool declineTerms();
    Q_INVOKABLE bool download();
    Q_INVOKABLE bool cancelDownload();
    Q_INVOKABLE bool takeFile(const QString &path);
    // A file dropped on the window.
    Q_INVOKABLE bool takeFileUrl(const QUrl &url);
    Q_INVOKABLE bool submitAnswers(const QVariantMap &answers);
    Q_INVOKABLE bool dryRun();
    // What the owner typed where the core asks for the phone's codename, as
    // typed. The core takes the codename and stops for anything else.
    Q_INVOKABLE bool confirmInstall(const QString &typed);
    // The owner does not go ahead: nothing typed is handed in, and the
    // core's own stop follows, with nothing written.
    Q_INVOKABLE bool declineInstall();
    // Sent by itself once the codename was taken (the core's screen then
    // names it); by the person only when that try found no phone.
    Q_INVOKABLE bool write();
    // After the write, the install link: sent by itself when the core's
    // `installed` screen asks for it, so the owner sees the first start.
    Q_INVOKABLE bool startLink();
    // The code the phone's screen shows, as typed.
    Q_INVOKABLE bool pair(const QString &code);
    // Shows the folder chooser; the folder picked goes to backupDir().
    Q_INVOKABLE void pickBackupFolder();
    // Where the radio backup goes: an absolute path to a new, empty folder.
    Q_INVOKABLE bool backupDir(const QString &path);
    Q_INVOKABLE bool retry();
    // The window is going: nothing more is sent by the app itself (the
    // writes that would follow a codename still on its way are not).
    Q_INVOKABLE void withdraw() { m_withdrawn = true; }

    // The screen's main action, for the button and for Space. `answers` and
    // `none` are the window's: a form to hand in, a window to close.
    Q_INVOKABLE bool primaryAction();
    // Shows the file chooser; the file picked is handed to takeFile().
    Q_INVOKABLE void pickFile();
    Q_INVOKABLE void dismissPhones();
    // The first of the core's objections to this field of the answers.
    Q_INVOKABLE QString problemFor(const QString &field) const;
    // Only pages on the web are opened, whatever a screen's link says.
    Q_INVOKABLE bool isWebLink(const QString &url) const;
    Q_INVOKABLE void copy(const QString &text);
    // A byte count for a person, in the language's own form.
    Q_INVOKABLE QString formatBytes(double bytes) const;

signals:
    void screenChanged();
    void busyChanged();
    void progressChanged();
    void phonesChanged();
    void problemsChanged();
    void actionErrorChanged();
    void pickerErrorChanged();
    void toolErrorChanged();
    void toolChanged();
    void sentLinesChanged();
    // The tool said hello with a protocol this app speaks.
    void ready();
    // Every line written to the tool, as written. For the tests.
    void sent(const QByteArray &line);

private:
    void launch(bool probeOnly);
    void reset();
    void readLines();
    void handle(const QByteArray &line);
    void handleHello(const QJsonObject &message);
    void exited(int code, QProcess::ExitStatus status);
    bool send(const QJsonObject &message, bool makesBusy = true);
    bool sendFor(std::initializer_list<const char *> actions, const QJsonObject &message);
    void fail(const QString &kind, const QString &line, const QString &detail = {});
    void stopTool();
    void setBusy(bool busy);
    void clearProgress();
    void setActionError(const QString &error);
    void setPickerError(const QString &error);

    FilePicker *m_picker;
    QProcess m_process;
    QTimer m_helloTimer;
    QByteArray m_stderr;
    bool m_probeOnly = false;
    bool m_greeted = false;
    bool m_leaving = false;
    bool m_reinstall = false;
    bool m_write = false;
    bool m_writeInHand = false;
    bool m_inputClosed = false;
    bool m_withdrawn = false;
    // The app stopped reading the tool while the tool was writing.
    bool m_unreadWrite = false;
    int m_writeWait = 30 * 60 * 1000;
    QString m_releaseDirectory;
    // The chooser was opened for the backup's folder, not for a file.
    bool m_pickingFolder = false;
    QString m_pretendShows;

    QVariantMap m_screen;
    bool m_busy = false;
    QString m_pending;
    bool m_hasProgress = false;
    QStringList m_sentLines;
    QString m_progressWhat;
    double m_progressDone = 0;
    double m_progressTotal = 0;
    QString m_phonesTitle;
    QVariantList m_phones;
    QVariantList m_problems;
    QString m_actionError;
    QString m_pickerError;
    QString m_toolErrorKind;
    QString m_toolError;
    QString m_toolErrorDetail;
    QString m_toolPath;
    QString m_toolVersion;
    int m_toolProtocol = 0;
};
