# Patches

A patch is a jar a server owner drops into `rava-patches/` next to the server (the working
directory rava starts in). At startup rava reads every patch there; while the program runs, it
edits the classes a patch targets as they are defined and loads the patch's own classes.
Nothing else is needed: no command-line flag, no agent, no change to the server's start
script. Without a `rava-patches/` directory nothing changes.

rava itself stays program-agnostic (no-adapt, no-snapshot): it knows nothing of what a patch
targets. A patch is the owner's choice of third-party code, like a mod, and its effect is the
patch's own; results with patches installed are reported separately from VM comparisons.

## Installing

```
server/
  rava-patches/
    parallel-levels.jar
  server.jar ...
```

A patch is a `.jar` or an unpacked directory with the same layout. rava prints one line per
patch at startup (`[rava] patch parallel-levels 1 from rava-patches/parallel-levels.jar`)
and one line per edited class. To turn patches off, remove them, or start with
`RAVA_PATCHES=0`; `RAVA_PATCHES=dir` reads them from another directory.

## Format

A patch holds a descriptor, `rava-patch.txt`, at its root, and class files in the usual
layout (`my/patch/Hooks.class`). The descriptor has one directive per line; `#` starts a
comment.

```
id parallel-levels        # required, unique among installed patches
version 1                 # required
open <class> [member]
redirect <class>.<method><desc> <owner>.<name><desc> -> <hook-class>.<hook-method>
```

Class names are internal names (`net/minecraft/server/MinecraftServer`), descriptors JVM
descriptors (`(Ljava/util/function/BooleanSupplier;)V`).

- **`open C`** makes class `C` public and not final; **`open C m`** does the same for every
  field and method of `C` named `m`. Patch code can then use them directly.
- **`redirect C.m(D) O.n(E) -> H.h`** replaces, inside the method `C.m(D)` only, every call
  of `O.n(E)` by a call of the static method `H.h`. For an instance call, `h` takes the
  receiver first: `static R h(O receiver, E...)`; for a static call it has `n`'s descriptor.
  The hook may call the original method itself. Constructor calls cannot be redirected.

Edits are applied when a class loader other than the boot loader defines the class (JDK
classes are never edited). All edits of one class apply or none do: if a method, member or
call site is missing (another version of the program), rava prints which edit has no target
and leaves the class unchanged. Edits never change code length, so stack maps and exception
tables stay valid and the edited class verifies and compiles as any other.

A patch's own classes are defined in the class loader of the code that first names them,
which is the loader of the edited class: they see the program's classes as that class does.
Put them in a package of their own (`dev/rava/patches/<id>/`).

## Writing a patch

1. Find the call sites to hook. `RAVA_PATCH_DUMP=dir` writes every targeted class to `dir`
   before (`Name.orig.class`) and after (`Name.class`) its edits; `javap -c -p` shows them.
   A descriptor with only an `open C` line dumps `C` without changing its behaviour.
2. Write the hooks as static methods and compile them against the program's jars (or stubs
   of the members they use): `javac -cp <program jars> -d out Hooks.java`.
3. Add `rava-patch.txt` to `out` and `jar cf my-patch.jar -C out .`.

`tests/patches/` is a complete small example (and the test of the mechanism):
`tests/patches/run.sh` builds a program and a patch with every kind of edit and checks the
output.

## Limits

- Only calls can be redirected, and only inside a named method; methods cannot be replaced
  whole and fields cannot be added.
- Hidden classes (lambdas, `defineHiddenClass`) are not edited.
- A patch's classes are visible by name only to loaders that resolve them from bytecode;
  `Class.forName` through another loader does not find them.
