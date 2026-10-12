#!/bin/bash
# JNI end to end: builds a C library with natives of every kind and a Java program calling
# them, then compares rava's output with HotSpot's. Usage: tests/jni/run.sh [work-dir]; R
# overrides the rava binary.
R=$(realpath "${R:-$(dirname "$0")/../../target/release/rava}")
T=$(realpath "$(dirname "$0")")
W=${1:-$(mktemp -d)}
J=${JAVA_HOME:-$(dirname "$(dirname "$(readlink -f "$(which javac)")")")}
case $(uname -s) in
  Darwin) lib=libjnitest.dylib; inc=darwin ;;
  MINGW*|MSYS*|CYGWIN*) lib=jnitest.dll; inc=win32 ;;
  *) lib=libjnitest.so; inc=linux ;;
esac
set -e
javac -nowarn -d "$W" "$T/JniTest.java" 2>/dev/null
${CC:-cc} -shared -fPIC -O2 -I"$J/include" -I"$J/include/$inc" -o "$W/$lib" "$T/jnitest.c"
set +e
want=$(cd "$W" && java -Djava.library.path=. -cp . JniTest 2>&1 | grep -v "Picked up")
got=$(cd "$W" && "$R" -Djava.library.path=. -cp . JniTest 2>&1 | grep -v "Picked up")
if [ "$got" == "$want" ]; then echo "jni: pass"; else echo "jni: FAIL"; diff <(echo "$got") <(echo "$want"); exit 1; fi
