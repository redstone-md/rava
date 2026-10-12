# GUI roadmap: the Minecraft client on rava

Goal: rava runs desktop Java applications as `java` does, tested on the Minecraft client
(vanilla and every mod loader), and renders OpenGL through Vulkan where that pays.

Rules stay as for the server: rava is program-agnostic (nothing keyed to Minecraft; a
Minecraft-specific change is a patch), no snapshots, the `java` command line unchanged.

## Steps

1. **JNI** (`src/jni/`). The client's natives (LWJGL: GLFW, OpenGL, OpenAL, STB) are shared
   libraries reached through the Java Native Interface, which every JVM provides. rava
   supplies the JDK's own natives in Rust and loaded no other library until now.
   - `NativeLibraries.load` opens libraries with `dlopen` and calls `JNI_OnLoad`.
   - A native method without a VM body binds to `Java_<class>_<name>[__<signature>]` in a
     loaded library, or to what `RegisterNatives` gave it.
   - The call runs with the thread in the native state (the collector proceeds meanwhile),
     through a generic System V call: arguments from the interpreter's slots into integer and
     vector registers, the rest on the stack.
   - `jobject` is the object's address. References handed to native code are roots the
     collector does not move (local references live in the thread's `handles`, popped when
     the native returns; global references in a VM table), and arguments sit in value-stack
     slots, which are scanned conservatively and pinned likewise.
   - `JNIEnv` lives inside `Thread`, so an environment pointer finds its thread. Its functions
     switch the thread back to the running state for their duration. Variadic `Call*Method`
     functions are assembly stubs that build a `va_list` over the saved argument registers
     and the caller's stack and call the `V` variants, which decode it by the method's
     signature.
   - Tested by `tests/jni/` (a C library exercising each part, compared with HotSpot).
   - Done: the above, with the functions a C library commonly uses (classes, exceptions,
     references, calls of every kind, fields, strings, arrays, direct buffers,
     `RegisterNatives`). A function not provided yet stops the process naming its index.
     With it, Paper and NeoForge servers run netty's native epoll transport instead of the
     NIO fallback.
   - Call cost as HotSpot's (10-13 ns for a static `add(int, int)`, from 160 ns at first): a
     native bound to its library function gets a generated entry stub for its signature
     (`jni/stub.rs`: frame, arguments straight into the C registers, native state by plain
     stores, local references dropped only when the call made any), and the collector issues
     the store-load barrier for every thread with `membarrier` (HotSpot's
     `UseSystemMemoryBarrier`) so the return from native code needs no fence.
2. **LWJGL**: the functions LWJGL 3 uses (direct buffers, critical arrays, `JavaVM` attach for
   callbacks from native threads); an LWJGL GLFW + OpenGL demo under Xvfb with Mesa.
3. **The client**: Minecraft 1.21 (vanilla, Fabric, NeoForge) to the title screen and into a
   singleplayer world under Xvfb (software OpenGL here: function, not frame rate), then the
   client's render thread profiled as the server's tick was.
   - Done for vanilla 1.21.1 (`bench/mc_client.py`, offline account, a world made by the
     vanilla server on rava, `--quickPlaySingleplayer`): title screen, world, F3 screen. Under
     Xvfb with llvmpipe both VMs draw 10-11 frames per second and the integrated server ticks
     in 10 ms: the software renderer is the limit here. An LWJGL GLFW + OpenGL demo renders
     as on HotSpot.
   - Done for Fabric (loader 0.19.5 with Fabric API and Sodium, 55 mods) and NeoForge 21.1.259
     with Sodium (`--loader`, `--mods`): both reach the world with Sodium's renderer, NeoForge's
     early loading window included. Two optimizing-compiler failures the client showed were
     fixed on the way (a branch to the next instruction pruned both ways).
4. **Vulkan**: for any OpenGL application, rava can ask Mesa for its GL-on-Vulkan driver
   (Zink) when a Vulkan driver is present. Translation alone does not raise frame rates
   (Minecraft's frame time is mostly the CPU's); it becomes the default only where
   measurements on real GPUs show a gain.
   - Done as an option: `-Drava.opengl=vulkan` (or `RAVA_OPENGL=vulkan`) sets Mesa's driver to
     Zink and Mesa as the GLVND vendor before anything loads `libGL`, when a Vulkan driver
     manifest is installed (`launcher/graphics.rs`); variables already set win. The vanilla
     client renders through Zink on Mesa's software Vulkan driver (lavapipe) under Xvfb at the
     same 8-9 frames per second as llvmpipe's OpenGL. Xvfb has no DRI3 and lavapipe is a CPU
     device, so there it also takes `LIBGL_KOPPER_DRI2=1 LIBGL_ALWAYS_SOFTWARE=1`; a real
     display and GPU need neither. The default stays the system driver until GPU measurements.
5. **Java 8** (1.12.2 with Forge's LaunchWrapper): the Java 8 class library's contract with the
   VM (no modules, its own natives and `Unsafe`), after the modern client works.
