# SPDX-License-Identifier: MIT
QT += core gui qml quick quickcontrols2 testlib dbus
CONFIG += c++17 testcase
TARGET = omainstall_tests
TEMPLATE = app

INCLUDEPATH += ../src
# Where the stand-in for the installer's tool and the recorded screens are.
DEFINES += TESTS_DIR=\\\"$$PWD\\\"

HEADERS += ../src/backend.h ../src/filepicker.h ../src/theme.h ../src/translator.h
SOURCES += omainstall_tests.cpp ../src/backend.cpp ../src/theme.cpp ../src/translator.cpp
# The window itself, so the tests load the QML the app ships.
RESOURCES += ../src/resources.qrc
