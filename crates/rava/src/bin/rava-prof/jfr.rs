//! `import-jfr`: a HotSpot recording, as `jfr print --json --events jdk.ExecutionSample` (and
//! `jdk.ObjectAllocationSample`, as allocation samples weighing their bytes) prints
//! it, written as a rava recording so the same reports and `diff` read both. A HotSpot frame
//! of type `Inlined` was inlined into the next frame out, as a rava inlined frame; `JIT
//! compiled` code counts as `opt`. Each sample weighs the sampling period (`--period`, the
//! JFR setting's: 10 ms in `profile`, 20 ms in `default`).

use crate::json::{Parser, Value};
use rava::profile::format::{Frame, Record, State, Tier, Writer};
use std::collections::HashMap;
use std::io::BufWriter;

pub fn import(input: &str, output: &str, period_ms: f64) -> Result<(), String> {
    let text = std::fs::read(input).map_err(|e| format!("{input}: {e}"))?;
    let out = std::fs::File::create(output).map_err(|e| format!("{output}: {e}"))?;
    let mut w = Writer::new(BufWriter::new(out)).map_err(|e| e.to_string())?;
    let io = |e: std::io::Error| e.to_string();
    w.write(&Record::Meta { key: "vm".into(), value: "HotSpot (imported from JFR)".into() }).map_err(io)?;
    w.write(&Record::Meta { key: "command".into(), value: input.into() }).map_err(io)?;
    w.write(&Record::Meta { key: "sample_period_us".into(), value: format!("{}", (period_ms * 1000.0) as u64) }).map_err(io)?;
    let mut strings: HashMap<String, u32> = HashMap::new();
    let mut stacks: HashMap<Vec<Frame>, u32> = HashMap::new();
    let mut threads: HashMap<u32, u32> = HashMap::new();
    let mut intern = |w: &mut Writer<_>, s: &str| -> Result<u32, String> {
        if let Some(&id) = strings.get(s) {
            return Ok(id);
        }
        let id = strings.len() as u32 + 1;
        strings.insert(s.to_string(), id);
        w.write(&Record::Str { id, text: s.to_string() }).map_err(|e| e.to_string())?;
        Ok(id)
    };
    let mut p = Parser::new(&text);
    if !p.seek_key("events") || !p.eat(b'[') {
        return Err(format!("{input}: no events array (is it `jfr print --json` output?)"));
    }
    let (mut first, mut n, mut truncated) = (None, 0u64, 0u64);
    if !p.eat(b']') {
        loop {
            let ev = p.value()?;
            if let Some(sample) = sample(&ev) {
                let t0 = *first.get_or_insert(sample.time);
                let name = intern(&mut w, &sample.thread_name)?;
                if threads.insert(sample.thread, name) != Some(name) {
                    w.write(&Record::Thread { id: sample.thread, name }).map_err(io)?;
                }
                let mut frames = Vec::with_capacity(sample.frames.len());
                for (method, bci, tier, inlined) in &sample.frames {
                    frames.push(Frame::Java { method: intern(&mut w, method)?, bci: *bci, tier: *tier, inlined: *inlined });
                }
                let next = stacks.len() as u32 + 1;
                let stack = match stacks.get(&frames) {
                    Some(&id) => id,
                    None => {
                        w.write(&Record::Stack { id: next, frames: frames.clone() }).map_err(io)?;
                        stacks.insert(frames, next);
                        next
                    }
                };
                let time = ((sample.time - t0).max(0.0) * 1e6) as u64;
                match &sample.alloc {
                    Some((class, bytes)) => {
                        let class = intern(&mut w, class)?;
                        w.write(&Record::Alloc { time, thread: sample.thread, class, bytes: *bytes, stack }).map_err(io)?;
                    }
                    None => w.write(&Record::Sample { time, thread: sample.thread, state: sample.state, cycles: 0, stack }).map_err(io)?,
                }
                n += 1;
                truncated += sample.truncated as u64;
            }
            if !p.eat(b',') {
                break;
            }
        }
    }
    w.write(&Record::Meta { key: "truncated_samples".into(), value: truncated.to_string() }).map_err(io)?;
    w.flush().map_err(io)?;
    eprintln!("imported {n} samples, {} stacks, {} threads", stacks.len(), threads.len());
    if truncated > 0 {
        eprintln!(
            "warning: {truncated} samples ({:.1}%) have stacks JFR cut to their innermost frames: their outer frames' inclusive time is lost (record with -XX:FlightRecorderOptions=stackdepth=2048)",
            100.0 * truncated as f64 / n.max(1) as f64
        );
    }
    Ok(())
}

struct Sample {
    /// JFR kept only the innermost frames (its `stackdepth`, 64 by default).
    truncated: bool,
    /// For an allocation sample: the class allocated and the bytes the sample weighs.
    alloc: Option<(String, u64)>,
    time: f64,
    thread: u32,
    thread_name: String,
    state: State,
    frames: Vec<(String, Option<u32>, Tier, bool)>,
}

fn sample(ev: &Value) -> Option<Sample> {
    let v = ev.get("values")?;
    let (state, alloc) = match ev.get("type")?.str()? {
        "jdk.ExecutionSample" => (State::Java, None),
        "jdk.NativeMethodSample" => (State::Native, None),
        "jdk.ObjectAllocationSample" => {
            let class = internal(v.get("objectClass").and_then(|c| c.get("name")).and_then(Value::str).unwrap_or("?"));
            (State::Java, Some((class, v.get("weight").and_then(Value::num).unwrap_or(0.0) as u64)))
        }
        _ => return None,
    };
    let th = v.get("sampledThread").or_else(|| v.get("eventThread"))?;
    let thread = th.get("osThreadId").and_then(Value::num).or_else(|| th.get("javaThreadId").and_then(Value::num))? as u32;
    let thread_name = th.get("javaName").and_then(Value::str).or_else(|| th.get("osName").and_then(Value::str)).unwrap_or("?").to_string();
    let time = v.get("startTime").and_then(Value::str).and_then(seconds)?;
    let mut frames = Vec::new();
    let truncated = v.get("stackTrace")?.get("truncated") == Some(&Value::Bool(true));
    for f in v.get("stackTrace")?.get("frames")?.arr() {
        let m = f.get("method")?;
        let holder = m.get("type").and_then(|t| t.get("name")).and_then(Value::str).unwrap_or("?");
        let name = m.get("name").and_then(Value::str).unwrap_or("?");
        let desc = m.get("descriptor").and_then(Value::str).unwrap_or("");
        let bci = f.get("bytecodeIndex").and_then(Value::num).filter(|&b| b >= 0.0).map(|b| b as u32);
        let (tier, inlined) = match f.get("type").and_then(Value::str).unwrap_or("") {
            "Interpreted" => (Tier::Interp, false),
            "Inlined" => (Tier::Opt, true),
            "JIT compiled" => (Tier::Opt, false),
            _ => (Tier::Unknown, false),
        };
        frames.push((format!("{}.{name}{desc}", internal(holder)), bci, tier, inlined));
    }
    Some(Sample { truncated, alloc, time, thread, thread_name, state, frames })
}

/// A class name in internal form: JFR names hidden classes (lambdas) with dots
/// (`a.B$$Lambda/0x...`), others with slashes.
fn internal(name: &str) -> String {
    if name.contains('/') && !name.contains("/0x") { name.to_string() } else { name.replace('.', "/") }
}

/// Seconds of an ISO 8601 time (`2026-10-21T01:49:18.123456789+03:00`) since the epoch, the
/// offset ignored (a recording keeps one).
fn seconds(s: &str) -> Option<f64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let time = time.trim_end_matches('Z');
    let time = time.find(['+', '-']).map_or(time, |i| &time[..i]);
    let mut t = time.split(':');
    let (h, mi, sec) = (t.next()?.parse::<f64>().ok()?, t.next()?.parse::<f64>().ok()?, t.next()?.parse::<f64>().ok()?);
    // Days from the civil date (Howard Hinnant's algorithm).
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    Some(days as f64 * 86400.0 + h * 3600.0 + mi * 60.0 + sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_classes_internally() {
        assert_eq!(internal("net.minecraft.A$$Lambda/0x0000000085f62b10"), "net/minecraft/A$$Lambda/0x0000000085f62b10");
        assert_eq!(internal("java/util/HashMap"), "java/util/HashMap");
    }

    #[test]
    fn reads_iso_times() {
        assert_eq!(seconds("1970-01-01T00:00:01Z"), Some(1.0));
        let a = seconds("2026-10-21T01:49:18.5+03:00").unwrap();
        let b = seconds("2026-10-21T01:49:20.25+03:00").unwrap();
        assert!((b - a - 1.75).abs() < 1e-6);
    }

    #[test]
    fn reads_a_sample() {
        let ev = r#"{"type": "jdk.ExecutionSample", "values": {"startTime": "2026-10-21T01:00:00Z",
            "sampledThread": {"osName": "w", "osThreadId": 42, "javaName": "Worker-1"}, "state": "STATE_RUNNABLE",
            "stackTrace": {"truncated": false, "frames": [
              {"method": {"type": {"name": "java\/util\/HashMap"}, "name": "hash", "descriptor": "(Ljava\/lang\/Object;)I"}, "bytecodeIndex": 9, "type": "Inlined"},
              {"method": {"type": {"name": "A"}, "name": "run", "descriptor": "()V"}, "bytecodeIndex": 3, "type": "JIT compiled"}]}}}"#;
        let v = Parser::new(ev.as_bytes()).value().unwrap();
        let s = sample(&v).unwrap();
        assert!(!s.truncated);
        assert_eq!((s.thread, s.thread_name.as_str()), (42, "Worker-1"));
        assert_eq!(s.frames[0], ("java/util/HashMap.hash(Ljava/lang/Object;)I".to_string(), Some(9), Tier::Opt, true));
        assert_eq!(s.frames[1].3, false);
    }
}
