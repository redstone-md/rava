use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;

const RECEIVER_KINDS: [&str; 8] = [
    "null",
    "arena",
    "pinned",
    "handle",
    "array",
    "box",
    "array_box",
    "other",
];

static READS: [AtomicU64; RECEIVER_KINDS.len()] =
    [const { AtomicU64::new(0) }; RECEIVER_KINDS.len()];
static WRITES: [AtomicU64; RECEIVER_KINDS.len()] =
    [const { AtomicU64::new(0) }; RECEIVER_KINDS.len()];

static SLOT_PROBE_SITES: AtomicU64 = AtomicU64::new(0);
static SLOT_PROBE_HITS: AtomicU64 = AtomicU64::new(0);
static SLOT_PROBE_MISSES: AtomicU64 = AtomicU64::new(0);

pub(crate) fn field_probe_census_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_FIELD_PROBE_CENSUS").is_some())
}

pub(crate) fn field_probe_note_site() {
    SLOT_PROBE_SITES.fetch_add(1, Relaxed);
}

pub(crate) fn field_probe_counter_ptr(hit: bool) -> i64 {
    let counter = if hit {
        &SLOT_PROBE_HITS
    } else {
        &SLOT_PROBE_MISSES
    };
    counter.as_ptr() as usize as i64
}

pub(crate) fn field_probe_census_report() -> String {
    if !field_probe_census_enabled() {
        return String::new();
    }
    format!(
        "[FIELD-PROBE] sites={} hit={} miss={}",
        SLOT_PROBE_SITES.load(Relaxed),
        SLOT_PROBE_HITS.load(Relaxed),
        SLOT_PROBE_MISSES.load(Relaxed)
    )
}

pub(crate) fn field_repr_census_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_FIELD_REPR_CENSUS").is_some())
}

fn receiver_kind(cell: i64) -> usize {
    if cell == 0 {
        return 0;
    }

    let high = (cell as u64) >> 32;
    if high == (crate::value::REF_TAG as u64) >> 32 {
        return 3;
    }
    if high == (crate::value::ARRAY_TAG as u64) >> 32 {
        return 4;
    }
    if high == (crate::value::BOX_TAG as u64) >> 32 {
        return 5;
    }
    if high == (crate::value::ABOX_TAG as u64) >> 32 {
        return 6;
    }

    let address = cell as u64;
    if crate::nextgen::pinned_handle_of(address).is_some() {
        2
    } else if crate::nextgen::is_arena_obj_addr(address) {
        1
    } else {
        7
    }
}

fn note(counters: &[AtomicU64; RECEIVER_KINDS.len()], cell: i64) {
    counters[receiver_kind(cell)].fetch_add(1, Relaxed);
}

pub(crate) extern "C" fn rava_getfield_census(obj: i64, field_id: i64, is_ref: i64) -> i64 {
    note(&READS, obj);
    super::rava_getfield(obj, field_id, is_ref)
}

pub(crate) extern "C" fn rava_putfield_census(
    obj: i64,
    field_id: i64,
    value: i64,
    kind: i64,
) -> i64 {
    note(&WRITES, obj);
    super::rava_putfield(obj, field_id, value, kind)
}

pub(crate) fn field_repr_census_report() -> String {
    if !field_repr_census_enabled() {
        return String::new();
    }

    let reads = std::array::from_fn::<_, 8, _>(|i| READS[i].load(Relaxed));
    let writes = std::array::from_fn::<_, 8, _>(|i| WRITES[i].load(Relaxed));
    let render = |values: &[u64; 8]| {
        RECEIVER_KINDS
            .iter()
            .zip(values)
            .map(|(name, count)| format!("{name}={count}"))
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        "[FIELD-REPR] getfield={} putfield={}",
        render(&reads),
        render(&writes)
    )
}
