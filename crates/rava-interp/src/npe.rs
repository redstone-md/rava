//! The JVM's HELPFUL `NullPointerException` message, reproduced.
//!
//! This engine raised `java/lang/String.length` where the reference implementation raises
//! `Cannot invoke "String.length()" because "<local0>" is null`. That text is guest-visible
//! and this jar logs it, so a wrong message is a wrong answer — just a quiet one.
//!
//! ## The format was MEASURED, not recalled
//!
//! `bench/NpeMsg.java` asks `java -Xint` one shape per line, compiled without `-g` (the jar
//! this engine runs ships without a LocalVariableTable, and the JVM falls back to
//! `<localN>` exactly then). Two rules came out of it that no amount of reasoning would
//! have produced:
//!
//! * **Only `java/lang/String` and `java/lang/Object` are printed unqualified.**
//!   `java.lang.Integer`, `java.lang.StringBuilder`, `java.lang.Thread` and
//!   `java.lang.CharSequence` all keep their package, so the rule is NOT "strip java.lang".
//! * Parameters follow the same rule, comma-space separated, primitives by keyword:
//!   `java.util.Map.put(Object, Object)`, `java.util.List.get(int)`.
//!
//! ## What is covered
//!
//! Every shape the probe exercises: invoke, field read, field assign, array load, array
//! store, array length, and `throw null`. The "because" clause names the null's source when
//! a bounded backward walk can identify it (`<localN>`, `Owner.field`, the return value of a
//! call) and is OMITTED when it cannot, which is the honest failure: HotSpot runs a full
//! dataflow simulation over the method to answer that question, and a guessed source is
//! worse than none in the one place a person reads to find a bug.

use rava_classfile::ConstantPool;

/// A class name as HotSpot's helpful NPE prints it.
///
/// `java/lang/String` → `String`, `java/lang/Object` → `Object`, everything else fully
/// qualified with dots. The two exceptions are not a package rule — see the module note.
pub(crate) fn ext_class(internal: &str) -> String {
    match internal {
        "java/lang/String" => "String".to_string(),
        "java/lang/Object" => "Object".to_string(),
        other => other.replace('/', "."),
    }
}

/// One field descriptor as a source-level type name.
fn ext_type(desc: &str) -> String {
    let (dims, rest) = {
        let n = desc.bytes().take_while(|b| *b == b'[').count();
        (n, &desc[n..])
    };
    let base = match rest.as_bytes().first() {
        Some(b'B') => "byte".to_string(),
        Some(b'C') => "char".to_string(),
        Some(b'D') => "double".to_string(),
        Some(b'F') => "float".to_string(),
        Some(b'I') => "int".to_string(),
        Some(b'J') => "long".to_string(),
        Some(b'S') => "short".to_string(),
        Some(b'Z') => "boolean".to_string(),
        Some(b'V') => "void".to_string(),
        Some(b'L') => ext_class(rest[1..].trim_end_matches(';')),
        _ => rest.to_string(),
    };
    format!("{base}{}", "[]".repeat(dims))
}

/// A method descriptor's PARAMETER list, rendered as the JVM renders it.
fn ext_params(desc: &str) -> String {
    let Some(close) = desc.find(')') else { return String::new() };
    let mut out: Vec<String> = Vec::new();
    let bytes = desc.as_bytes();
    let mut i = 1usize;
    while i < close {
        let start = i;
        while i < close && bytes[i] == b'[' {
            i += 1;
        }
        if i < close && bytes[i] == b'L' {
            while i < close && bytes[i] != b';' {
                i += 1;
            }
            i += 1;
        } else {
            i += 1;
        }
        out.push(ext_type(&desc[start..i.min(close)]));
    }
    out.join(", ")
}

/// The element-type word in `Cannot load from int array`.
fn array_kind(elem: &str) -> &'static str {
    match elem {
        "byte" | "[B" | "B" => "byte",
        "char" | "[C" | "C" => "char",
        "double" | "[D" | "D" => "double",
        "float" | "[F" | "F" => "float",
        "int" | "[I" | "I" => "int",
        "long" | "[J" | "J" => "long",
        "short" | "[S" | "S" => "short",
        "boolean" | "[Z" | "Z" => "boolean",
        _ => "object",
    }
}

/// The element type of an array access, from the OPCODE — the only source available, since
/// a null array carries no type. `iaload`/`iastore` say int, `aaload`/`aastore` say object.
pub(crate) fn array_kind_for_op(op: u8) -> &'static str {
    match op {
        0x2e | 0x4f => "int",
        0x2f | 0x50 => "long",
        0x30 | 0x51 => "float",
        0x31 | 0x52 => "double",
        0x32 | 0x53 => "object",
        0x33 | 0x54 => "byte",
        0x34 | 0x55 => "char",
        0x35 | 0x56 => "short",
        _ => "object",
    }
}

fn because(src: Option<String>) -> String {
    match src {
        Some(s) => format!(" because {s} is null"),
        None => String::new(),
    }
}

pub(crate) fn invoke(owner: &str, name: &str, desc: &str, src: Option<String>) -> String {
    format!(
        "Cannot invoke \"{}.{}({})\"{}",
        ext_class(owner),
        name,
        ext_params(desc),
        because(src)
    )
}

pub(crate) fn read_field(name: &str, src: Option<String>) -> String {
    format!("Cannot read field \"{name}\"{}", because(src))
}

pub(crate) fn assign_field(name: &str, src: Option<String>) -> String {
    format!("Cannot assign field \"{name}\"{}", because(src))
}

pub(crate) fn array_load(elem: &str, src: Option<String>) -> String {
    format!("Cannot load from {} array{}", array_kind(elem), because(src))
}

pub(crate) fn array_store(elem: &str, src: Option<String>) -> String {
    format!("Cannot store to {} array{}", array_kind(elem), because(src))
}

pub(crate) fn array_length(src: Option<String>) -> String {
    format!("Cannot read the array length{}", because(src))
}

pub(crate) fn monitor_null(src: Option<String>) -> String {
    format!("Cannot enter synchronized block{}", because(src))
}

pub(crate) fn throw_null() -> String {
    "Cannot throw exception because \"null\" is null".to_string()
}

/// NAME THE NULL, by walking back over instruction boundaries to whichever instruction
/// pushed the operand `depth` slots below the top of the stack.
///
/// HotSpot answers this with a full dataflow simulation of the method. This is the bounded
/// version: it recognises the instructions that push exactly one operand and nothing else,
/// and gives up the moment it meets anything it cannot account for — because a MISCOUNTED
/// stack would name the wrong expression, and a message that confidently names the wrong
/// variable is worse than one that names none.
///
/// `depth` is how many operands sit ABOVE the null one: 0 for `arraylength`, 1 for
/// `iaload` (the index is on top), and the argument count for an invoke.
pub(crate) fn source_at(code: &[u8], pc: usize, depth: usize, pool: &ConstantPool) -> Option<String> {
    // Instruction boundaries are only knowable forwards — the encoding is variable length —
    // so walk from the start once and keep the offsets. Methods are short and a boot builds
    // ten exceptions, so this costs nothing anyone can measure.
    let mut bounds: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while i < pc {
        bounds.push(i);
        i += crate::jit::insn_len_at(code, i)?;
    }
    let k = producer(code, &bounds, bounds.len(), depth, pool)?;
    describe(code, &bounds, k, pool)
}

/// The stack effect of the instructions this walk understands, as `(pushes, pops)`.
///
/// `None` ends the walk. That is the point: stepping backwards over an instruction moves the
/// slot being tracked by `-pushes + pops`, and getting that arithmetic wrong does not fail
/// loudly — it names a DIFFERENT variable, confidently, in the one place a person reads to
/// find a bug.
fn effect(code: &[u8], at: usize, pool: &ConstantPool) -> Option<(usize, usize)> {
    Some(match code[at] {
        0x01..=0x11 => (1, 0), // aconst_null / *const_* / bipush / sipush
        0x12..=0x14 => (1, 0), // ldc / ldc_w / ldc2_w
        0x15..=0x2d => (1, 0), // *load n / *load_0..3
        0x2e..=0x35 => (1, 2), // *aload — array, index
        0x36..=0x3a => (0, 1), // *store n
        0x3b..=0x4e => (0, 1), // *store_0..3
        0x4f..=0x56 => (0, 3), // *astore — array, index, value
        0x57 => (0, 1),        // pop
        0x58 => (0, 2),        // pop2
        0x59 => (2, 1),        // dup — both copies name the value beneath
        0xbe => (1, 1),        // arraylength
        0xbb => (1, 0),        // new
        0xb2 => (1, 0),        // getstatic
        0xb4 => (1, 1),        // getfield
        0xc0 | 0xc1 => (1, 1), // checkcast / instanceof
        0xb6..=0xb9 => {
            let idx = u16::from_be_bytes([code[at + 1], code[at + 2]]);
            let m = pool.member_ref(idx)?;
            let recv = usize::from(code[at] != 0xb8);
            (usize::from(!m.descriptor.ends_with(")V")), crate::count_args(m.descriptor) + recv)
        }
        _ => return None,
    })
}

/// WHICH instruction produced the operand `depth` slots below the top of the stack at
/// `bounds[upto]`, as an index into `bounds`.
fn producer(
    code: &[u8],
    bounds: &[usize],
    upto: usize,
    mut depth: usize,
    pool: &ConstantPool,
) -> Option<usize> {
    for k in (0..upto).rev() {
        let (p, q) = effect(code, bounds[k], pool)?;
        if depth < p {
            return Some(k);
        }
        depth = depth - p + q;
    }
    None
}

/// NAME the value an instruction produces, in the JVM's own vocabulary.
///
/// Recursive for an array load, because that is how the JVM renders a chained access:
/// `x[0][0]` reports its null as `"<local0>[0]"`. Everything it cannot name honestly
/// returns `None`, and the caller then omits the "because" clause entirely rather than
/// guessing.
fn describe(code: &[u8], bounds: &[usize], k: usize, pool: &ConstantPool) -> Option<String> {
    let at = bounds[k];
    match code[at] {
        0x01 => Some("\"null\"".to_string()),
        0x19 => Some(format!("\"<local{}>\"", code.get(at + 1).copied().unwrap_or(0))),
        0x2a..=0x2d => Some(format!("\"<local{}>\"", code[at] - 0x2a)),
        0xb2 => {
            let idx = u16::from_be_bytes([code[at + 1], code[at + 2]]);
            let m = pool.member_ref(idx)?;
            Some(format!("\"{}.{}\"", ext_class(m.owner), m.name))
        }
        0xb6..=0xb9 => {
            let idx = u16::from_be_bytes([code[at + 1], code[at + 2]]);
            let m = pool.member_ref(idx)?;
            Some(format!(
                "the return value of \"{}.{}({})\"",
                ext_class(m.owner),
                m.name,
                ext_params(m.descriptor)
            ))
        }
        // `dup` names whatever it duplicated, which is why `synchronized (o)` — compiled as
        // `aload; dup; astore; monitorenter` — still reports the local rather than nothing.
        0x59 => {
            let src = producer(code, bounds, k, 0, pool)?;
            describe(code, bounds, src, pool)
        }
        // An array element: the array is one slot below the index at this point, so both
        // operands are located with the same search this function was reached by.
        0x2e..=0x35 => {
            let arr = producer(code, bounds, k, 1, pool)?;
            let idx = producer(code, bounds, k, 0, pool)?;
            let arr_txt = describe(code, bounds, arr, pool)?;
            let idx_txt = const_int(code, bounds[idx])?;
            // The array's own rendering is quoted; the JVM folds the subscript INSIDE those
            // quotes, so the closing quote is reopened rather than appended.
            Some(format!("{}[{idx_txt}]\"", arr_txt.trim_end_matches('"')))
        }
        _ => None,
    }
}

/// The integer an instruction pushes, when it is a constant — the only form the JVM renders
/// inside a subscript.
fn const_int(code: &[u8], at: usize) -> Option<String> {
    Some(match code[at] {
        0x02..=0x08 => (i32::from(code[at]) - 0x03).to_string(), // iconst_m1..iconst_5
        0x10 => (code[at + 1] as i8).to_string(),                // bipush
        0x11 => i16::from_be_bytes([code[at + 1], code[at + 2]]).to_string(), // sipush
        _ => return None,
    })
}
