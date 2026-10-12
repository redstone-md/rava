# Platforms roadmap: Windows and macOS

rava was written for Linux on x86-64. This is the plan for Windows and macOS: what ties the VM
to Linux today, and the order in which it comes apart. The rules stay: one VM, nothing keyed to
a program, the `java` command line unchanged.

## What is Linux-specific

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
