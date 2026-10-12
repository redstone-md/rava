#include <jni.h>
#include <string.h>
#include <stdio.h>

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

JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM *vm, void *reserved) {
    JNIEnv *env;
    if ((*vm)->GetEnv(vm, (void **) &env, JNI_VERSION_1_8) != JNI_OK) return JNI_ERR;
    jclass c = (*env)->FindClass(env, "JniTest");
    JNINativeMethod m[] = {{"registered", "(I)I", (void *) triple}};
    if ((*env)->RegisterNatives(env, c, m, 1) != 0) return JNI_ERR;
    return JNI_VERSION_1_8;
}
