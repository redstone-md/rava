#![cfg(all(feature = "next-gen-gc", target_os = "linux", target_arch = "x86_64"))]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("rava-prepared-aot-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn u16be(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn utf8(out: &mut Vec<u8>, value: &str) {
    out.push(1);
    u16be(out, value.len() as u16);
    out.extend_from_slice(value.as_bytes());
}

fn class(index: u16, out: &mut Vec<u8>) {
    out.extend_from_slice(&[7, (index >> 8) as u8, index as u8]);
}

fn member(out: &mut Vec<u8>, name: u16, descriptor: u16, stack: u16, locals: u16, code: &[u8]) {
    u16be(out, 0x0009); // public static
    u16be(out, name);
    u16be(out, descriptor);
    u16be(out, 1); // Code attribute
    u16be(out, 7);
    out.extend_from_slice(&(12u32 + code.len() as u32).to_be_bytes());
    u16be(out, stack);
    u16be(out, locals);
    out.extend_from_slice(&(code.len() as u32).to_be_bytes());
    out.extend_from_slice(code);
    u16be(out, 0); // exception handlers
    u16be(out, 0); // nested code attributes
}

// main() exercises the primitive-store callback and invokes probe() -> helper() via rava_invoke.
fn probe_class() -> Vec<u8> {
    let mut out = vec![0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52];
    u16be(&mut out, 15); // constant_pool_count
    utf8(&mut out, "AotProbe"); // #1
    class(1, &mut out); // #2
    utf8(&mut out, "java/lang/Object"); // #3
    class(3, &mut out); // #4
    utf8(&mut out, "main"); // #5
    utf8(&mut out, "([Ljava/lang/String;)V"); // #6
    utf8(&mut out, "Code"); // #7
    utf8(&mut out, "probe"); // #8
    utf8(&mut out, "()I"); // #9
    utf8(&mut out, "helper"); // #10
    out.extend_from_slice(&[12, 0, 8, 0, 9]); // #11 NameAndType probe:()I
    out.extend_from_slice(&[10, 0, 2, 0, 11]); // #12 Methodref AotProbe.probe
    out.extend_from_slice(&[12, 0, 10, 0, 9]); // #13 NameAndType helper:()I
    out.extend_from_slice(&[10, 0, 2, 0, 13]); // #14 Methodref AotProbe.helper

    u16be(&mut out, 0x0021); // public + super
    u16be(&mut out, 2); // this_class
    u16be(&mut out, 4); // super_class
    u16be(&mut out, 0); // interfaces
    u16be(&mut out, 0); // fields
    u16be(&mut out, 3); // methods
    member(
        &mut out,
        5,
        6,
        4,
        1,
        &[
            0x04, 0xbc, 10, 0x59, 0x03, 0x10, 7, 0x4f, 0x57, 0xb8, 0, 12, 0x57, 0xb1,
        ],
    );
    member(&mut out, 8, 9, 1, 0, &[0xb8, 0, 14, 0xac]);
    member(&mut out, 10, 9, 2, 0, &[0x10, 3, 0x10, 4, 0x60, 0xac]);
    u16be(&mut out, 0); // class attributes
    out
}

// AotDirect for the direct-leaf tracer: main() exercises four no-GC primitive leaf
// helpers and one same-signature NON-leaf helper, then exits with the arithmetic
// total (the observable that must be identical in both RAVA_AOT_DIRECT_LEAF modes).
//
//   sum5(JJJJJ)J  — five long args through all five SysV integer registers; body is
//                   param loads + ladd only (no consts — the leaf dialect has no
//                   constant table), so it is a valid no-GC leaf and is NOT inline
//                   NumericLeaf material (NumericLeaf only admits all-int params).
//   flag()Z       — iconst_1|iconst_1; deliberately NOT the two-byte flatcall shape.
//   mix(JZ)J      — long+boolean arg mix: iload_2 param, i2l, ladd.
//   sign(I)I      — signed int arg/result + the permitted FORWARD branch: iflt +5 →
//                   pc6, so (iconst_0, ireturn) vs (iload_0, ireturn). A straight-
//                   line iload body would be NumericLeaf-inlinable; the branch is not.
//   Alien.id(J)J  — primitive-signature but deliberately outside the strict
//                   leaf opcode dialect, so its call uses rava_invoke fallback.
//
// main = (1+2+3+4+5) + 100*flag() + mix(7,true) + Alien.id(7) + sign(-1)
//      = 15 + 100 + 8 + 14 + (-1) = 136.
fn direct_leaf_class() -> Vec<u8> {
    let mut out = vec![0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52];
    u16be(&mut out, 36); // constant_pool_count (indices 1..=35)
    utf8(&mut out, "AotDirect"); // #1
    class(1, &mut out); // #2
    utf8(&mut out, "java/lang/Object"); // #3
    class(3, &mut out); // #4
    utf8(&mut out, "java/lang/System"); // #5
    class(5, &mut out); // #6
    utf8(&mut out, "Code"); // #7  (member() expects the Code name at index 7)
    utf8(&mut out, "main"); // #8
    utf8(&mut out, "([Ljava/lang/String;)V"); // #9
    utf8(&mut out, "sum5"); // #10
    utf8(&mut out, "(JJJJJ)J"); // #11
    out.extend_from_slice(&[12, 0, 10, 0, 11]); // #12 NameAndType sum5:(JJJJJ)J
    out.extend_from_slice(&[10, 0, 2, 0, 12]); // #13 Methodref AotDirect.sum5
    utf8(&mut out, "flag"); // #14
    utf8(&mut out, "()Z"); // #15
    out.extend_from_slice(&[12, 0, 14, 0, 15]); // #16 NameAndType flag:()Z
    out.extend_from_slice(&[10, 0, 2, 0, 16]); // #17 Methodref AotDirect.flag
    utf8(&mut out, "mix"); // #18
    utf8(&mut out, "(JZ)J"); // #19
    out.extend_from_slice(&[12, 0, 18, 0, 19]); // #20 NameAndType mix:(JZ)J
    out.extend_from_slice(&[10, 0, 2, 0, 20]); // #21 Methodref AotDirect.mix
    utf8(&mut out, "exit"); // #22
    utf8(&mut out, "(I)V"); // #23
    out.extend_from_slice(&[12, 0, 22, 0, 23]); // #24 NameAndType exit:(I)V
    out.extend_from_slice(&[10, 0, 6, 0, 24]); // #25 Methodref System.exit
    utf8(&mut out, "Alien"); // #26
    class(26, &mut out); // #27
    utf8(&mut out, "id"); // #28
    utf8(&mut out, "(J)J"); // #29
    out.extend_from_slice(&[12, 0, 28, 0, 29]); // #30 NameAndType id:(J)J
    out.extend_from_slice(&[10, 0, 27, 0, 30]); // #31 Methodref Alien.id
    utf8(&mut out, "sign"); // #32
    utf8(&mut out, "(I)I"); // #33
    out.extend_from_slice(&[12, 0, 32, 0, 33]); // #34 NameAndType sign:(I)I
    out.extend_from_slice(&[10, 0, 2, 0, 34]); // #35 Methodref AotDirect.sign

    u16be(&mut out, 0x0021); // public + super
    u16be(&mut out, 2); // this_class
    u16be(&mut out, 4); // super_class
    u16be(&mut out, 0); // interfaces
    u16be(&mut out, 0); // fields
    u16be(&mut out, 5); // methods
                        // main: five iconst_1..5 each i2l, sum5, l2i, *100 (bipush 100, flag, imul), +,
                        // mix(7L,true), +, Alien.id(7L), +, sign(-1), +, System.exit(total).
    member(
        &mut out,
        8,
        9,
        10,
        1,
        &[
            0x04, 0x85, 0x05, 0x85, 0x06, 0x85, 0x07, 0x85, 0x08, 0x85, // 1L..5L
            0xb8, 0, 13,   // invokestatic sum5
            0x88, // l2i
            0x10, 100, // bipush 100
            0xb8, 0, 17,   // invokestatic flag
            0x68, // imul
            0x60, // iadd
            0x10, 7, 0x85, // bipush 7; i2l
            0x04, // iconst_1 (true)
            0xb8, 0, 21,   // invokestatic mix
            0x88, // l2i
            0x60, // iadd
            0x10, 7, 0x85, // bipush 7; i2l
            0xb8, 0, 31,   // invokestatic Alien.id (non-leaf fallback)
            0x88, // l2i
            0x60, // iadd
            0x02, // iconst_m1
            0xb8, 0, 35,   // invokestatic sign (forward-branch int leaf)
            0x60, // iadd
            0xb8, 0, 25,   // invokestatic System.exit (native-served → interpreter)
            0xb1, // return (unreachable)
        ],
    );
    // sum5: lload_0, lload 2, ladd, lload 4, ladd, lload 6, ladd, lload 8, ladd, lreturn.
    member(
        &mut out,
        10,
        11,
        4,
        10,
        &[
            0x16, 0, 0x16, 2, 0x61, 0x16, 4, 0x61, 0x16, 6, 0x61, 0x16, 8, 0x61, 0xad,
        ],
    );
    // flag: iconst_1, iconst_1, ior, ireturn.
    member(&mut out, 14, 15, 2, 0, &[0x04, 0x04, 0x80, 0xac]);
    // mix: lload_0, iload_2, i2l, ladd, lreturn.
    member(&mut out, 18, 19, 4, 3, &[0x16, 0, 0x1c, 0x85, 0x61, 0xad]);
    // sign: iload_0; iflt +5 → pc6; iconst_0; ireturn; iload_0; ireturn — the leaf
    // dialect's forward-branch shape (blocks enter empty, return-terminated).
    member(
        &mut out,
        32,
        33,
        1,
        1,
        &[0x1a, 0x9b, 0, 5, 0x03, 0xac, 0x1a, 0xac],
    );
    u16be(&mut out, 0); // class attributes
    out
}

// Alien: the non-leaf fallback target. Same primitive `(J)J` shape the leaf dialect
// accepts, but `dup2` is intentionally outside its narrow opcode whitelist. The
// classifier refuses it; the call site must resolve its leaf pointer to 0.
fn non_leaf_class() -> Vec<u8> {
    let mut out = vec![0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52];
    u16be(&mut out, 10); // constant_pool_count
    utf8(&mut out, "Alien"); // #1
    class(1, &mut out); // #2
    utf8(&mut out, "java/lang/Object"); // #3
    class(3, &mut out); // #4
    utf8(&mut out, "id"); // #5
    utf8(&mut out, "(J)J"); // #6
    utf8(&mut out, "Code"); // #7
    out.extend_from_slice(&[12, 0, 5, 0, 6]); // #8 NameAndType id:(J)J
    out.extend_from_slice(&[10, 0, 2, 0, 8]); // #9 Methodref Alien.id
    u16be(&mut out, 0x0021); // public + super
    u16be(&mut out, 2); // this_class
    u16be(&mut out, 4); // super_class
    u16be(&mut out, 0); // interfaces
    u16be(&mut out, 0); // fields
    u16be(&mut out, 1); // methods
                        // id: lload_0, dup2, ladd, lreturn — returns 2x input, but dup2 is not
                        // admitted by the direct leaf dialect, so this exercises the old fallback.
    member(&mut out, 5, 6, 4, 2, &[0x1e, 0x5c, 0x61, 0xad]);
    u16be(&mut out, 0); // class attributes
    out
}

fn route_counter(json: &str, name: &str) -> Option<u64> {
    let prefix =
        format!("\"category\":\"aot_route\",\"name\":\"{name}\",\"kind\":\"count\",\"value\":");
    let value = json.split_once(&prefix)?.1;
    value
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

fn counter(json: &str, name: &str) -> Option<u64> {
    let prefix = format!("\"category\":\"aot\",\"name\":\"{name}\",\"kind\":\"count\",\"value\":");
    let value = json.split_once(&prefix)?.1;
    value
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

fn success(output: &Output, command: &str) {
    assert!(
        output.status.success(),
        "{command} failed ({}):\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn prepared_aot_links_and_runs_in_a_second_process_without_jit_compile() {
    let root = TestDirectory::new();
    let classes = root.0.join("classes");
    let prepared = root.0.join("prepared");
    fs::create_dir_all(&classes).expect("create classpath");
    fs::write(classes.join("AotProbe.class"), probe_class()).expect("write fixture class");

    let binary = env!("CARGO_BIN_EXE_rava");
    let prepare = Command::new(binary)
        .arg("--prepare-aot")
        .arg(&prepared)
        .arg(&classes)
        .output()
        .expect("run offline AOT preparation");
    success(&prepare, "rava --prepare-aot");
    let report = String::from_utf8_lossy(&prepare.stdout);
    assert!(
        report.contains("classes=1"),
        "unexpected preparation report: {report}"
    );
    assert!(
        report.contains("compiled=3"),
        "fixture methods were not all prepared: {report}"
    );

    let run = Command::new(binary)
        .arg("-cp")
        .arg(&classes)
        .arg("AotProbe")
        .env("RAVA_AOT_PREPARED_DIR", &prepared)
        .env("RAVA_JIT", "0")
        .env("RAVA_DIAG", "aot")
        .env("RAVA_DIAG_FORMAT", "json")
        .output()
        .expect("run prepared fixture in a fresh process");
    success(&run, "rava with prepared AOT directory");

    let stderr = String::from_utf8_lossy(&run.stderr);
    let report = stderr
        .lines()
        .find(|line| line.starts_with("{\"reason\""))
        .unwrap_or("");
    assert!(
        counter(report, "cold_calls").is_some_and(|n| n > 0),
        "no AOT method was invoked: {stderr}"
    );
    assert!(
        counter(report, "read_ns").is_some_and(|n| n > 0),
        "no prepared object was read: {stderr}"
    );
    assert!(
        counter(report, "decode_ns").is_some_and(|n| n > 0),
        "no prepared object was decoded: {stderr}"
    );
    assert!(
        counter(report, "bind_ns").is_some_and(|n| n > 0),
        "no object was linked or relocated: {stderr}"
    );
}

// `RAVA_AOT_DIRECT_LEAF` gates the primitive leaf ABI independently of the
// pre-existing `RAVA_AOT_DIRECT` cached-target dispatch. Prepare one archive and run
// fresh processes with that older path explicitly off, so the leaf-only comparison
// isolates the no-pack direct function-pointer calls. The direct-on run must show
// FEWER aot.pack_completed counts while producing the identical numeric result; the
// non-leaf helper stays on the packing fallback in BOTH modes, and System.exit
// (native-served) stays on the interpreter in both.
//
// Expected arithmetic (identical in both modes):
// main = (1+2+3+4+5) + 100*flag() + mix(7,true) + Alien.id(7) + sign(-1)
//      = 15 + 100 + 8 + 14 + (-1) = 136 → exit code 136.
#[test]
fn prepared_direct_leaf_bypasses_packing_and_falls_back_when_unavailable() {
    let root = TestDirectory::new();
    let classes = root.0.join("classes");
    let prepared = root.0.join("prepared");
    fs::create_dir_all(&classes).expect("create classpath");
    fs::write(classes.join("AotDirect.class"), direct_leaf_class()).expect("write caller");
    fs::write(classes.join("Alien.class"), non_leaf_class()).expect("write non-leaf");

    let binary = env!("CARGO_BIN_EXE_rava");
    let prepare = Command::new(binary)
        .arg("--prepare-aot")
        .arg(&prepared)
        .arg(&classes)
        .output()
        .expect("run offline AOT preparation");
    success(&prepare, "rava --prepare-aot");
    let report = String::from_utf8_lossy(&prepare.stdout);
    assert!(
        report.contains("classes=2"),
        "unexpected preparation report: {report}"
    );
    // Both classes' methods compile through the regular translator; direct-leaf
    // artifacts are prepared separately and must not shift this count.
    assert!(
        report.contains("compiled=6"),
        "fixture methods were not all prepared: {report}"
    );
    // The archive must actually carry the four native leaf artifacts (sum5, flag,
    // mix, sign); Alien.id is primitive-signature but refused as a leaf.
    assert!(
        report.contains("direct_leaves=4"),
        "preparation did not record four direct-leaf artifacts: {report}"
    );

    let base_env = |command: &mut Command| {
        command
            .arg("-cp")
            .arg(&classes)
            .arg("AotDirect")
            .env("RAVA_AOT_PREPARED_DIR", &prepared)
            .env("RAVA_JIT", "0")
            .env("RAVA_DIAG", "aot_route")
            .env("RAVA_DIAG_FORMAT", "json");
    };

    // Direct OFF: every helper call rides rava_invoke → aot_run_input (packed).
    // NOTE: the run itself exits 136 (System.exit), so status.success() is NOT
    // asserted here — the numeric assertions below pin the exact exit code.
    let mut off = Command::new(binary);
    base_env(&mut off);
    let off = off
        .env("RAVA_AOT_DIRECT", "0")
        .env("RAVA_AOT_DIRECT_LEAF", "0")
        .output()
        .expect("run direct-off fixture in a fresh process");

    // Direct ON: leaf targets resolve to nonzero native pointers and bypass packing.
    let mut on = Command::new(binary);
    base_env(&mut on);
    let on = on
        .env("RAVA_AOT_DIRECT", "0")
        .env("RAVA_AOT_DIRECT_LEAF", "1")
        .output()
        .expect("run direct-on fixture in a fresh process");

    // Both modes must agree on the number: it is a plain arithmetic chain, so any
    // direct-ABI register-order/width bug (or a missed fallback) changes the total.
    assert_eq!(off.status.code(), Some(136), "direct-off numeric result");
    assert_eq!(on.status.code(), Some(136), "direct-on numeric result");

    let off_stderr = String::from_utf8_lossy(&off.stderr);
    let on_stderr = String::from_utf8_lossy(&on.stderr);
    let off_report = off_stderr
        .lines()
        .find(|line| line.starts_with("{\"reason\""))
        .unwrap_or("");
    let on_report = on_stderr
        .lines()
        .find(|line| line.starts_with("{\"reason\""))
        .unwrap_or("");
    let off_packs = route_counter(off_report, "aot.pack_completed")
        .unwrap_or_else(|| panic!("direct-off reported no aot.pack_completed: {off_stderr}"));
    let on_packs = route_counter(on_report, "aot.pack_completed")
        .unwrap_or_else(|| panic!("direct-on reported no aot.pack_completed: {on_stderr}"));
    // At least two helper calls use packed rava_invoke in direct-off and bypass
    // packing in direct-on. Existing AOT fast routes can already elide packing
    // for other sites, so do not assume all four helpers contribute to the delta.
    assert!(
        off_packs >= on_packs + 2,
        "direct-on did not bypass packed leaf calls (off={off_packs}, on={on_packs})\noff:\n{off_stderr}\non:\n{on_stderr}"
    );
    // The pre-existing cached-AOT static dispatch is explicitly held off in both
    // children so this test exercises only primitive leaf calls.
    assert_eq!(
        route_counter(on_report, "aot.direct_static_attempt").unwrap_or(0),
        0,
        "old cached-static dispatch was unexpectedly enabled: {on_stderr}"
    );
    // The old cached-AOT route remains a valid fallback after the JIT direct cache misses.
    let mut legacy = Command::new(binary);
    base_env(&mut legacy);
    let legacy = legacy
        .env("RAVA_AOT_DIRECT", "1")
        .env("RAVA_AOT_DIRECT_LEAF", "0")
        .output()
        .expect("run legacy AOT direct fallback in a fresh process");
    assert_eq!(legacy.status.code(), Some(136), "legacy AOT result");
    let legacy_stderr = String::from_utf8_lossy(&legacy.stderr);
    let legacy_report = legacy_stderr
        .lines()
        .find(|line| line.starts_with("{\"reason\""))
        .unwrap_or("");
    assert!(
        route_counter(legacy_report, "aot.direct_static_attempt").unwrap_or(0) > 0,
        "legacy AOT fallback was not attempted after a JIT miss: {legacy_stderr}"
    );

}
