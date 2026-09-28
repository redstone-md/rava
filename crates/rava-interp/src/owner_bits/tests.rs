//! Owner-bit tests.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::owner_bits::*;

#[cfg(test)]
mod tests {
    use super::*;

    /// The two arms must agree for every needle — that is the whole safety argument for the
    /// flag, and it is the one thing a unit test can settle without a boot.
    #[test]
    fn memo_and_scan_agree() {
        let names = [
            "java/util/ArrayList",
            "java/util/BitSet",
            "java/util/concurrent/atomic/AtomicInteger",
            "java/util/concurrent/atomic/AtomicIntegerFieldUpdater",
            "java/util/concurrent/atomic/LongAdder",
            "java/lang/management/RuntimeMXBean",
            "java/util/concurrent/locks/ReentrantLock",
            "net/minecraft/world/level/block/state/BlockState",
            "com/google/common/collect/RegularImmutableMap",
            "it/unimi/dsi/fastutil/doubles/DoubleArrayList",
            "",
        ];
        for n in names {
            let scanned = compute(n);
            for i in 0..NEEDLES.len() {
                assert_eq!(
                    scanned & (1 << i) != 0,
                    n.contains(NEEDLES[i]),
                    "bit {i} ({}) disagrees for {n}",
                    NEEDLES[i]
                );
            }
        }
    }

    #[test]
    fn collection_mask_covers_exactly_the_collection_needles() {
        assert_eq!(COLLECTION_ANY, LIST | SET | QUEUE | DEQUE | COLLECTION | VECTOR | STACK);
        assert_eq!(compute("java/util/ArrayList") & COLLECTION_ANY, LIST);
        assert_eq!(compute("java/util/ArrayDeque") & COLLECTION_ANY, DEQUE);
        assert_eq!(compute("java/lang/String") & COLLECTION_ANY, 0);
    }

    /// `contains_any` must give the same answer whichever arm serves it — the flag is a
    /// performance switch, and a divergence here would be a silent dispatch-routing change.
    #[test]
    fn masked_query_matches_the_direct_scan() {
        let names = [
            "java/util/concurrent/ConcurrentLinkedQueue",
            "java/util/Stack",
            "java/util/concurrent/atomic/AtomicReferenceArray",
            "net/minecraft/core/Registry",
            "io/netty/util/concurrent/SingleThreadEventExecutor",
        ];
        let masks = [COLLECTION_ANY, MXBEAN, ATOMIC, EXECUTOR | MAILBOX, LOCK, LIST | STACK];
        for n in names {
            for m in masks {
                let direct = NEEDLES
                    .iter()
                    .enumerate()
                    .any(|(i, k)| m & (1 << i) != 0 && n.contains(k));
                assert_eq!(direct, compute(n) & m != 0, "mask {m:#x} disagrees for {n}");
            }
        }
    }

    /// The needle bits must not collide with the namespace bit — `DISPATCH_ANY`
    /// masks over both, so an overlap would make `may_serve` answer a needle
    /// question with a prefix answer.
    #[test]
    fn namespace_bit_is_above_every_needle() {
        assert!(NEEDLES.len() < 31, "needle bits would reach the namespace bit");
        assert_eq!(NAMESPACE & ((1u32 << NEEDLES.len()) - 1), 0);
    }

    /// `may_serve` is a SUPERSET claim: it must be true for every class-shaped
    /// literal the gauntlet keys an arm on. The literals below are the namespace
    /// representatives extracted from natives/nio/net/stream/reflect/map.
    #[test]
    fn may_serve_admits_every_dispatcher_namespace() {
        for n in [
            "java/lang/Object",
            "java/util/HashMap",
            "java/io/BufferedReader",
            "java/nio/file/Files",
            "java/net/URI",
            "java/time/Instant",
            "java/security/MessageDigest",
            "java/math/BigDecimal",
            "java/text/DecimalFormat",
            "java/awt/GraphicsEnvironment",
            "javax/crypto/KeyGenerator",
            "javax/net/ssl/HttpsURLConnection",
            "jdk/jfr/Event",
            "jdk/internal/misc/Unsafe",
            "sun/misc/Unsafe",
            "com/google/common/collect/MapMaker",
            "com/google/gson/internal/LazilyParsedNumber",
            "io/netty/channel/epoll/Native",
            "it/unimi/dsi/fastutil/longs/Long2ObjectOpenHashMap",
        ] {
            assert!(may_serve(n), "{n} must reach the dispatch gauntlet");
            assert!(may_serve_scan(n), "{n} must reach the dispatch gauntlet");
        }
    }

    /// The non-literal arms that must reach the dispatch gauntlet; unknown custom wrappers do not.
    #[test]
    fn may_serve_admits_the_substring_arms() {
        for n in [
            "net/minecraft/core/NonNullList", // stream::is_collection
            "net/minecraft/world/level/ContextKeySet", // stream::is_collection
            "com/mojang/serialization/DataFixerError", // natives throwable arm
            "net/minecraft/ReportedException", // natives throwable arm
            "net/minecraft/util/thread/ConsecutiveExecutor", // reflect executor arm
            "some/pkg/CustomEventLoop", // reflect executor arm
            "some/pkg/CustomScheduler", // reflect executor arm
            "some/pkg/CustomCondition", // reflect locks arm
            "some/pkg/CustomSemaphore", // reflect locks arm
            "some/pkg/WrappedByteBuffer", // natives bytebuffer arm
        ] {
            assert!(may_serve(n), "{n} must reach the dispatch gauntlet");
            assert!(may_serve_scan(n), "{n} must reach the dispatch gauntlet");
        }
    }

    #[test]
    fn buffered_input_bit_is_jdk_exact() {
        assert!(contains_any("java/io/BufferedInputStream", BUFFERED_INPUT));
        let custom = "net/minecraft/util/FastBufferedInputStream";
        assert!(!contains_any(custom, BUFFERED_INPUT));
        assert!(!may_serve(custom));
        assert!(!may_serve_scan(custom));
    }

    /// …and a plain jar class with none of those marks is the case the gate exists
    /// for. This is the only assertion that says "skip", so it is the only one that
    /// can be wrong in the dangerous direction.
    #[test]
    fn may_serve_rejects_a_plain_jar_class() {
        for n in [
            "net/minecraft/world/level/levelgen/NoiseChunk",
            "com/mojang/serialization/codecs/RecordCodecBuilder",
            "net/minecraft/core/Holder$Reference",
            "org/slf4j/LoggerFactory",
        ] {
            assert!(!may_serve(n), "{n} should not need the gauntlet");
            assert!(!may_serve_scan(n), "{n} should not need the gauntlet");
        }
    }

    /// Every stage predicate must admit the owner literals its dispatcher matches
    /// on. These are the arm literals extracted from each dispatcher's `match
    /// owner`; a new arm outside its stage's declared namespaces fails here (and,
    /// on a real workload, under RAVA_DISPATCH_GATE_VERIFY).
    #[test]
    fn stage_predicates_admit_their_dispatchers_arms() {
        for (bit, owners) in [
            (
                S_NIO,
                &[
                    "java/nio/file/Files",
                    "java/nio/channels/FileChannel",
                    "java/io/RandomAccessFile",
                    "java/io/File",
                    "java/util/zip/GZIPOutputStream",
                    "java/security/DigestOutputStream",
                    "io/netty/buffer/ByteBufOutputStream",
                ][..],
            ),
            (
                S_NET,
                &[
                    "io/netty/channel/nio/NioEventLoop",
                    "java/nio/ByteOrder",
                    "java/net/ServerSocket",
                    "java/nio/channels/SocketChannel",
                ][..],
            ),
            (
                S_STREAM,
                &[
                    "java/util/stream/Stream",
                    "java/util/Arrays",
                    "java/util/Collections",
                    "java/util/Optional",
                    "java/io/BufferedReader",
                    "com/google/common/collect/Sets",
                    "net/minecraft/core/NonNullList",
                ][..],
            ),
            (
                S_REFLECT,
                &[
                    "java/lang/Class",
                    "java/lang/invoke/MethodHandles",
                    "java/util/Comparator",
                    "java/net/URI",
                    "java/nio/charset/Charset",
                    "javax/net/ssl/HttpsURLConnection",
                    "sun/reflect/Reflection",
                    "jdk/internal/reflect/Reflection",
                    "com/google/common/util/concurrent/SettableFuture",
                    "java/util/concurrent/atomic/AtomicInteger",
                ][..],
            ),
            (
                S_MAP,
                &[
                    "java/util/HashMap",
                    "java/util/Map$Entry",
                    "java/util/concurrent/ConcurrentHashMap",
                    "com/google/common/collect/MapMaker",
                    "it/unimi/dsi/fastutil/objects/Reference2ObjectArrayMap",
                ][..],
            ),
            (
                S_TIME,
                &[
                    "java/time/Instant",
                    "java/time/format/DateTimeFormatter",
                    "java/text/SimpleDateFormat",
                    "java/util/Calendar",
                    "java/util/GregorianCalendar",
                    "java/util/Date",
                ][..],
            ),
        ] {
            for o in owners {
                assert!(compute(o) & bit != 0, "stage {bit:#x} must admit {o}");
            }
        }
    }

    /// The plan must actually EXCLUDE stages for the owners the census named — a
    /// plan with every bit set is correct but buys nothing.
    #[test]
    fn plan_excludes_the_stages_a_hot_owner_cannot_reach() {
        // `java/lang/Boolean#booleanValue` and `StringBuilder#append` walked all
        // five sub-dispatchers to reach an arm in `dispatch_owner`'s own tail.
        for o in ["java/lang/Boolean", "java/lang/StringBuilder", "java/lang/Math"] {
            let p = compute(o);
            assert_eq!(p & (S_NIO | S_NET | S_STREAM | S_MAP | S_TIME), 0, "{o}");
        }
        // A fastutil map reaches map:: only.
        let p = compute("it/unimi/dsi/fastutil/objects/Reference2ObjectArrayMap");
        assert_eq!(p & (S_NIO | S_NET | S_STREAM | S_REFLECT | S_TIME), 0);
        assert_ne!(p & S_MAP, 0);
        // java/lang/Class reaches reflect:: only.
        let p = compute("java/lang/Class");
        assert_eq!(p & (S_NIO | S_NET | S_STREAM | S_MAP | S_TIME), 0);
        assert_ne!(p & S_REFLECT, 0);
    }

    /// Memo and scan must agree — the gate's flag is a performance switch, and a
    /// divergence would be a silent change to which natives get served.
    #[test]
    fn may_serve_memo_matches_the_scan() {
        for n in [
            "java/lang/String",
            "net/minecraft/world/item/ItemStack",
            "it/unimi/dsi/fastutil/objects/Object2ObjectMaps",
            "io/netty/util/concurrent/SingleThreadEventExecutor",
            "",
        ] {
            assert_eq!(compute(n) & DISPATCH_ANY != 0, may_serve_scan(n), "disagree for {n}");
        }
    }
}
