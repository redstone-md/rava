#!/bin/bash
# Patches end to end: compiles a small program and a patch editing it (call redirects, opened
# members, lazily loaded patch classes), installs the patch as a jar in rava-patches/ of a
# work directory and checks rava's output. Usage: tests/patches/run.sh [work-dir]; R overrides
# the rava binary.
R=$(realpath "${R:-$(dirname "$0")/../../target/release/rava}")
T=$(realpath "$(dirname "$0")")
W=${1:-$(mktemp -d)}
set -e
rm -rf "$W/app" "$W/patch" "$W/rava-patches"; mkdir -p "$W/app" "$W/patch" "$W/rava-patches"
javac -nowarn -d "$W/app" $(find "$T/app" -name "*.java") 2>/dev/null
javac -nowarn -cp "$W/app" -d "$W/patch" $(find "$T/patch" -name "*.java") 2>/dev/null
cp "$T/patch/rava-patch.txt" "$W/patch/"
jar cf "$W/rava-patches/test-hooks.jar" -C "$W/patch" . 2>/dev/null
set +e
got=$(cd "$W" && "$R" -cp app ravatest.app.PatchApp 2>/dev/null)
if [ "$got" == "$(cat "$T/expected.txt")" ]; then echo "patches: pass"; else echo "patches: FAIL"; diff <(echo "$got") "$T/expected.txt"; exit 1; fi
