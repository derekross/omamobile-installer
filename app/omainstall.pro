# SPDX-License-Identifier: MIT
QT += core gui qml quick quickcontrols2 dbus

CONFIG += c++17 release
TARGET = omainstall
TEMPLATE = app

VERSION = 0.1.0
DEFINES += APP_VERSION=\\\"$$VERSION\\\"

HEADERS += \
    src/backend.h \
    src/cli.h \
    src/filepicker.h \
    src/portalfilepicker.h \
    src/theme.h \
    src/translator.h

SOURCES += \
    src/main.cpp \
    src/backend.cpp \
    src/cli.cpp \
    src/portalfilepicker.cpp \
    src/theme.cpp \
    src/translator.cpp

RESOURCES += src/resources.qrc
