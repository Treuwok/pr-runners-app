#!/usr/bin/env bash
# 建置 Android 版需要的環境變數（計畫 Global Constraints）。用法：source scripts/android-env.sh
# JDK 用 Android Studio 內建的 21；系統預設的 JDK 25 不要用於 Gradle。
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
export NDK_HOME="${NDK_HOME:-$ANDROID_HOME/ndk/28.2.13676358}"
export JAVA_HOME="${JAVA_HOME:-/Applications/Android Studio.app/Contents/jbr/Contents/Home}"
export PATH="$ANDROID_HOME/platform-tools:$ANDROID_HOME/emulator:$PATH"
