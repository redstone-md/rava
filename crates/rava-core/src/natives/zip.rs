//! `java.util.zip` natives: inflate/deflate streams (miniz_oxide), CRC-32 and Adler-32.
//! Stream handles are leaked boxes whose address is the Java-side `long`.

use super::{ret_int, ret_long, ret_void, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::runtime::throw_new;
use crate::thread::{JResult, Thread};
use miniz_oxide::deflate::core::{create_comp_flags_from_zip_params, CompressorOxide};
use miniz_oxide::inflate::stream::InflateState;
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus, StreamResult};

const INF: &str = "java/util/zip/Inflater";
const DEF: &str = "java/util/zip/Deflater";

/// A byte range of a Java `byte[]` (bounds already checked by the JDK caller).
fn java_bytes<'a>(arr: u64, off: u64, len: u64) -> &'a mut [u8] {
    // SAFETY: a byte[] and a range the Java code validated.
    unsafe { &mut obj::array_slice::<u8>(arr as Oop)[off as u32 as usize..][..len as u32 as usize] }
}

fn native_bytes<'a>(addr: u64, len: u64) -> &'a mut [u8] {
    // SAFETY: a direct-buffer address and length supplied by the JDK.
    unsafe { std::slice::from_raw_parts_mut(addr as *mut u8, len as u32 as usize) }
}

/// Packs a stream step as the JDK expects: consumed | written << 31 | finished << 62 | flag << 63.
fn pack(r: &StreamResult, finished: bool, flag: bool) -> u64 {
    r.bytes_consumed as u64 | (r.bytes_written as u64) << 31 | (finished as u64) << 62 | (flag as u64) << 63
}

fn inflate(t: &mut Thread, addr: u64, input: &[u8], output: &mut [u8]) -> JResult<u64> {
    // SAFETY: `addr` is a live InflateState from `init`.
    let state = unsafe { &mut *(addr as *mut InflateState) };
    let r = miniz_oxide::inflate::stream::inflate(state, input, output, MZFlush::None);
    match r.status {
        Ok(MZStatus::StreamEnd) => Ok(pack(&r, true, false)),
        Ok(MZStatus::NeedDict) => Ok(pack(&r, false, true)),
        Ok(MZStatus::Ok) | Err(MZError::Buf) => Ok(pack(&r, false, false)),
        Err(e) => Err(throw_new(t, "java/util/zip/DataFormatException", &format!("invalid stored block lengths ({e:?})"))),
    }
}

struct Deflate {
    c: CompressorOxide,
    finished: bool,
    nowrap: bool,
}

fn deflate(addr: u64, input: &[u8], output: &mut [u8], flush: u64, params: u64) -> u64 {
    // SAFETY: `addr` is a live Deflate from `init`.
    let d = unsafe { &mut *(addr as *mut Deflate) };
    if params & 1 != 0 {
        // Level changes apply; zlib's strategies map onto miniz's single strategy.
        let level = match (params as i32) >> 3 {
            l if l < 0 => 6,
            l => l.min(10) as u8,
        };
        d.c.set_format_and_level(if d.nowrap { DataFormat::Raw } else { DataFormat::Zlib }, level);
    }
    let flush = MZFlush::new(flush as i32).unwrap_or(MZFlush::None);
    let r = miniz_oxide::deflate::stream::deflate(&mut d.c, input, output, flush);
    if matches!(r.status, Ok(MZStatus::StreamEnd)) {
        d.finished = true;
    }
    pack(&r, d.finished, false)
}

pub(super) fn register(r: &mut NativeRegistry) {
    for c in [INF, DEF, "java/util/zip/CRC32", "java/util/zip/Adler32"] {
        r.add(c, "initIDs", "()V", |_, _| ret_void());
    }
    r.add(INF, "init", "(Z)J", |_, a| {
        let format = if a[0] != 0 { DataFormat::Raw } else { DataFormat::Zlib };
        ret_long(Box::into_raw(InflateState::new_boxed(format)) as i64)
    });
    r.add(INF, "reset", "(J)V", |_, a| {
        // SAFETY: a live InflateState.
        let s = unsafe { &mut *(a[0] as *mut InflateState) };
        s.reset_as(miniz_oxide::inflate::stream::MinReset);
        ret_void()
    });
    r.add(INF, "end", "(J)V", |_, a| {
        // SAFETY: ends ownership of the box created by `init`.
        drop(unsafe { Box::from_raw(a[0] as *mut InflateState) });
        ret_void()
    });
    r.add(INF, "getAdler", "(J)I", |_, _| ret_int(1));
    r.add(INF, "inflateBytesBytes", "(J[BII[BII)J", |t, a| {
        Ok(inflate(t, a[1], java_bytes(a[3], a[4], a[5]), java_bytes(a[6], a[7], a[8]))?)
    });
    r.add(INF, "inflateBytesBuffer", "(J[BIIJI)J", |t, a| {
        Ok(inflate(t, a[1], java_bytes(a[3], a[4], a[5]), native_bytes(a[6], a[8]))?)
    });
    r.add(INF, "inflateBufferBytes", "(JJI[BII)J", |t, a| {
        Ok(inflate(t, a[1], native_bytes(a[3], a[5]), java_bytes(a[6], a[7], a[8]))?)
    });
    r.add(INF, "inflateBufferBuffer", "(JJIJI)J", |t, a| {
        Ok(inflate(t, a[1], native_bytes(a[3], a[5]), native_bytes(a[6], a[8]))?)
    });
    r.add(DEF, "init", "(IIZ)J", |_, a| {
        let (level, strategy, nowrap) = (a[0] as i32, a[1] as i32, a[2] != 0);
        let flags = create_comp_flags_from_zip_params(level, if nowrap { -15 } else { 15 }, strategy);
        let d = Deflate { c: CompressorOxide::new(flags), finished: false, nowrap };
        ret_long(Box::into_raw(Box::new(d)) as i64)
    });
    r.add(DEF, "reset", "(J)V", |_, a| {
        // SAFETY: a live Deflate.
        let d = unsafe { &mut *(a[0] as *mut Deflate) };
        d.c.reset();
        d.finished = false;
        ret_void()
    });
    r.add(DEF, "end", "(J)V", |_, a| {
        drop(unsafe { Box::from_raw(a[0] as *mut Deflate) });
        ret_void()
    });
    r.add(DEF, "getAdler", "(J)I", |_, a| ret_int(unsafe { &*(a[0] as *const Deflate) }.c.adler32() as i32));
    r.add(DEF, "deflateBytesBytes", "(J[BII[BIIII)J", |_, a| {
        Ok(deflate(a[1], java_bytes(a[3], a[4], a[5]), java_bytes(a[6], a[7], a[8]), a[9], a[10]))
    });
    r.add(DEF, "deflateBytesBuffer", "(J[BIIJIII)J", |_, a| {
        Ok(deflate(a[1], java_bytes(a[3], a[4], a[5]), native_bytes(a[6], a[8]), a[9], a[10]))
    });
    r.add(DEF, "deflateBufferBytes", "(JJI[BIIII)J", |_, a| {
        Ok(deflate(a[1], native_bytes(a[3], a[5]), java_bytes(a[6], a[7], a[8]), a[9], a[10]))
    });
    r.add(DEF, "deflateBufferBuffer", "(JJIJIII)J", |_, a| {
        Ok(deflate(a[1], native_bytes(a[3], a[5]), native_bytes(a[6], a[8]), a[9], a[10]))
    });
    r.add("java/util/zip/CRC32", "update", "(II)I", |_, a| ret_int(crc(a[0], &[a[1] as u8])));
    r.add("java/util/zip/CRC32", "updateBytes0", "(I[BII)I", |_, a| ret_int(crc(a[0], java_bytes(a[1], a[2], a[3]))));
    r.add("java/util/zip/CRC32", "updateByteBuffer0", "(IJII)I", |_, a| {
        ret_int(crc(a[0], native_bytes(a[1] + a[3] as u32 as u64, a[4])))
    });
    r.add("java/util/zip/Adler32", "update", "(II)I", |_, a| ret_int(adler(a[0], &[a[1] as u8])));
    r.add("java/util/zip/Adler32", "updateBytes", "(I[BII)I", |_, a| ret_int(adler(a[0], java_bytes(a[1], a[2], a[3]))));
    r.add("java/util/zip/Adler32", "updateByteBuffer", "(IJII)I", |_, a| {
        ret_int(adler(a[0], native_bytes(a[1] + a[3] as u32 as u64, a[4])))
    });
}

fn crc(init: u64, data: &[u8]) -> i32 {
    let mut h = crc32fast::Hasher::new_with_initial(init as u32);
    h.update(data);
    h.finalize() as i32
}

fn adler(init: u64, data: &[u8]) -> i32 {
    let mut a = adler2::Adler32::from_checksum(init as u32);
    a.write_slice(data);
    a.checksum() as i32
}
