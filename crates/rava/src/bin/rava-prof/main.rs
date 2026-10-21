//! `rava-prof`: analyzes recordings made with `RAVA_RECORD` (`docs/profiling.md`).

mod diff;
mod jfr;
mod json;
mod model;
mod report;
mod symbols;

use model::Profile;
use rava::profile::format::State;
use report::View;
use symbols::Symbols;

const USAGE: &str = "\
usage: rava-prof <command> [options]

commands:
  summary <file>                 duration, CPU by state, code by tier, CPU per thread
  threads <file> [--window <s>]  cores busy per thread name per window (default 10 s)
  top <file> [--inclusive]       frames by self (or inclusive) CPU seconds
  tree <file> <frame>            callees under a frame, inclusive, as a tree
                                 [--depth <n>] (8) [--min <percent>] (0.5)
                                 [--callers]: the callers above it instead
  diff <a> <b> [--inclusive]     two recordings' frames side by side, by the difference
  folded <file>                  folded stacks (thread;outermost;...;innermost ms)
  import-jfr <in.json> <out>     HotSpot `jfr print --json --events jdk.ExecutionSample`
                                 output as a recording [--period <ms>] (10)

options:
  --thread <prefix>   samples of threads whose name starts with it
  --state <state>     samples in one state: java, vm, syscall, native
  --from <s> --to <s> samples of a time window
  --within <frame>    (top, diff) samples with that frame on their stack
  --top <n>           rows (default 40)
  --leaf              self time to the innermost frame, inlined or not (default: the
                      innermost frame holding compiled code, comparable across VMs)
  --tiers             Java frames with their tier
  --keep-lambdas      keep lambda classes' addresses (merged by default)
  --no-symbols        native frames as module+offset
";

fn main() {
    if let Err(e) = run(std::env::args().skip(1).collect()) {
        eprintln!("rava-prof: {e}");
        std::process::exit(1);
    }
}

struct Args {
    positional: Vec<String>,
    view: View,
    inclusive: bool,
    callers: bool,
    no_symbols: bool,
    window: f64,
    depth: usize,
    min: f64,
    period: f64,
}

fn parse(args: Vec<String>) -> Result<Args, String> {
    let mut a = Args { positional: Vec::new(), view: View { physical: true, top: 40, merge_lambdas: true, ..Default::default() }, inclusive: false, callers: false, no_symbols: false, window: 10.0, depth: 8, min: 0.5, period: 10.0 };
    let mut it = args.into_iter();
    let num = |it: &mut std::vec::IntoIter<String>, flag: &str| -> Result<f64, String> { it.next().and_then(|v| v.parse().ok()).ok_or_else(|| format!("{flag} needs a number")) };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--thread" => a.view.filter.thread = Some(it.next().ok_or("--thread needs a name prefix")?),
            "--state" => a.view.filter.state = Some(it.next().and_then(|s| State::parse(&s)).ok_or("--state needs java, vm, syscall or native")?),
            "--from" => a.view.filter.from = Some(num(&mut it, "--from")?),
            "--to" => a.view.filter.to = Some(num(&mut it, "--to")?),
            "--top" => a.view.top = num(&mut it, "--top")? as usize,
            "--within" => a.view.within = Some(it.next().ok_or("--within needs a frame")?),
            "--window" => a.window = num(&mut it, "--window")?.max(0.1),
            "--depth" => a.depth = num(&mut it, "--depth")? as usize,
            "--min" => a.min = num(&mut it, "--min")?,
            "--period" => a.period = num(&mut it, "--period")?,
            "--inclusive" => a.inclusive = true,
            "--callers" => a.callers = true,
            "--leaf" => a.view.physical = false,
            "--tiers" => a.view.tiers = true,
            "--keep-lambdas" => a.view.merge_lambdas = false,
            "--no-symbols" => a.no_symbols = true,
            "-h" | "--help" => return Err(USAGE.trim_end().to_string()),
            s if s.starts_with("--") => return Err(format!("unknown option {s}\n\n{USAGE}")),
            _ => a.positional.push(arg),
        }
    }
    Ok(a)
}

fn run(args: Vec<String>) -> Result<(), String> {
    let a = parse(args)?;
    let mut sym = Symbols::new(a.no_symbols);
    let pos: Vec<&str> = a.positional.iter().map(|s| s.as_str()).collect();
    let v = &a.view;
    match pos[..] {
        ["summary", f] => report::summary(&Profile::load(f)?, v),
        ["threads", f] => report::threads(&Profile::load(f)?, v, a.window),
        ["top", f] => report::top(&Profile::load(f)?, v, a.inclusive, &mut sym),
        ["tree", f, root] => report::tree(&Profile::load(f)?, v, root, a.depth, a.min, a.callers, &mut sym),
        ["diff", x, y] => diff::diff(&Profile::load(x)?, &Profile::load(y)?, v, a.inclusive, &mut sym),
        ["folded", f] => report::folded(&Profile::load(f)?, v, &mut sym),
        ["import-jfr", input, output] => jfr::import(input, output, a.period)?,
        _ => return Err(USAGE.trim_end().to_string()),
    }
    Ok(())
}
