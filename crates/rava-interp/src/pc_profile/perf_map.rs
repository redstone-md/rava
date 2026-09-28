use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::{Mutex, OnceLock};

static OUTPUT: OnceLock<Option<Mutex<BufWriter<File>>>> = OnceLock::new();
static ENABLED: OnceLock<bool> = OnceLock::new();

pub(super) fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var_os("RAVA_PERF_MAP").is_some())
}

/// Publish one finalized guest method to Linux perf's per-process map.
pub(super) fn register(start: usize, len: usize, mid: u32) {
    if !enabled() || start == 0 || len == 0 {
        return;
    }
    let Some(name) = crate::jit::compiled_method_name(mid) else {
        return;
    };
    let Some(output) = output() else {
        return;
    };
    if let Ok(mut output) = output.lock() {
        if let Err(error) = write_record(&mut output, start, len, &name) {
            eprintln!("[PERF-MAP] failed to publish {name}: {error}");
        }
    }
}

fn output() -> Option<&'static Mutex<BufWriter<File>>> {
    OUTPUT
        .get_or_init(|| {
            let path = format!("/tmp/perf-{}.map", std::process::id());
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(file) => Some(Mutex::new(BufWriter::new(file))),
                Err(error) => {
                    eprintln!("[PERF-MAP] cannot create {path}: {error}");
                    None
                }
            }
        })
        .as_ref()
}

fn write_record(
    output: &mut BufWriter<File>,
    start: usize,
    len: usize,
    name: &str,
) -> std::io::Result<()> {
    output.write_all(format_record(start, len, name).as_bytes())?;
    output.flush()
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats_perf_map_addresses_and_method_name() {
        let record = super::format_record(0x1234, 0x2a, "pkg/Type.run(I)V");
        assert_eq!(record, "1234 2a pkg/Type.run(I)V\n");
    }
}

fn format_record(start: usize, len: usize, name: &str) -> String {
    format!("{start:x} {len:x} {name}\n")
}
