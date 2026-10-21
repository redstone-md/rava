//! The recording format (`docs/profiling.md`): the magic `RAVAPROF`, a little-endian `u16`
//! version, then records of a kind byte, a LEB128 length and the payload. Integers are
//! unsigned LEB128; strings, modules and stacks are written once with an id and referred to by
//! it. A reader skips the kinds it does not know, so new kinds need no new version; a changed
//! layout does.

use std::io::{self, Read, Write};

pub const MAGIC: &[u8; 8] = b"RAVAPROF";
pub const VERSION: u16 = 1;

/// The tier of the code a Java frame runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Tier {
    Unknown = 0,
    Interp = 1,
    Baseline = 2,
    Opt = 3,
    Osr = 4,
}

impl Tier {
    fn from(b: u8) -> Tier {
        match b {
            1 => Tier::Interp,
            2 => Tier::Baseline,
            3 => Tier::Opt,
            4 => Tier::Osr,
            _ => Tier::Unknown,
        }
    }

    pub fn name(self) -> &'static str {
        ["?", "interp", "baseline", "opt", "osr"][self as usize]
    }
}

/// Where a sampled thread was (`docs/profiling.md`, thread states).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum State {
    Java = 0,
    Vm = 1,
    Syscall = 2,
    Native = 3,
}

impl State {
    fn from(b: u8) -> State {
        match b {
            0 => State::Java,
            1 => State::Vm,
            2 => State::Syscall,
            _ => State::Native,
        }
    }

    pub fn name(self) -> &'static str {
        ["java", "vm", "syscall", "native"][self as usize]
    }

    pub fn parse(s: &str) -> Option<State> {
        ["java", "vm", "syscall", "native"].iter().position(|&n| n == s).map(|i| State::from(i as u8))
    }
}

/// One frame of a stack, innermost first.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Frame {
    /// A Java method (an interned string, `holder.name(descriptor)`), its bytecode index when
    /// known, its code's tier and whether it was inlined into the next frame out.
    Java { method: u32, bci: Option<u32>, tier: Tier, inlined: bool },
    /// An offset into a loaded module.
    Native { module: u32, offset: u64 },
    /// An address in no known module or code.
    Address(u64),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Record {
    Meta { key: String, value: String },
    Str { id: u32, text: String },
    Thread { id: u32, name: u32 },
    Module { id: u32, path: u32, base: u64, size: u64 },
    Code { start: u64, size: u64, method: u32, tier: Tier },
    Stack { id: u32, frames: Vec<Frame> },
    /// `time` in microseconds since the recording started; `cycles` the thread used since its
    /// previous sample.
    Sample { time: u64, thread: u32, state: State, cycles: u64, stack: u32 },
    /// A thread's total CPU cycles at `time`.
    ThreadCpu { time: u64, thread: u32, cycles: u64 },
    /// A kind this reader does not know.
    Unknown(u8),
}

const META: u8 = 1;
const STR: u8 = 2;
const THREAD: u8 = 3;
const MODULE: u8 = 4;
const CODE: u8 = 5;
const STACK: u8 = 6;
const SAMPLE: u8 = 7;
const THREAD_CPU: u8 = 8;

fn put(buf: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            buf.push(b);
            return;
        }
        buf.push(b | 0x80);
    }
}

fn put_str(buf: &mut Vec<u8>, s: &str) {
    put(buf, s.len() as u64);
    buf.extend_from_slice(s.as_bytes());
}

/// Writes records to `out`, the header first.
pub struct Writer<W: Write> {
    out: W,
    buf: Vec<u8>,
}

impl<W: Write> Writer<W> {
    pub fn new(mut out: W) -> io::Result<Writer<W>> {
        out.write_all(MAGIC)?;
        out.write_all(&VERSION.to_le_bytes())?;
        Ok(Writer { out, buf: Vec::with_capacity(256) })
    }

    pub fn write(&mut self, r: &Record) -> io::Result<()> {
        let b = &mut self.buf;
        b.clear();
        let kind = match r {
            Record::Meta { key, value } => {
                put_str(b, key);
                put_str(b, value);
                META
            }
            Record::Str { id, text } => {
                put(b, *id as u64);
                put_str(b, text);
                STR
            }
            Record::Thread { id, name } => {
                put(b, *id as u64);
                put(b, *name as u64);
                THREAD
            }
            Record::Module { id, path, base, size } => {
                for v in [*id as u64, *path as u64, *base, *size] {
                    put(b, v);
                }
                MODULE
            }
            Record::Code { start, size, method, tier } => {
                for v in [*start, *size, *method as u64, *tier as u64] {
                    put(b, v);
                }
                CODE
            }
            Record::Stack { id, frames } => {
                put(b, *id as u64);
                put(b, frames.len() as u64);
                for f in frames {
                    match *f {
                        Frame::Java { method, bci, tier, inlined } => {
                            b.push(0);
                            put(b, method as u64);
                            put(b, bci.map_or(0, |x| x as u64 + 1));
                            b.push(tier as u8 | (inlined as u8) << 7);
                        }
                        Frame::Native { module, offset } => {
                            b.push(1);
                            put(b, module as u64);
                            put(b, offset);
                        }
                        Frame::Address(a) => {
                            b.push(2);
                            put(b, a);
                        }
                    }
                }
                STACK
            }
            Record::Sample { time, thread, state, cycles, stack } => {
                for v in [*time, *thread as u64, *state as u64, *cycles, *stack as u64] {
                    put(b, v);
                }
                SAMPLE
            }
            Record::ThreadCpu { time, thread, cycles } => {
                for v in [*time, *thread as u64, *cycles] {
                    put(b, v);
                }
                THREAD_CPU
            }
            Record::Unknown(_) => return Ok(()),
        };
        let mut head = Vec::with_capacity(11);
        head.push(kind);
        put(&mut head, b.len() as u64);
        self.out.write_all(&head)?;
        self.out.write_all(b)
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Reads the records of a recording.
pub struct Reader<R: Read> {
    input: R,
    payload: Vec<u8>,
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

/// A cursor over one record's payload.
struct Cur<'a>(&'a [u8]);

impl Cur<'_> {
    fn num(&mut self) -> io::Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let (&b, rest) = self.0.split_first().ok_or_else(|| bad("a truncated number"))?;
            self.0 = rest;
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(bad("an overlong number"))
    }

    fn id(&mut self) -> io::Result<u32> {
        u32::try_from(self.num()?).map_err(|_| bad("an id over 32 bits"))
    }

    fn byte(&mut self) -> io::Result<u8> {
        let (&b, rest) = self.0.split_first().ok_or_else(|| bad("a truncated record"))?;
        self.0 = rest;
        Ok(b)
    }

    fn text(&mut self) -> io::Result<String> {
        let n = self.num()? as usize;
        if n > self.0.len() {
            return Err(bad("a truncated string"));
        }
        let (s, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(String::from_utf8_lossy(s).into_owned())
    }
}

impl<R: Read> Reader<R> {
    pub fn new(mut input: R) -> io::Result<Reader<R>> {
        let mut head = [0u8; 10];
        input.read_exact(&mut head)?;
        if &head[..8] != MAGIC {
            return Err(bad("not a rava recording"));
        }
        let version = u16::from_le_bytes([head[8], head[9]]);
        if version != VERSION {
            return Err(bad(&format!("recording version {version}, this reader reads {VERSION}")));
        }
        Ok(Reader { input, payload: Vec::new() })
    }

    fn num(&mut self) -> io::Result<Option<u64>> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let mut b = [0u8];
            if self.input.read(&mut b)? == 0 {
                return if shift == 0 { Ok(None) } else { Err(bad("a truncated length")) };
            }
            v |= ((b[0] & 0x7f) as u64) << shift;
            if b[0] & 0x80 == 0 {
                return Ok(Some(v));
            }
        }
        Err(bad("an overlong length"))
    }

    /// The next record, or None at the end (a record cut short by a crash ends it too).
    pub fn next(&mut self) -> io::Result<Option<Record>> {
        let mut kind = [0u8];
        if self.input.read(&mut kind)? == 0 {
            return Ok(None);
        }
        let Some(len) = self.num()? else { return Ok(None) };
        self.payload.resize(len as usize, 0);
        if self.input.read_exact(&mut self.payload).is_err() {
            return Ok(None);
        }
        let mut c = Cur(&self.payload);
        Ok(Some(match kind[0] {
            META => Record::Meta { key: c.text()?, value: c.text()? },
            STR => Record::Str { id: c.id()?, text: c.text()? },
            THREAD => Record::Thread { id: c.id()?, name: c.id()? },
            MODULE => Record::Module { id: c.id()?, path: c.id()?, base: c.num()?, size: c.num()? },
            CODE => Record::Code { start: c.num()?, size: c.num()?, method: c.id()?, tier: Tier::from(c.num()? as u8) },
            STACK => {
                let id = c.id()?;
                let n = c.num()? as usize;
                let mut frames = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    frames.push(match c.byte()? {
                        0 => {
                            let method = c.id()?;
                            let bci = c.num()?.checked_sub(1).map(|x| x as u32);
                            let t = c.byte()?;
                            Frame::Java { method, bci, tier: Tier::from(t & 0x7f), inlined: t & 0x80 != 0 }
                        }
                        1 => Frame::Native { module: c.id()?, offset: c.num()? },
                        _ => Frame::Address(c.num()?),
                    });
                }
                Record::Stack { id, frames }
            }
            SAMPLE => Record::Sample { time: c.num()?, thread: c.id()?, state: State::from(c.num()? as u8), cycles: c.num()?, stack: c.id()? },
            THREAD_CPU => Record::ThreadCpu { time: c.num()?, thread: c.id()?, cycles: c.num()? },
            k => Record::Unknown(k),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip() {
        let records = vec![
            Record::Meta { key: "vm".into(), value: "rava 0.0.2".into() },
            Record::Str { id: 1, text: "java/lang/String.hashCode()I".into() },
            Record::Thread { id: 77, name: 1 },
            Record::Module { id: 2, path: 1, base: 0x7ff6_0000_0000, size: 0x40_0000 },
            Record::Code { start: 0x1234_5678, size: 300, method: 1, tier: Tier::Opt },
            Record::Stack {
                id: 3,
                frames: vec![
                    Frame::Java { method: 1, bci: Some(0), tier: Tier::Opt, inlined: true },
                    Frame::Java { method: 1, bci: None, tier: Tier::Baseline, inlined: false },
                    Frame::Native { module: 2, offset: 0x3d7488 },
                    Frame::Address(u64::MAX),
                ],
            },
            Record::Sample { time: 1_000_000, thread: 77, state: State::Syscall, cycles: 3_000_000, stack: 3 },
            Record::ThreadCpu { time: 2_000_000, thread: 77, cycles: u64::MAX >> 1 },
        ];
        let mut w = Writer::new(Vec::new()).unwrap();
        for r in &records {
            w.write(r).unwrap();
        }
        let bytes = w.out;
        let mut r = Reader::new(&bytes[..]).unwrap();
        let mut got = Vec::new();
        while let Some(x) = r.next().unwrap() {
            got.push(x);
        }
        assert_eq!(got, records);
        // A recording cut short in its last record still reads up to it.
        let mut r = Reader::new(&bytes[..bytes.len() - 2]).unwrap();
        let mut n = 0;
        while r.next().unwrap().is_some() {
            n += 1;
        }
        assert_eq!(n, records.len() - 1);
    }

    #[test]
    fn unknown_kinds_are_skipped() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&[200, 3, 1, 2, 3]);
        let mut w = Writer::new(Vec::new()).unwrap();
        w.write(&Record::Thread { id: 1, name: 2 }).unwrap();
        bytes.extend_from_slice(&w.out[10..]);
        let mut r = Reader::new(&bytes[..]).unwrap();
        assert_eq!(r.next().unwrap(), Some(Record::Unknown(200)));
        assert_eq!(r.next().unwrap(), Some(Record::Thread { id: 1, name: 2 }));
        assert_eq!(r.next().unwrap(), None);
    }
}
