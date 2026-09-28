// Gate for the composite-comparator key cache (RAVA_SORT_KEY_MEMO).
//
// Reproduces the datapack sort shape without any Minecraft class: a
// `Comparator.comparingLong(...).thenComparingLong(...)...` chain of C clauses over
// N elements, exactly the shape the registry/biome-tree build sorts with. Each clause
// extractor counts its own invocations, so the run reports BOTH the sorted order (which
// must not change) and how many extractor applies the sort actually paid for (which is
// the thing the memo removes).
//
// Two data sets, because the whole point is that `thenComparing` short-circuits:
//   DISTINCT  — clause 0 decides every comparison; clauses 1..C-1 are never asked for.
//   TIED      — clause 0 ties in blocks, so later clauses ARE reached and must be right.
//
// Usage: SortKeyMemo [n] [clauses]
public class SortKeyMemo {
    static final int SLOTS = 8;
    static long[] calls = new long[SLOTS];

    public static void main(String[] args) {
        int n = args.length > 0 ? Integer.parseInt(args[0]) : 1024;
        int clauses = args.length > 1 ? Integer.parseInt(args[1]) : 7;
        run("DISTINCT", build(n, clauses, 0), clauses);
        run("TIED", build(n, clauses, 8), clauses);
    }

    /// `tieBlock` = how many consecutive elements share a clause-0 key (0 = all distinct).
    static long[][] build(int n, int clauses, int tieBlock) {
        long[][] rows = new long[n][clauses + 1];
        long seed = 0x9E3779B97F4A7C15L;
        for (int i = 0; i < n; i++) {
            for (int c = 0; c < clauses; c++) {
                seed = seed * 6364136223846793005L + 1442695040888963407L;
                rows[i][c] = (seed >>> 24) % 1000L;
            }
            if (tieBlock > 0) {
                rows[i][0] = (long) (i / tieBlock);
            }
            rows[i][clauses] = i; // stable identity, never a sort key
        }
        return rows;
    }

    static void run(String label, long[][] rows, int clauses) {
        java.util.List<long[]> list = new java.util.ArrayList<long[]>();
        for (long[] r : rows) {
            list.add(r);
        }
        for (int i = 0; i < SLOTS; i++) {
            calls[i] = 0;
        }
        java.util.Comparator<long[]> cmp = java.util.Comparator.comparingLong(keyfn(0));
        for (int c = 1; c < clauses; c++) {
            cmp = cmp.thenComparingLong(keyfn(c));
        }
        list.sort(cmp);

        long order = 1125899906842597L;
        for (long[] r : list) {
            order = order * 31L + r[clauses];
        }
        long total = 0;
        StringBuilder per = new StringBuilder();
        for (int c = 0; c < clauses; c++) {
            total += calls[c];
            per.append(calls[c]).append(c + 1 < clauses ? "," : "");
        }
        System.out.println(label + " n=" + rows.length + " clauses=" + clauses
            + " ORDER=" + Long.toHexString(order)
            + " APPLIES=" + total + " PER_CLAUSE=" + per);
        // Ordering invariant, checked in-guest so a wrong order fails loudly rather than
        // only differing from a recorded hash.
        for (int i = 1; i < list.size(); i++) {
            if (raw(list.get(i - 1), list.get(i), clauses) > 0) {
                throw new IllegalStateException(label + ": out of order at " + i);
            }
        }
    }

    static java.util.function.ToLongFunction<long[]> keyfn(final int c) {
        return row -> {
            calls[c]++;
            return row[c];
        };
    }

    /// The comparator's contract, evaluated WITHOUT the plan — the order oracle.
    static int raw(long[] a, long[] b, int clauses) {
        for (int c = 0; c < clauses; c++) {
            if (a[c] != b[c]) {
                return a[c] < b[c] ? -1 : 1;
            }
        }
        return 0;
    }
}
