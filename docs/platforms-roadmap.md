# Platforms: Linux, macOS and Windows

rava was written for Linux on x86-64 and now also runs on macOS and Windows (x86-64). This page
records what tied the VM to Linux, the order in which it came apart, and where each port
stands ([Status](#status)). The rules stayed: one VM, nothing keyed to a program, the `java`
command line unchanged.

## What was Linux-specific

| Area | Linux today | macOS | Windows |
|---|---|---|---|
| Address space | `mmap` with `MAP_NORESERVE`, `MAP_FIXED_NOREPLACE`, `MAP_32BIT`; `madvise` (huge pages, `DONTNEED`, `POPULATE_WRITE`) | `mmap` without those flags; the low 4 GiB is `__PAGEZERO` unless the binary is linked with a small one | `VirtualAlloc` reserve and commit, `VirtualFree`, `DiscardVirtualMemory` |
| Low memory (classes and methods named by 32-bit immediates; the 32-bit class word) | `MAP_32BIT` | hinted `mmap` below 2 GiB, with `-pagezero_size` | `VirtualAlloc` at chosen low addresses |
| Implicit null checks | `SIGSEGV` handler rewriting `ucontext` RIP | `SIGSEGV`/`SIGBUS`, `__darwin_mcontext64` | vectored exception handler, `CONTEXT.Rip` |
| Safepoint barrier | `membarrier` | (fence on return from native) | `FlushProcessWriteBuffers` |
| Native calls (JNI, stubs) | System V ABI | System V ABI | Win64 ABI: four register arguments, 32 bytes of shadow space, other callee-saved registers |
| Libraries | `dlopen` | `dlopen` | `LoadLibraryW`, `GetProcAddress` |
| Thread stacks | `pthread_getattr_np` | `pthread_get_stackaddr_np` | `GetCurrentThreadStackLimits` |
| JDK natives (`natives/`) | Linux class library: epoll, inotify, `/proc`, mount table | BSD class library: kqueue, `MacOSXWatchService` polling, `getmntinfo`, `BsdNativeDispatcher` | Windows class library: IOCP and `WindowsSelectorImpl`, `WinNTFileSystem`, `WindowsNativeDispatcher`, registry preferences, `ProcessImpl` |
| Distribution | `bin/rava`, `lib/modules` | the same layout, `lib/modules` from a macOS JDK | `bin\rava.exe`, `lib\modules` from a Windows JDK |

## Order

1. **Platform layer** (`src/os/`): address space, faults, threads, libraries, process facts behind
   one interface, Linux first, with no change in behavior. The JDK natives specific to Linux are
   compiled only there.
2. **macOS on x86-64**: the layer for macOS, the BSD class library's natives, a CI job and a
   release archive. Apple Silicon Macs run it through Rosetta 2 until step 5.
3. **Windows on x86-64**: the layer for Windows, the Win64 ABI in JNI calls, the Windows class
   library's natives, tested under Wine here and on Windows runners in CI; a release zip.
4. **Release matrix**: Linux, macOS and Windows archives from one tag.
5. **AArch64**: a second backend for both compiler tiers, for Apple Silicon and Linux on ARM.

## Status

- **Platform layer**: done (`src/os/`: `unix.rs` for Linux and macOS, `windows.rs`). The heap
  reserves its range uncommitted and commits each page as allocation reaches it (a no-op on
  Unix).
- **macOS x86-64**: done. The binary links with a 4 KiB `__PAGEZERO`; the heap takes the first
  free range from 4 GiB. Natives of the BSD class library: `KQueue`, `BsdNativeDispatcher`, path
  normalization, `F_FULLFSYNC`, socket options; `NetworkInterface` from `getifaddrs`. CI runs the
  semantic probes, the JNI test and the patches test on an Intel Mac, with the same four
  red-by-design probes as Linux.
- **Windows x86-64**: the JDK boots and runs programs; tested here with a MinGW build under Wine
  and in CI on Windows runners. Done: platform properties, `java.io` over handles,
  `WinNTFileSystem`, `WindowsNativeDispatcher` (files and security), file channels, sockets over
  Winsock behind the shared socket layer (`natives/sock`), the `WEPoll` selector over `WSAPoll`, asynchronous channels over completion ports,
  `NetworkInterface` from `GetAdaptersAddresses`, child processes, and JNI in the Win64
  convention (native calls, variadic `Call*Method`, `va_list`). Not yet: the generated JNI call
  stubs (natives take the generic path), `ProcessHandle.Info`, AWT. Under Wine, 124 of 132
  semantic probes match HotSpot's Linux output; the others are the four helpful-NPE probes and
  output that differs by design on Windows (the ANSI code page, path syntax, interfaces).
- **Release matrix**: the release workflow builds and checks a tar.gz for Linux and macOS and a
  zip for Windows and publishes them together.
