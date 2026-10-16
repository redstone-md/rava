# Minecraft

rava runs Minecraft servers and the Minecraft client the same way `java` does. Point whatever
starts the game at `rava` instead of `java`. Nothing in the VM knows about Minecraft, and it
runs the game like any other program (see the [rules](../README.md#rules)).

## Which versions

rava bundles the Java 21 class library, so it runs the versions that run on Java 21:

| Minecraft | Java it needs | rava |
|---|---|---|
| 1.20.5 – 1.21.x | 21 | runs: vanilla, Fabric, NeoForge |
| 26.x | 25 | not yet: needs the Java 25 class library |
| 1.17 – 1.20.4 | 17 | untested; these versions also run on Java 21 with HotSpot |
| 1.12.2 and older (LaunchWrapper) | 8 | does not run |

## Servers

```sh
rava -Xmx4G -jar paper.jar --nogui
```

Any start script works once you replace `java` with `rava`. That includes `@user_jvm_args.txt`
argument files (NeoForge, Forge) and `JAVA_TOOL_OPTIONS`. On Linux, netty's native epoll
transport loads as it does with HotSpot.

Tested so far: Paper 1.21.1 starts and serves (its cold start to `Done` is the main
benchmark), and NeoForge 1.21.1 with the All The Mods 10 pack starts and ticks.

## Client

Launchers ask which Java to use. Give them rava's executable from the unpacked archive:

| System | Executable |
|---|---|
| Linux, macOS | `rava-<version>-<system>/bin/rava` |
| Windows | `rava-<version>-windows-x86_64\bin\rava.exe` |

In Prism Launcher, set *Settings → Java → Java path* for every instance, or *Edit instance →
Settings → Java installation* for one. Prism runs the executable to check its version and
reports `Java is version 21.0.x … from rava`. Prism's compatibility check refuses an instance
that asks for Java 25, which is correct (see the table above).

Other launchers (the official one, MultiMC, ATLauncher) have a setting for the Java executable
or "Java path". Set it to rava's executable there.

Pass memory and JVM arguments as usual (`-Xmx4G`, `-XX:...`). rava accepts and ignores options
it does not use, the way HotSpot ignores options for other collectors.

### Graphics

The client draws through the system's OpenGL driver, as it does with HotSpot. Where a Vulkan
driver is installed, rava can ask Mesa to run OpenGL on Vulkan (Zink):

```sh
rava -Drava.opengl=vulkan ...      # or RAVA_OPENGL=vulkan in the environment
```

In Prism Launcher, put `-Drava.opengl=vulkan` in the instance's JVM arguments. On Linux this
uses the system's Mesa. Windows has no Mesa, so the Windows archive carries one in `lib/mesa`
(from [mesa-dist-win](https://github.com/pal1000/mesa-dist-win)); `RAVA_MESA=<dir>` points
rava at another copy. The game's F3 screen then names the renderer as `zink Vulkan ...`.

## When something goes wrong

A crash report or an exception in the log names what failed. An `UnsatisfiedLinkError` for a
`java.*`, `sun.*` or `jdk.*` method is a native that rava does not implement yet. Please report
it.

For a hang (the window stops responding, the log stops), get a thread dump as described in
[troubleshooting.md](troubleshooting.md). With Prism, set the environment variable
`RAVA_DUMP_EVERY=30` in the instance's settings and reproduce the hang.

Open reports in [Issues](https://github.com/redstone-md/rava/issues) and attach the full log.
