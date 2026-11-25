#!/usr/bin/env bash
# Builds android/build/kamaitachi.apk without Gradle: cargo-ndk for the native
# library, javac and d8 for the Java side, aapt2 for the manifest and icon, then
# zipalign and apksigner. Needs the Android SDK (platform, build-tools, NDK), a JDK,
# zip, and cargo-ndk with the aarch64-linux-android target.
#
# The APK is signed with a key kept outside the repo (KEYSTORE, by default
# ~/.android/kamaitachi.keystore, made on the first run). Keep it: an update signed
# with another key will not install over this one.
set -euo pipefail
cd "$(dirname "$0")"

: "${ANDROID_HOME:=/data/android}"
NDK=${ANDROID_NDK_HOME:-$(ls -d "$ANDROID_HOME"/ndk/* | tail -1)}
BT=$(ls -d "$ANDROID_HOME"/build-tools/* | tail -1)
JAR=$(ls -d "$ANDROID_HOME"/platforms/android-* | tail -1)/android.jar
KEYSTORE=${KEYSTORE:-$HOME/.android/kamaitachi.keystore}
OUT=build

rm -rf "$OUT/classes" "$OUT/apk"
mkdir -p "$OUT/classes" "$OUT/apk/lib"

echo "== native library"
ANDROID_NDK_HOME=$NDK cargo ndk -t arm64-v8a -o "$OUT/apk/lib" build --release

echo "== java"
javac --release 8 -Xlint:-options -classpath "$JAR" -d "$OUT/classes" $(find java -name '*.java')
"$BT/d8" --release --min-api 30 --lib "$JAR" --output "$OUT/apk" $(find "$OUT/classes" -name '*.class')

echo "== resources and manifest"
"$BT/aapt2" compile --dir res -o "$OUT/res.zip"
"$BT/aapt2" link -o "$OUT/unsigned.apk" -I "$JAR" --manifest AndroidManifest.xml "$OUT/res.zip"
(cd "$OUT/apk" && zip -qr ../unsigned.apk classes.dex lib)

echo "== align and sign"
if [ ! -f "$KEYSTORE" ]; then
  mkdir -p "$(dirname "$KEYSTORE")"
  keytool -genkeypair -keystore "$KEYSTORE" -storepass android -keypass android -alias kamaitachi \
    -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=kamaitachi" >/dev/null
fi
"$BT/zipalign" -f -p 4 "$OUT/unsigned.apk" "$OUT/aligned.apk"
"$BT/apksigner" sign --ks "$KEYSTORE" --ks-pass pass:android --ks-key-alias kamaitachi --out "$OUT/kamaitachi.apk" "$OUT/aligned.apk"
rm -f "$OUT/unsigned.apk" "$OUT/aligned.apk" "$OUT/kamaitachi.apk.idsig"
ls -la "$OUT/kamaitachi.apk"
