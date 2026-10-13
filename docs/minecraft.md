# Minecraft

rava runs Minecraft servers and the Minecraft client the way `java` does: point whatever starts
the game at `rava` instead of `java`. Nothing in the VM knows about Minecraft; it runs the game
as any other program (see the [rules](../README.md#rules)).

## Which versions

The bundled class library is Java 21's, so rava runs what runs on Java 21:

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

Any start script works once `java` is replaced by `rava`, including `@user_jvm_args.txt`
argument files (NeoForge, Forge) and `JAVA_TOOL_OPTIONS`. On Linux, netty's native epoll
transport loads as with HotSpot.

What has been run: Paper 1.21.1 (starts and serves; the cold start to `Done` is the main
benchmark), NeoForge 1.21.1 with the All The Mods 10 pack (starts and ticks).

## Client

Launchers ask which Java to use; give them rava's executable from the unpacked archive:

| System | Executable |
|---|---|
| Linux, macOS | `rava-<version>-<system>/bin/rava` |
| Windows | `rava-<version>-windows-x86_64\bin\rava.exe` |

**Prism Launcher:** *Settings → Java → Java path* for every instance, or *Edit instance →
Settings → Java installation* for one. Prism runs the executable to check its version and
reports `Java is version 21.0.x … from rava`. An instance that asks for Java 25 is refused
by Prism's compatibility check, rightly: see the table above.

**Other launchers** (the official one, MultiMC, ATLauncher): wherever they let you choose the
Java executable or "Java path".

Memory and JVM arguments are passed as usual (`-Xmx4G`, `-XX:...`); options rava does not use
are accepted and ignored, as HotSpot ignores options of other collectors.

### Graphics

The client draws through the system's OpenGL driver, as with HotSpot. On Linux, rava can ask
Mesa to run OpenGL on Vulkan (Zink) where a Vulkan driver is installed:

```sh
rava -Drava.opengl=vulkan ...      # or RAVA_OPENGL=vulkan in the environment
```

## When something goes wrong

- A crash report or an exception in the log names what failed; an `UnsatisfiedLinkError` for a
  `java.*`, `sun.*` or `jdk.*` method is a native rava does not implement yet. Please report it.
- A hang (the window stops responding, the log stops): get a thread dump, as described in
  [troubleshooting.md](troubleshooting.md). With Prism, set the environment variable
  `RAVA_DUMP_EVERY=30` in the instance's settings and reproduce the hang.

Reports go to [Issues](https://github.com/redstone-md/rava/issues), with the full log.
