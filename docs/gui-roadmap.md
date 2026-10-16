# GUI roadmap: the Minecraft client on rava

Goal: rava runs desktop Java applications the way `java` does, tested on the Minecraft client
(vanilla and every mod loader), and renders OpenGL through Vulkan where that pays off.

The server rules still apply. rava is program-agnostic (nothing keyed to Minecraft; a
Minecraft-specific change is a patch), uses no snapshots, and takes the `java` command line
unchanged.

## Steps

1. **JNI** (`src/jni/`). The client's natives (LWJGL: GLFW, OpenGL, OpenAL, STB) are shared
   libraries reached through the Java Native Interface, which every JVM provides. rava
   implements the JDK's own natives in Rust, and until this step it loaded no other library.
   - `NativeLibraries.load` opens libraries with `dlopen` and calls `JNI_OnLoad`.
   - A native method without a VM body binds to `Java_<class>_<name>[__<signature>]` in a
     loaded library, or to whatever `RegisterNatives` gave it.
   - The call runs with the thread in the native state, so the collector can proceed
     meanwhile. It goes through a generic System V call: arguments move from the
     interpreter's slots into integer and vector registers, and the rest go on the stack.
   - `jobject` is the object's address. References handed to native code are roots the
     collector does not move. Local references live in the thread's `handles` and are popped
     when the native returns; global references live in a VM table. Arguments sit in
     value-stack slots, which are scanned conservatively and pinned the same way.
   - `JNIEnv` lives inside `Thread`, so an environment pointer finds its thread. Its functions
     switch the thread back to the running state while they run. The variadic `Call*Method`
     functions are assembly stubs: they build a `va_list` over the saved argument registers
     and the caller's stack and call the `V` variants, which decode it by the method's
     signature.
   - Tested by `tests/jni/`, a C library that exercises each part, compared with HotSpot.
   - Done: all of the above, with the functions a C library commonly uses (classes,
     exceptions, references, calls of every kind, fields, strings, arrays, direct buffers,
     `RegisterNatives`). Calling a function that is not provided yet stops the process and
     names its index. With JNI in place, Paper and NeoForge servers run netty's native epoll
     transport instead of the NIO fallback.
   - Call cost matches HotSpot's: 10-13 ns for a static `add(int, int)`, down from 160 ns at
     first. A native bound to its library function gets a generated entry stub for its
     signature (`jni/stub.rs`: frame, arguments straight into the C registers, native state
     by plain stores, local references dropped only when the call made any). The collector
     issues the store-load barrier for every thread with `membarrier` (HotSpot's
     `UseSystemMemoryBarrier`), so the return from native code needs no fence.
2. **LWJGL**: the functions LWJGL 3 uses (direct buffers, critical arrays, `JavaVM` attach for
   callbacks from native threads), checked with an LWJGL GLFW + OpenGL demo under Xvfb with
   Mesa.
3. **The client**: Minecraft 1.21 (vanilla, Fabric, NeoForge) to the title screen and into a
   singleplayer world under Xvfb (software OpenGL here, so this tests function, not frame
   rate). After that, profile the client's render thread the way the server's tick was
   profiled.
   - Done for vanilla 1.21.1 (`bench/mc_client.py`, offline account, a world made by the
     vanilla server on rava, `--quickPlaySingleplayer`): title screen, world, F3 screen. Under
     Xvfb with llvmpipe both VMs draw 10-11 frames per second and the integrated server ticks
     in 10 ms, so the software renderer is the limit here. An LWJGL GLFW + OpenGL demo renders
     the same as on HotSpot.
   - Done for Fabric (loader 0.19.5 with Fabric API and Sodium, 55 mods) and NeoForge 21.1.259
     with Sodium (`--loader`, `--mods`). Both reach the world with Sodium's renderer, including
     NeoForge's early loading window. Two optimizing-compiler bugs the client exposed were
     fixed along the way (a branch to the next instruction was pruned both ways).
4. **Vulkan**: for any OpenGL application, rava can ask Mesa for its GL-on-Vulkan driver
   (Zink) when a Vulkan driver is present. Translation alone does not raise frame rates
   (Minecraft's frame time is mostly CPU time), so it becomes the default only where
   measurements on real GPUs show a gain.
   - Done as an option. `-Drava.opengl=vulkan` (or `RAVA_OPENGL=vulkan`) sets Mesa's driver to
     Zink and Mesa as the GLVND vendor before anything loads `libGL`, when a Vulkan driver
     manifest is installed (`launcher/graphics.rs`). Variables the user already set take
     precedence. The vanilla client renders through Zink on Mesa's software Vulkan driver
     (lavapipe) under Xvfb at the same 8-9 frames per second as llvmpipe's OpenGL. Xvfb has no
     DRI3 and lavapipe is a CPU device, so that setup also needs
     `LIBGL_KOPPER_DRI2=1 LIBGL_ALWAYS_SOFTWARE=1`; a real display and GPU need neither. The
     default stays the system driver until there are GPU measurements.
   - Windows: there is no system Mesa, so the Windows archive carries mesa-dist-win's
     `opengl32.dll` and `libgallium_wgl.dll` in `lib/mesa` (`dist/make.sh` fetches a pinned
     release and checks its digest; licenses in `legal/mesa`). With the option rava sets
     `GALLIUM_DRIVER=zink` and loads that `opengl32.dll` by full path before the program
     starts, so GLFW's and LWJGL's later `LoadLibrary("opengl32.dll")` get Mesa's module. An
     LWJGL test reports `zink Vulkan 1.4 (NVIDIA GeForce RTX 5070)`, OpenGL 4.6.
5. **Java 8** (1.12.2 with Forge's LaunchWrapper): the Java 8 class library's contract with the
   VM (no modules, its own natives and `Unsafe`). This comes after the modern client works.
