# JIT ThreadLocal storage consistency

2026-09-06. Reviewing retained startup logs exposed an IllegalStateException
immediately after Done: `Profiler is already active`. The logger printed the
message template but omitted its Throwable argument, hiding the actual reason.
Previous time-to-Done measurements do not establish post-startup service health.

Cause: native leaf dispatch can call Vm::ws_threadlocal on the main thread.
That function used tl_worker storage for get/set, while ordinary native remove
cleared the object's $tlset field. A later compiled get returned the old value
from the worker table. Minecraft Profiler.startUsing rejects that stale value
after Profiler.stopUsing calls ThreadLocal.remove.

Fix: ws_threadlocal now requires parallel::gilfree(). Other calls decline to the
existing native implementation, keeping get/set/remove on the same storage.
The worker implementation and region-local table lifecycle are unchanged.

Evidence root: `%TEMP%/rava-post-done-0e6bb58252c24201b9028dc83613c7eb`.
The pointer is `%TEMP%/rava-post-done-current.txt`. The directory contains the temporary
logger patch, diagnostic/fix build logs, frozen binaries and probe results.

The temporary logger diagnostic identified the exception in run
`rava-product-boot-9868fcba37814476b0f520b8d6676b42`. It was removed before the
normal release fix was built. An authlib network error was also logged, but the
fatal exception was `Profiler is already active`.

Control executable SHA256:
`659A37D667531B6B0CE7C4F7C6AFC0E349FBAE8D9AB15DE49991D8E6CEF194C6`.
Fixed normal-release SHA256:
`05ADCBE8983DE95EED4E0275E336AAE44B92C4E0C4070D4807730AA11D1CDB6C`.

The 10,000-iteration set/get/remove/get reproducer prints stale=9984 on control
and stale=0 on the fixed binary and Java -Xint. Synchronous JIT, threshold 16 and
inline budget 0 retain and exercise compiled call sites; they are diagnostic
coverage controls, not performance tuning. The permanent ThreadLocalRemoveSem
also passes the existing differential harness with no excluded divergence.

Fixed fresh-world startup first survived a two-second post-Done window (run
`rava-product-boot-c0aa9ea014d84b1199f23b4d8eaab916`). A second run,
`rava-product-boot-80084b74a67346aebaa461f74a387f1f`, records alive=true and
crash=false in post-done.json after ten seconds, before the harness kills its
owned process. Both loaded 1470 recipes and 1584 advancements.

These runs do not establish long-term server health, working online authentication
or a speed gain. Their startup times were 26.698 and 27.330 s, with the diagnostic
marker enabled but diagnostic logger code absent. No paired performance claim
is made. The below-five-second/no-adapt/no-snapshot goal remains unmet.
