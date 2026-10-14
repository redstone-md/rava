#include <jni.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#ifdef _WIN32
#include <windows.h>
#else
#include <pthread.h>
#endif

JNIEXPORT jint JNICALL Java_JniTest_add(JNIEnv *env, jclass c, jint a, jint b) { return a + b; }

JNIEXPORT jlong JNICALL Java_JniTest_mix(JNIEnv *env, jclass c, jbyte b, jshort s, jchar ch, jint i, jlong l, jfloat f, jdouble d, jboolean z) {
    return (jlong) b + s + ch + i + l + (jlong) (f * 100) + (jlong) (d * 100) + (z ? 1000 : 0);
}

JNIEXPORT jdouble JNICALL Java_JniTest_many(JNIEnv *env, jclass c, jdouble a, jint b, jdouble cc, jint d, jdouble e, jint f, jdouble g, jint h,
                                            jdouble i, jint j, jdouble k, jint l, jdouble m, jint n, jdouble o, jint p, jdouble q, jdouble r) {
    return a + b * 2 + cc * 3 + d * 4 + e * 5 + f * 6 + g * 7 + h * 8 + i * 9 + j * 10 + k * 11 + l * 12 + m * 13 + n * 14 + o * 15 + p * 16 + q * 17 + r * 18;
}

JNIEXPORT jstring JNICALL Java_JniTest_greet(JNIEnv *env, jclass c, jstring name) {
    const char *s = (*env)->GetStringUTFChars(env, name, NULL);
    char buf[128];
    snprintf(buf, sizeof buf, "hello %s", s);
    (*env)->ReleaseStringUTFChars(env, name, s);
    return (*env)->NewStringUTF(env, buf);
}

JNIEXPORT jcharArray JNICALL Java_JniTest_stringChars(JNIEnv *env, jclass c, jstring s) {
    jsize n = (*env)->GetStringLength(env, s);
    const jchar *chars = (*env)->GetStringChars(env, s, NULL);
    if (chars == NULL) return NULL;
    jcharArray out = (*env)->NewCharArray(env, n);
    if (out != NULL) (*env)->SetCharArrayRegion(env, out, 0, n, chars);
    (*env)->ReleaseStringChars(env, s, chars);
    return out;
}

JNIEXPORT jbyteArray JNICALL Java_JniTest_stringBytes(JNIEnv *env, jclass c, jstring s) {
    jsize n = (*env)->GetStringUTFLength(env, s);
    const char *chars = (*env)->GetStringUTFChars(env, s, NULL);
    if (chars == NULL) return NULL;
    if (strlen(chars) != (size_t) n) {
        (*env)->ThrowNew(env, (*env)->FindClass(env, "java/lang/AssertionError"), "modified UTF-8 length");
        (*env)->ReleaseStringUTFChars(env, s, chars);
        return NULL;
    }
    jbyteArray out = (*env)->NewByteArray(env, n);
    if (out != NULL) (*env)->SetByteArrayRegion(env, out, 0, n, (const jbyte *) chars);
    (*env)->ReleaseStringUTFChars(env, s, chars);
    return out;
}

JNIEXPORT jstring JNICALL Java_JniTest_stringRoundTrip(JNIEnv *env, jclass c, jstring s) {
    const char *chars = (*env)->GetStringUTFChars(env, s, NULL);
    if (chars == NULL) return NULL;
    jstring out = (*env)->NewStringUTF(env, chars);
    (*env)->ReleaseStringUTFChars(env, s, chars);
    return out;
}

JNIEXPORT jcharArray JNICALL Java_JniTest_stringRegion(JNIEnv *env, jclass c, jstring s, jint start, jint len) {
    jsize n = (*env)->GetStringLength(env, s);
    /* Size from the source so invalid ranges can reach the JNI bounds check without a huge malloc. */
    jchar *chars = calloc((size_t) n + 1, sizeof(jchar));
    if (chars == NULL) return NULL;
    (*env)->GetStringRegion(env, s, start, len, chars);
    if ((*env)->ExceptionCheck(env)) { free(chars); return NULL; }
    jcharArray out = (*env)->NewCharArray(env, len);
    if (out != NULL) (*env)->SetCharArrayRegion(env, out, 0, len, chars);
    free(chars);
    return out;
}

JNIEXPORT jbyteArray JNICALL Java_JniTest_stringUtfRegion(JNIEnv *env, jclass c, jstring s, jint start, jint len) {
    jsize n = (*env)->GetStringLength(env, s);
    char *chars = calloc((size_t) n * 3 + 1, 1);
    if (chars == NULL) return NULL;
    (*env)->GetStringUTFRegion(env, s, start, len, chars);
    if ((*env)->ExceptionCheck(env)) { free(chars); return NULL; }
    jsize bytes = (jsize) strlen(chars);
    jbyteArray out = (*env)->NewByteArray(env, bytes);
    if (out != NULL) (*env)->SetByteArrayRegion(env, out, 0, bytes, (const jbyte *) chars);
    free(chars);
    return out;
}

/* No Java allocations in the loop: measures JNI getters rather than return-array construction. */
JNIEXPORT jlong JNICALL Java_JniTest_stringGetterLoop(JNIEnv *env, jclass c, jstring s, jint iterations, jint kind) {
    jlong sum = 0;
    for (jint i = 0; i < iterations; i++) {
        if (kind == 0) sum += (*env)->GetStringLength(env, s);
        else if (kind == 1) sum += (*env)->GetStringUTFLength(env, s);
        else if (kind == 2) {
            const jchar *chars = (*env)->GetStringChars(env, s, NULL);
            if (chars == NULL) return sum;
            sum += chars[0];
            (*env)->ReleaseStringChars(env, s, chars);
        } else {
            const char *chars = (*env)->GetStringUTFChars(env, s, NULL);
            if (chars == NULL) return sum;
            sum += (unsigned char) chars[0];
            (*env)->ReleaseStringUTFChars(env, s, chars);
        }
    }
    return sum;
}

JNIEXPORT jint JNICALL Java_JniTest_sum(JNIEnv *env, jclass c, jintArray a) {
    jsize n = (*env)->GetArrayLength(env, a);
    jint *e = (*env)->GetIntArrayElements(env, a, NULL);
    jint s = 0;
    for (jsize i = 0; i < n; i++) s += e[i];
    (*env)->ReleaseIntArrayElements(env, a, e, JNI_ABORT);
    return s;
}

JNIEXPORT void JNICALL Java_JniTest_fill(JNIEnv *env, jclass c, jbyteArray a, jbyte v) {
    jsize n = (*env)->GetArrayLength(env, a);
    jbyte buf[16];
    for (jsize i = 0; i < n; i++) buf[i] = v + i;
    (*env)->SetByteArrayRegion(env, a, 0, n, buf);
}

JNIEXPORT jobject JNICALL Java_JniTest_pass(JNIEnv *env, jclass c, jobject o) { return o; }

JNIEXPORT jint JNICALL Java_JniTest_bump(JNIEnv *env, jobject self, jint by) {
    jclass k = (*env)->GetObjectClass(env, self);
    jfieldID f = (*env)->GetFieldID(env, k, "count", "I");
    jint v = (*env)->GetIntField(env, self, f) + by;
    (*env)->SetIntField(env, self, f, v);
    return v;
}

JNIEXPORT void JNICALL Java_JniTest_callBack(JNIEnv *env, jclass c, jint n) {
    jmethodID m = (*env)->GetStaticMethodID(env, c, "onCall", "(ILjava/lang/String;)V");
    for (jint i = 1; i <= n; i++) {
        jstring s = (*env)->NewStringUTF(env, "from native");
        (*env)->CallStaticVoidMethod(env, c, m, i, s);
        (*env)->DeleteLocalRef(env, s);
    }
    jfieldID f = (*env)->GetStaticFieldID(env, c, "label", "Ljava/lang/String;");
    jstring l = (*env)->GetStaticObjectField(env, c, f);
    const char *s = (*env)->GetStringUTFChars(env, l, NULL);
    printf("label %s\n", s);
    fflush(stdout);
    (*env)->ReleaseStringUTFChars(env, l, s);
}

JNIEXPORT void JNICALL Java_JniTest_raise(JNIEnv *env, jclass c, jstring msg) {
    const char *s = (*env)->GetStringUTFChars(env, msg, NULL);
    jclass e = (*env)->FindClass(env, "java/lang/IllegalStateException");
    (*env)->ThrowNew(env, e, s);
    (*env)->ReleaseStringUTFChars(env, msg, s);
}

static jint triple(JNIEnv *env, jclass c, jint x) { return 3 * x; }

static jobject kept;

JNIEXPORT jstring JNICALL Java_JniTest_globalRoundTrip(JNIEnv *env, jclass c, jstring s) {
    kept = (*env)->NewGlobalRef(env, s);
    jstring r = (*env)->NewLocalRef(env, kept);
    (*env)->DeleteGlobalRef(env, kept);
    return r;
}

JNIEXPORT jintArray JNICALL Java_JniTest_makeArray(JNIEnv *env, jclass c, jint n) {
    jintArray a = (*env)->NewIntArray(env, n);
    jint buf[16];
    for (jint i = 0; i < n; i++) buf[i] = i * i;
    (*env)->SetIntArrayRegion(env, a, 0, n, buf);
    return a;
}

/* Threads the VM did not start, as a driver's callback thread is: each finds itself detached
 * (GetEnv gives JNI_EDETACHED and a null env), attaches (named, as a daemon, or without
 * arguments), calls into Java and detaches; the last one, a daemon, ends still attached. */
static JavaVM *jvm;
static jclass test_class;

static void foreign_body(int mode) {
    JNIEnv *env = (JNIEnv *) (void *) 0x8a00000000LL;
    jint rc = (*jvm)->GetEnv(jvm, (void **) &env, JNI_VERSION_1_8);
    jboolean detached = rc == JNI_EDETACHED && env == NULL;
    JavaVMAttachArgs args = {JNI_VERSION_1_8, mode == 0 ? "native-worker" : NULL, NULL};
    if (mode == 1 || mode == 3) rc = (*jvm)->AttachCurrentThreadAsDaemon(jvm, (void **) &env, &args);
    else rc = (*jvm)->AttachCurrentThread(jvm, (void **) &env, mode == 2 ? NULL : &args);
    if (rc != JNI_OK) return;
    JNIEnv *again = NULL;
    (*jvm)->GetEnv(jvm, (void **) &again, JNI_VERSION_1_8);
    jmethodID m = (*env)->GetStaticMethodID(env, test_class, "onForeign", "(IZZ)V");
    (*env)->CallStaticVoidMethod(env, test_class, m, mode, detached, again == env);
    if ((*env)->ExceptionCheck(env)) {
        (*env)->ExceptionDescribe(env);
        (*env)->ExceptionClear(env);
    }
    if (mode != 3) (*jvm)->DetachCurrentThread(jvm);
}

#ifdef _WIN32
static DWORD WINAPI foreign_main(LPVOID mode) { foreign_body((int) (intptr_t) mode); return 0; }
#else
static void *foreign_main(void *mode) { foreign_body((int) (intptr_t) mode); return NULL; }
#endif

JNIEXPORT void JNICALL Java_JniTest_foreignThreads(JNIEnv *env, jclass c) {
    (*env)->GetJavaVM(env, &jvm);
    test_class = (*env)->NewGlobalRef(env, c);
    for (intptr_t mode = 0; mode < 4; mode++) {
#ifdef _WIN32
        HANDLE h = CreateThread(NULL, 0, foreign_main, (LPVOID) mode, 0, NULL);
        WaitForSingleObject(h, INFINITE);
        CloseHandle(h);
#else
        pthread_t th;
        pthread_create(&th, NULL, foreign_main, (void *) mode);
        pthread_join(th, NULL);
#endif
    }
}

JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM *vm, void *reserved) {
    JNIEnv *env;
    if ((*vm)->GetEnv(vm, (void **) &env, JNI_VERSION_1_8) != JNI_OK) return JNI_ERR;
    jclass c = (*env)->FindClass(env, "JniTest");
    JNINativeMethod m[] = {{"registered", "(I)I", (void *) triple}};
    if ((*env)->RegisterNatives(env, c, m, 1) != 0) return JNI_ERR;
    return JNI_VERSION_1_8;
}
