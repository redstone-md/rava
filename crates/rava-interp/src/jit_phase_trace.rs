//! Opt-in phase intervals; the bounded diagnostic channel never blocks producers.
use std::io::{BufWriter, Write};
use std::sync::{Arc, OnceLock, mpsc};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Record {
    phase: &'static str,
    key: usize,
    epoch_us: u128,
    elapsed_us: u128,
    thread: std::thread::ThreadId,
}

struct Trace {
    sender: mpsc::SyncSender<Record>,
    dropped: Arc<AtomicU64>,
}

pub(crate) struct Span {
    record: Record,
    start: Instant,
    trace: &'static Trace,
    _local: std::marker::PhantomData<*mut ()>,
}

fn initialize() -> Option<Trace> {
    let path = std::env::var_os("RAVA_JIT_PHASE_TRACE")?;
    let file = match std::fs::File::create(path) {
        Ok(file) => file,
        Err(error) => { eprintln!("[JIT-PHASE-ERROR] open: {error}"); return None; }
    };
    let (sender, receiver) = mpsc::sync_channel::<Record>(65536);
    let dropped = Arc::new(AtomicU64::new(0));
    let writer_dropped = Arc::clone(&dropped);
    if let Err(error) = std::thread::Builder::new().name("rava-jit-phase-trace".into())
        .spawn(move || {
            if let Err(error) = write_records(file, receiver, writer_dropped) {
                eprintln!("[JIT-PHASE-ERROR] write: {error}");
            }
        })
    {
        eprintln!("[JIT-PHASE-ERROR] spawn: {error}");
        return None;
    }
    Some(Trace { sender, dropped })
}

fn write_records(file: std::fs::File, receiver: mpsc::Receiver<Record>, dropped: Arc<AtomicU64>)
    -> std::io::Result<()>
{
    let mut writer = BufWriter::with_capacity(65536, file);
    let mut last_flush = Instant::now();
    let mut last_dropped = 0;
    loop {
        let disconnected = match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(r) => {
                writeln!(writer,
                    "[JIT-PHASE] phase={} thread={:?} key={:x} start_us={} elapsed_us={}",
                    r.phase, r.thread, r.key, r.epoch_us, r.elapsed_us)?;
                false
            }
            Err(mpsc::RecvTimeoutError::Timeout) => false,
            Err(mpsc::RecvTimeoutError::Disconnected) => true,
        };
        if disconnected || last_flush.elapsed() >= Duration::from_millis(50) {
            let count = dropped.load(Ordering::Relaxed);
            if count != last_dropped {
                writeln!(writer, "[JIT-PHASE-DROPPED] {count}")?;
                last_dropped = count;
            }
            writer.flush()?;
            last_flush = Instant::now();
        }
        if disconnected { return Ok(()); }
    }
}

pub(crate) fn enter(phase: &'static str, key: usize) -> Option<Span> {
    static TRACE: OnceLock<Option<Trace>> = OnceLock::new();
    let trace = TRACE.get_or_init(initialize).as_ref()?;
    Some(Span {
        record: Record {
            phase, key, elapsed_us: 0,
            epoch_us: SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_micros(),
            thread: std::thread::current().id(),
        },
        start: Instant::now(), trace,
        _local: std::marker::PhantomData,
    })
}

impl Drop for Span {
    fn drop(&mut self) {
        let record = Record {
            phase: self.record.phase, key: self.record.key,
            epoch_us: self.record.epoch_us, thread: self.record.thread,
            elapsed_us: self.start.elapsed().as_micros(),
        };
        if self.trace.sender.try_send(record).is_err() {
            self.trace.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}
