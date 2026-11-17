#!/usr/bin/env bash
# Build the Starpod APK: libstarpod.so (crates/starpod-android) for each ABI,
# packed with android/AndroidManifest.xml and android/res, aligned and signed.
#
# The engine (meow, its newest stable release) is
# downloaded for each ABI and packed as lib/<abi>/libmeow.so, next to
# libstarpod.so, so Android runs it from the native library directory.
#
# The app is a NativeActivity with two small Java classes for the VPN
# (android/java), so no Gradle is needed: javac and d8 make classes.dex,
# aapt2 links the manifest and resources, the libraries are added as
# lib/<abi>/, zipalign aligns them and apksigner signs.
#
# Needs: the Android SDK (build-tools 34+ and a platform), a JDK 11+, the NDK,
# cargo-ndk, curl and tar (for the engine), and the Rust targets for the ABIs:
#   rustup target add aarch64-linux-android x86_64-linux-android
#   cargo install cargo-ndk
#
# Environment:
#   ANDROID_HOME        the SDK (default: $LOCALAPPDATA/Android/Sdk or ~/Android/Sdk)
#   ANDROID_NDK_HOME    the NDK (default: the newest under $ANDROID_HOME/ndk)
#   STARPOD_ABIS        space-separated ABIs (default: "arm64-v8a x86_64")
#   STARPOD_KEYSTORE    a keystore to sign with; without it a debug keystore is
#                       made in target/android/debug.keystore
#   STARPOD_KEYSTORE_PASS, STARPOD_KEY_ALIAS   its password and key alias
#   STARPOD_APK         the output path (default: target/android/starpod.apk)
#   STARPOD_ENGINE_VERSION  the meow version to pack (default: its newest release)
#   GITHUB_TOKEN        sent with the GitHub API call when set (its rate limit)
#
# Run from the repository root:  bash tools/build-android.sh
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

if [ -z "${ANDROID_HOME:-}" ]; then
    for candidate in "${LOCALAPPDATA:-}/Android/Sdk" "$HOME/Android/Sdk" "$HOME/Library/Android/sdk"; do
        if [ -d "$candidate" ]; then ANDROID_HOME=$candidate; break; fi
    done
fi
: "${ANDROID_HOME:?set ANDROID_HOME to the Android SDK}"
export ANDROID_HOME
if [ -z "${ANDROID_NDK_HOME:-}" ]; then
    ANDROID_NDK_HOME=$(ls -d "$ANDROID_HOME"/ndk/* 2>/dev/null | sort -V | tail -n1 || true)
fi
: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME to the NDK}"
export ANDROID_NDK_HOME

BUILD_TOOLS=$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -n1)
# The newest whole-numbered platform (a preview such as android-37.0 is skipped).
PLATFORM=$(ls -d "$ANDROID_HOME"/platforms/android-[0-9][0-9] | sort -V | tail -n1)
# Slint's Android backend compiles a Java helper against android.jar at build
# time. cargo-ndk sets ANDROID_PLATFORM to the minimum API (26), which need
# not be installed, so the jar is named outright.
ANDROID_JAR="$PLATFORM/android.jar"
if command -v cygpath >/dev/null 2>&1; then ANDROID_JAR=$(cygpath -m "$ANDROID_JAR"); fi
export ANDROID_JAR
EXE=""
BAT=""
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) EXE=".exe"; BAT=".bat" ;; esac
AAPT2="$BUILD_TOOLS/aapt2$EXE"
D8="$BUILD_TOOLS/d8$BAT"
ZIPALIGN="$BUILD_TOOLS/zipalign$EXE"
APKSIGNER="$BUILD_TOOLS/apksigner$BAT"

read -r -a ABIS <<< "${STARPOD_ABIS:-arm64-v8a x86_64}"
OUT="$ROOT/target/android"
APK="${STARPOD_APK:-$OUT/starpod.apk}"
mkdir -p "$OUT"

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
# 0.0.3 -> 3, 1.2.3 -> 10203: increases with every release.
IFS=. read -r MAJOR MINOR PATCH <<< "${VERSION%%-*}"
VERSION_CODE=$((MAJOR * 10000 + MINOR * 100 + PATCH))
echo "Starpod $VERSION (version code $VERSION_CODE), ABIs: ${ABIS[*]}"

# 0. The engine version: the newest stable meow release, unless
# STARPOD_ENGINE_VERSION names one. The app reads the same variable at compile
# time, so it reports the version packed with it. When GitHub cannot be asked,
# the version the code was written against is used.
if [ -z "${STARPOD_ENGINE_VERSION:-}" ]; then
    auth=()
    if [ -n "${GITHUB_TOKEN:-}" ]; then auth=(-H "Authorization: Bearer $GITHUB_TOKEN"); fi
    STARPOD_ENGINE_VERSION=$(curl -fsSL --retry 3 "${auth[@]}" \
        https://api.github.com/repos/meow-rs/meow-rs/releases/latest \
        | sed -n 's/.*"tag_name": *"v\([^"]*\)".*/\1/p' | head -n1 || true)
fi
if [ -z "${STARPOD_ENGINE_VERSION:-}" ]; then
    STARPOD_ENGINE_VERSION=$(sed -n 's/^ *None => "\(.*\)",$/\1/p' crates/starpod-engine/src/download.rs | head -n1)
    echo "could not ask GitHub for the newest engine; using $STARPOD_ENGINE_VERSION"
fi
: "${STARPOD_ENGINE_VERSION:?could not decide the engine version}"
export STARPOD_ENGINE_VERSION
ENGINE_VERSION=$STARPOD_ENGINE_VERSION
echo "engine: meow $ENGINE_VERSION"

# 1. The native library for each ABI.
targets=()
for abi in "${ABIS[@]}"; do targets+=(-t "$abi"); done
cargo ndk "${targets[@]}" --platform 26 -o "$OUT/lib" build --release -p starpod-android

# 2. The engine (meow) for each ABI, from its release on GitHub. The tarball
# is cached in $OUT/engine/ and its checksum is checked before and after use.
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}
engine_dir="$OUT/engine"
mkdir -p "$engine_dir"
for abi in "${ABIS[@]}"; do
    case "$abi" in
        arm64-v8a) triple=aarch64-unknown-linux-musl ;;
        x86_64) triple=x86_64-unknown-linux-musl ;;
        armeabi-v7a) triple=armv7-unknown-linux-musleabihf ;;
        *) echo "no meow engine build for ABI $abi (use arm64-v8a, x86_64 or armeabi-v7a)" >&2; exit 1 ;;
    esac
    name="meow-v$ENGINE_VERSION-$triple.tar.gz"
    url="https://github.com/meow-rs/meow-rs/releases/download/v$ENGINE_VERSION/$name"
    tarball="$engine_dir/$name"

    expected=$(curl -fsSL --retry 3 "$url.sha256" | awk '{print $1}')
    if [ -z "$expected" ]; then echo "could not read the checksum from $url.sha256" >&2; exit 1; fi
    if [ -f "$tarball" ] && [ "$(sha256_of "$tarball")" = "$expected" ]; then
        echo "engine $name is already downloaded"
    else
        echo "downloading the engine $name"
        curl -fsSL --retry 3 -o "$tarball" "$url"
        if [ "$(sha256_of "$tarball")" != "$expected" ]; then
            rm -f "$tarball"
            echo "checksum mismatch for $name, not using it" >&2
            exit 1
        fi
    fi

    extract="$engine_dir/extract-$abi"
    rm -rf "$extract"
    mkdir -p "$extract"
    tar -xzf "$tarball" -C "$extract"
    engine_bin=$(find "$extract" -name meow -type f | head -n1 || true)
    if [ -z "$engine_bin" ]; then echo "no meow binary in $name" >&2; exit 1; fi
    mkdir -p "$engine_dir/$abi"
    cp "$engine_bin" "$engine_dir/$abi/libmeow.so"
    chmod 755 "$engine_dir/$abi/libmeow.so"
    rm -rf "$extract"
done

# 3. Manifest and resources.
work="$OUT/apk-work"
rm -rf "$work"
mkdir -p "$work"
sed -e "s/__VERSION_CODE__/$VERSION_CODE/" -e "s/__VERSION_NAME__/$VERSION/" \
    android/AndroidManifest.xml > "$work/AndroidManifest.xml"
"$AAPT2" compile --dir android/res -o "$work/res.zip"
# A build signed with the debug key is debuggable, so `adb shell run-as` can
# reach its files; a release build (STARPOD_KEYSTORE set) is not.
DEBUG_MODE=()
if [ -z "${STARPOD_KEYSTORE:-}" ]; then DEBUG_MODE=(--debug-mode); fi
"$AAPT2" link -o "$work/unaligned.apk" -I "$PLATFORM/android.jar" "${DEBUG_MODE[@]}" \
    --manifest "$work/AndroidManifest.xml" "$work/res.zip"

# 4. The Java classes, compiled against the platform and dexed.
mkdir -p "$work/classes" "$work/pack"
javac --release 11 -classpath "$ANDROID_JAR" -d "$work/classes"     $(find android/java -name '*.java')
classes=()
while IFS= read -r -d '' class; do classes+=("$class"); done     < <(find "$work/classes" -name '*.class' -print0)
"$D8" --release --min-api 26 --lib "$ANDROID_JAR" --output "$work/pack" "${classes[@]}"

# 5. The libraries and the engine, stored uncompressed under lib/<abi>/.
mkdir -p "$work/pack/lib"
for abi in "${ABIS[@]}"; do
    mkdir -p "$work/pack/lib/$abi"
    cp "$OUT/lib/$abi/libstarpod.so" "$work/pack/lib/$abi/"
    cp "$engine_dir/$abi/libmeow.so" "$work/pack/lib/$abi/"
done
PYTHON=$(command -v python3 || command -v python)
"$PYTHON" - "$work/unaligned.apk" "$work/pack" <<'PY'
import os, sys, zipfile
apk, root = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(apk, "a") as zf:
    for folder, _, files in os.walk(root):
        for name in files:
            path = os.path.join(folder, name)
            arc = os.path.relpath(path, root).replace(os.sep, "/")
            zf.write(path, arc, compress_type=zipfile.ZIP_STORED)
PY

# 6. Align (page-aligned .so files) and sign.
"$ZIPALIGN" -f -p 4 "$work/unaligned.apk" "$work/aligned.apk"
if [ -n "${STARPOD_KEYSTORE:-}" ]; then
    KEYSTORE=$STARPOD_KEYSTORE
    PASS=${STARPOD_KEYSTORE_PASS:?set STARPOD_KEYSTORE_PASS}
    ALIAS=${STARPOD_KEY_ALIAS:?set STARPOD_KEY_ALIAS}
else
    KEYSTORE="$OUT/debug.keystore"
    PASS=android
    ALIAS=starpod-debug
    if [ ! -f "$KEYSTORE" ]; then
        keytool -genkeypair -keystore "$KEYSTORE" -storepass "$PASS" -keypass "$PASS" \
            -alias "$ALIAS" -keyalg RSA -keysize 2048 -validity 10000 \
            -dname "CN=Starpod debug" >/dev/null
    fi
    echo "signing with the debug keystore $KEYSTORE"
fi
"$APKSIGNER" sign --ks "$KEYSTORE" --ks-pass "pass:$PASS" --key-pass "pass:$PASS" \
    --ks-key-alias "$ALIAS" --out "$APK" "$work/aligned.apk"
"$APKSIGNER" verify "$APK"
echo "built $APK ($(du -h "$APK" | cut -f1))"
