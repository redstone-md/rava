# What the 958 MB is

`java -Xint` boots this server in **351 MB**. rava needs **958 MB at peak**, and every slicing
axis of the CPU profile has come back flat — top function 4.3%, top category 11.4%, top phase's
own top 4.8%, hottest single instruction inside `run_hot` 4.07%. A uniform 2.7x resident set is
the one candidate that would tax every axis equally, so the first job is to find out what the
memory IS, not to guess.

`RAVA_CENSUS=1` now prints the decomposition at every new peak:

```
os_rss=958MB  malloc_live=533MB  arena_committed=328MB  unaccounted=97MB
```

## Two heaps, both full

The arena — the new object representation — holds **328 MB**. The legacy handle heap holds, by
its own census, **344 MB**:

```
live_objects=1105361  field_slots=2309316  live_arrays=808360  live_maps=7385
   67.9MB 19.7%  array payload
   53.4MB 15.5%  field blob (1 alloc/obj)
   42.6MB 12.4%  array obj table
   33.7MB  9.8%  obj.struct (Object)
   33.7MB  9.8%  obj.desc_table
   21.3MB  6.2%  array desc table
   16.9MB  4.9%  obj.box_tax (malloc)
   16.9MB  4.9%  obj.table (Vec<Box>)
   16.0MB  4.7%  array_box side
  344.0MB        TOTAL (Rust-side)
```

So `malloc_live` (533) minus the census (344) leaves **~189 MB of engine metadata outside both
heaps** — class files, constant pools, JIT code and its side tables, the name-keyed caches.

## The ratio, which is the actual finding

1,105,361 objects carry 2,309,316 field slots. At `sizeof(Value)=16B` that is **37 MB of data**.
The bookkeeping around it — field blob + obj.struct + desc table + box tax + pointer table —
is **154.6 MB**.

**Four bytes of metadata for every byte of object data.** A HotSpot object header is 12-16 bytes;
ours is ~140. That is not a profile line anyone can attack — it is the shape of the
representation, and it is the same conclusion the profile reached from the other side (two object
representations, `Value = Ref | Obj` on every field, native ABI copying a `Value` at every
boundary). It is now a number rather than an inference.

Arrays say it even more plainly: **80 MB of spine and descriptors for 68 MB of payload**, and
86.3% of live arrays hold 8 elements or fewer.

## Slack that is merely waste

- `array obj table`: 1,394,385 slots for 808,360 live arrays — **42% dead slots**.
- free-handle vectors retain **8.6 MB** (arr = 586,025 live of 1,198,515 capacity).
- map entry vectors: 296,160 capacity for 216,489 entries — **2.4 MB slack**.

Tens of MB, i.e. memory and not time. Recorded so nobody re-derives it.

## The diagnostic used to abort the boot it measured

Both census flags killed a product boot before this was measurable:

```
thread panicked at nextgen/tlab.rs: RefCell already borrowed
panic in a function that cannot unwind
```

`arena_committed_bytes` and `tlab_in_nursery` both took the TLAB borrow, and every caller worth
having runs INSIDE a collection that already holds it. Both now read lock-free sources — the
published extent table for committed bytes, and a `try_with_tlab` with a last-known-extent
fallback for the nursery test. A diagnostic that can abort the process is worse than no
diagnostic; that is why the fix is in the engine and not in the harness.
