//! Tiny method-descriptor reader: counts argument values so the interpreter
//! knows how many operands a call consumes. One slot per argument in the
//! current value model (see `value.rs`).

/// Number of argument values in a method descriptor like `(ILjava/lang/String;)V`.
pub fn arg_count(descriptor: &str) -> usize {
    let bytes = descriptor.as_bytes();
    let mut i = match descriptor.find('(') {
        Some(p) => p + 1,
        None => return 0,
    };
    let mut count = 0;
    while i < bytes.len() && bytes[i] != b')' {
        match bytes[i] {
            b'[' => {
                i += 1; // array marker binds to the element type that follows
                continue;
            }
            b'L' => {
                // Object type runs to the next ';'.
                while i < bytes.len() && bytes[i] != b';' {
                    i += 1;
                }
                i += 1;
            }
            _ => i += 1, // primitive: single char
        }
        count += 1;
    }
    count
}

/// Number of LOCAL SLOTS the arguments occupy (JVMS 2.6.1): every argument takes one slot except
/// a bare `J`/`D`, which take two. An ARRAY is always one slot however wide its element type is -
/// `[J` and `[D` are narrow references, a distinction this tree has already been bitten by once
/// (see `has_wide_params` and the freeze-table wrong-answer bug).
///
/// `arg_count` answers "how many arguments"; this answers "how much room do they need", which is
/// what a caller building the callee's locals has to reserve.
pub fn arg_slots(descriptor: &str) -> usize {
    let bytes = descriptor.as_bytes();
    let mut i = match descriptor.find('(') {
        Some(p) => p + 1,
        None => return 0,
    };
    let mut slots = 0;
    while i < bytes.len() && bytes[i] != b')' {
        let is_array = bytes[i] == b'[';
        while i < bytes.len() && bytes[i] == b'[' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let kind = bytes[i];
        if kind == b'L' {
            while i < bytes.len() && bytes[i] != b';' {
                i += 1;
            }
        }
        i += 1;
        slots += if !is_array && matches!(kind, b'J' | b'D') { 2 } else { 1 };
    }
    slots
}

/// Whether the descriptor returns a value (return type is not `V`).
/// Part of the descriptor API; used by call sites as opcode coverage grows.
#[allow(dead_code)]
pub fn has_return(descriptor: &str) -> bool {
    match descriptor.rfind(')') {
        Some(p) => descriptor.as_bytes().get(p + 1) != Some(&b'V'),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arg_slots_counts_wide_params_but_not_wide_arrays() {
        assert_eq!(arg_slots("()V"), 0);
        assert_eq!(arg_slots("(II)I"), 2);
        assert_eq!(arg_slots("(J)V"), 2);
        assert_eq!(arg_slots("(ID)V"), 3);
        assert_eq!(arg_slots("(Ljava/lang/Object;I)Z"), 2);
        // The trap: long/double ARRAYS are narrow references, one slot each.
        assert_eq!(arg_slots("([J[D)V"), 2);
        assert_eq!(arg_slots("([[I)V"), 1);
        // The ring's real noise callee, four doubles after three ints.
        assert_eq!(arg_slots("(IIIDDDD)D"), 11);
    }

    #[test]
    fn counts_mixed_args() {
        assert_eq!(arg_count("(ILjava/lang/String;[IJ)V"), 4);
        assert_eq!(arg_count("()V"), 0);
        assert_eq!(arg_count("([Ljava/lang/String;)V"), 1);
    }

    #[test]
    fn detects_return() {
        assert!(has_return("()I"));
        assert!(!has_return("(I)V"));
        assert!(has_return("()Ljava/lang/String;"));
    }
}

// ⭐**`memchr` is 5-6% of every time slice of the datapack-reload phase, and the caller
// attribution names these two lines.** `bench/boot_pc.ps1 -Stacks memchr` reports the VERIFIED
// chain
//
//   core::str::pattern::next_match_back < core::str::iter::SplitInternal::next_back<char>
//       < rava_interp::vmstate::Vm::jit_enter < rava_interp::vmstate::Vm::jit_run
//
// — `descriptor.rsplit(')').next()`, a reverse SUBSTRING SEARCH run on every entry into a
// compiled method that returns a reference, in a function entered 8,545,386 times per boot. Its
// companion `descriptor.find('(')` is a forward one on the same path. Both answers are
// constants of the method.
//
// `str::rsplit`/`str::find` on a `char` go through `Pattern`, which sets up `memchr` — the profile
// even shows `drop_in_place<memchr::cow::CowBytes>` beneath it. On a descriptor tail of a dozen
// bytes that setup costs more than the comparison it vectorises, so these are plain byte loops.
// Both delimiters are ASCII, so the boundaries produced are the same char boundaries `str` gives.
//
// Equivalence is exact including the edge cases the call sites relied on:
//   * `rsplit(')').next().unwrap_or("")` on a descriptor with NO `)` yields the WHOLE string
//     (rsplit always emits one piece), never `""` — so the fallback here is the whole string;
//   * `find('(').map(|p| p + 1).unwrap_or(0)` yields 0 when there is no `(`.

/// Kill switch (`RAVA_NO_FAST_DESC=1`) — routes both helpers back through `str::pattern` so a
/// same-binary A/B can price the change. Equivalence is by construction; this exists to MEASURE.
fn desc_slow() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_FAST_DESC").is_some())
}

/// The return-type substring: everything after the LAST `)`, or the whole descriptor if absent.
#[inline]
pub(crate) fn ret_desc(descriptor: &str) -> &str {
    if desc_slow() {
        return descriptor.rsplit(')').next().unwrap_or("");
    }
    let b = descriptor.as_bytes();
    let mut i = b.len();
    while i > 0 {
        i -= 1;
        if b[i] == b')' {
            return &descriptor[i + 1..];
        }
    }
    descriptor
}

/// Index just past the `(` that opens the parameter list; 0 when there is none.
#[inline]
pub(crate) fn params_start(descriptor: &str) -> usize {
    if desc_slow() {
        return descriptor.find('(').map(|p| p + 1).unwrap_or(0);
    }
    let b = descriptor.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'(' {
            return i + 1;
        }
        i += 1;
    }
    0
}
