// SPDX-License-Identifier: MIT
#pragma once

#include <QObject>
#include <QString>

// Asks the person for one file they already have. The window only ever talks
// to this interface, so the tests hand the backend a fake and no dialog opens.
// The pattern (an abstract picker with the portal behind it) is Omacut's
// (MIT, David Heinemeier Hansson).
class FilePicker : public QObject {
    Q_OBJECT

public:
    using QObject::QObject;

    // Asks for one existing file. Exactly one of the three signals follows.
    virtual void open(const QString &title) = 0;
    // Asks for one folder (the install link's radio backup). The same three
    // signals follow. A picker that has no folder chooser says so.
    virtual void openFolder(const QString &title) {
        Q_UNUSED(title);
        emit failed(QStringLiteral("this chooser picks no folders"));
    }

signals:
    void picked(const QString &path);
    void cancelled();
    // The picker itself could not be shown; `reason` is the system's words.
    void failed(const QString &reason);
};
