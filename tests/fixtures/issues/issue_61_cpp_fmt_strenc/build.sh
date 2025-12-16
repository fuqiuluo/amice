#!/usr/bin/env bash
# git clone https://github.com/fmtlib/fmt

LD_LIBRARY_PATH=~/Downloads/clang-linux-x86-ndk-r29-r563880c/lib:$LD_LIBRARY_PATH
export LD_LIBRARY_PATH

export AMICE_STRING_ENCRYPTION=true
export AMICE_PATH=~/Downloads/libamice-linux-x64-android-ndk-r29/libamice.so

NDK_ROOT=$ANDROID_HOME/ndk/29.0.14206865

cmake \
    -H. \
    -Bbuild \
    -DANDROID_ABI=arm64-v8a \
    -DANDROID_PLATFORM=android-26 \
    -DANDROID_NDK=$NDK_ROOT \
    -DCMAKE_TOOLCHAIN_FILE=$NDK_ROOT/build/cmake/android.toolchain.cmake \
    -G Ninja
cd build
ninja
