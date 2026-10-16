# Java releases

rava runs the Java class library of a given release: its own copy, shipped beside it by
`dist/make.sh`, or a JDK's for a development build. Supporting a release means honoring that
library's contract with the VM. The VM itself does not change between releases.

| Release | Status |
|---|---|
| 21 (LTS) | tested: semantics (all but the four red-by-design probes, helpful NPE texts), Paper, NeoForge |
| 25 (LTS) | tested: semantics as on 21, Paper |
| 22-24 | runs through the same paths (the protocols below switch at their releases), untested |
| < 21 | refused at launch |

## Layers

1. Core (`interp/`, `jit/`, `gc/`, `heap.rs`, `classfile.rs`): bytecode, class files, the
   object model and the collector. Class files up to the newest supported release parse here.
   Nothing in the core asks which release is running.
2. Library protocol (`jdk.rs`): every difference between releases in what the VM and the
   class library expect of each other. It is decided once, from the library that was found
   (`release`'s `JAVA_VERSION`). Code elsewhere asks `crate::jdk::library()` instead of
   probing classes: `class_frame_info()` for the stack-walking protocol, `optional_field()`
   for fields that a release added and the VM fills in.
3. Natives (`natives/`): registered by class, name and descriptor. A method whose signature a
   release changed has one entry per signature, each noting the release it belongs to.
4. Launcher (`launcher/`): the `java` command line and the launcher protocol of
   `sun.launcher.LauncherHelper`.

## Differences handled

| Contract | 21 | 25 |
|---|---|---|
| `Class` state | natives (`getModifiers`, `isPrimitive`, `getProtectionDomain`) | fields `modifiers`, `primitive`, `protectionDomain` the VM fills in (`runtime/mirror.rs`) |
| `SystemProps.Raw` | 39 platform properties | 40 (`native_encoding`, `stdin_encoding`); both laid out by the class's own `_*_NDX` constants (`natives/props.rs`) |
| launcher `main` kind | `LauncherHelper.getMainType()` | statics `isStaticMain`, `noArgMain` (`launcher/mod.rs`) |
| stack walking | `StackFrameInfo.memberName` initialized by the VM; batches by end index | `ClassFrameInfo.classOrMemberName` + `flags`, `expandStackFrameInfo`; batches by frame count (`natives/stackwalk.rs`) |
| native libraries | `NativeLibraries.load` returns true | also expects `NativeLibraryImpl.handle` set (an opaque non-zero one; the VM supplies the natives) |
| `FileKey.init` | instance, sets `st_dev`/`st_ino` | static, fills a `long[2]` |
| natives added | | `CDS.getCDSConfigStatus`, `PhantomReference.clear0`, `Thread.sleepNanos0`, `UnixFileDispatcherImpl.available0` |

## Adding a release

Run `tests/semantics/run.sh` with `JAVA_HOME` set to that release's JDK. The reference output
comes from its `java`. Each `UnsatisfiedLinkError` names a native to register. Each protocol
change gets a predicate in `jdk.rs` and a row in the table above.
