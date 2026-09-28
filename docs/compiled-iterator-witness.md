# Compiled iterator source checks

Expanded AOT coverage exposed a compiled-call shortcut that advanced synthetic
iterators without checking their ExpSize/Source witness. Disabling only
RAVA_NO_JIT_ITER_SHORTCUT's shortcut made IterSem pass and removed GuestListSem's
concurrent-modification mismatch on the preceding frozen release.

The normal and compiled paths now share iterator_source_changed. The shortcut
checks it after hasNext/exhaustion handling and before consuming an element.
An invalid witness falls through to the native throwing path with the cursor
unchanged. The existing size-based witness contract is preserved; this change
does not claim to implement a new modification-counter model.

The regression failed on the old code (next returned Some(0) instead of refusing
the changed source) and passes after the fix. It verifies ordinary next remains
served, hasNext remains nonthrowing, and both refusal and native exception leave
the position unchanged. Logs: `%TEMP%/rava-iterator-witness-before.log` and
`%TEMP%/rava-iterator-witness-after.log`.

Release IterSem and GuestListSem now match Java with expanded prepared constants
and the shortcut enabled. Evidence:
`%TEMP%/rava-string-boundary-probes-252923412da1484fbbe360c2a14f1e69`.
The same release also passes the three string-related differential probes.
Full application validation and wall measurements are pending.
