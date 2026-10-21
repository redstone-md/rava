//! `annotate`: CPU by instruction in the compiled code of the methods whose name contains a
//! pattern. Each code region (one installation of a method's code) lists its hottest offsets
//! with the bytes there, when the recording has them (`RAVA_RECORD=...,code`), and the source
//! frame the samples there were in (innermost first, `method@bci`). `--dump <dir>` writes each
//! region's machine code (`<method>-<start>.bin`) and its offsets with their seconds
//! (`.txt`), for a disassembler.

use crate::model::{Profile, Region};
use crate::report::View;
use crate::symbols::Symbols;
use rava::profile::format::Frame;
use std::collections::HashMap;
use std::fmt::Write as _;

/// Hot offsets of one region: seconds per offset, and per offset the frames seen there.
#[derive(Default)]
struct Hot {
    total: f64,
    offsets: HashMap<u32, (f64, HashMap<u32, f64>)>,
}

pub fn annotate(p: &Profile, v: &View, pattern: &str, dump: Option<&str>, sym: &mut Symbols) -> Result<(), String> {
    let name = |r: &Region| p.strings.get(&r.method).map_or("?", |s| s.as_str());
    let mut hot: HashMap<u32, Hot> = HashMap::new();
    let (mut matched, mut all) = (0.0, 0.0);
    for s in p.samples.iter().filter(|s| p.keeps(&v.filter, s)) {
        all += s.weight;
        let Some((region, offset)) = s.at else { continue };
        if !name(&p.regions[region as usize]).contains(pattern) {
            continue;
        }
        matched += s.weight;
        let h = hot.entry(region).or_default();
        h.total += s.weight;
        let o = h.offsets.entry(offset).or_default();
        o.0 += s.weight;
        *o.1.entry(s.stack).or_default() += s.weight;
    }
    if hot.is_empty() {
        let pcs = p.samples.iter().filter(|s| s.at.is_some()).count();
        return Err(if pcs == 0 { "the recording has no instruction addresses (recorded before rava-prof annotate existed?)".into() } else { format!("no samples in compiled code of a method matching {pattern:?}") });
    }
    println!("{matched:.2} CPU s in code of methods matching {pattern:?} ({:.1}% of {all:.2} s)", 100.0 * matched / all.max(1e-9));
    let mut regions: Vec<(u32, Hot)> = hot.into_iter().collect();
    regions.sort_by(|a, b| b.1.total.total_cmp(&a.1.total));
    for (id, h) in regions.iter().take(v.top.clamp(1, 8)) {
        let r = &p.regions[*id as usize];
        println!();
        println!("{} [{}] at {:#x}, {} bytes: {:.2} s ({:.1}%)", name(r), r.tier.name(), r.start, r.size, h.total, 100.0 * h.total / matched);
        let mut offsets: Vec<(&u32, &(f64, HashMap<u32, f64>))> = h.offsets.iter().collect();
        offsets.sort_by(|a, b| b.1 .0.total_cmp(&a.1 .0));
        for (&off, (w, stacks)) in offsets.into_iter().take(v.top) {
            let bytes = r.bytes.as_ref().map_or(String::new(), |b| hex(&b[(off as usize).min(b.len())..(off as usize + 8).min(b.len())]));
            let stack = stacks.iter().max_by(|a, b| a.1.total_cmp(b.1)).map_or(0, |(&s, _)| s);
            println!("  +{off:#06x} {w:8.3} s {:5.1}%  {bytes:<24} {}", 100.0 * w / h.total, source(p, stack, sym));
        }
        if let Some(dir) = dump {
            write_dump(dir, name(r), r, h)?;
        }
    }
    if let Some(dir) = dump {
        println!();
        println!("machine code and offsets written to {dir}");
    }
    Ok(())
}

/// The innermost Java frame of a stack, as `method@bci`.
fn source(p: &Profile, stack: u32, sym: &mut Symbols) -> String {
    let Some(frames) = p.stacks.get(&stack) else { return String::new() };
    match frames.iter().find(|f| matches!(f, Frame::Java { .. })) {
        Some(f @ Frame::Java { bci, .. }) => {
            let label = short(&p.label(f, sym));
            match bci {
                Some(b) => format!("{label}@{b}"),
                None => label,
            }
        }
        _ => String::new(),
    }
}

/// `pkg/Class.method(desc)` as `Class.method`.
fn short(method: &str) -> String {
    let name = method.split('(').next().unwrap_or(method);
    name.rsplit('/').next().unwrap_or(name).to_string()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x} ");
        s
    })
}

fn write_dump(dir: &str, method: &str, r: &Region, h: &Hot) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{dir}: {e}"))?;
    let file: String = short(method).chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '$' { c } else { '_' }).collect();
    let base = format!("{dir}/{file}-{:x}", r.start);
    if let Some(b) = &r.bytes {
        std::fs::write(format!("{base}.bin"), b).map_err(|e| format!("{base}.bin: {e}"))?;
    }
    let mut offsets: Vec<(&u32, &(f64, HashMap<u32, f64>))> = h.offsets.iter().collect();
    offsets.sort_by_key(|o| *o.0);
    let text = offsets.iter().fold(format!("# {method} [{}] at {:#x}\n", r.tier.name(), r.start), |mut s, (off, (w, _))| {
        let _ = writeln!(s, "{off:#x} {w:.4}");
        s
    });
    std::fs::write(format!("{base}.txt"), text).map_err(|e| format!("{base}.txt: {e}"))
}
