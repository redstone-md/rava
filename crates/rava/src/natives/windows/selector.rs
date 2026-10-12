//! `sun.nio.ch.WEPoll`, the Windows selector's poller. The JDK links the wepoll library (epoll
//! over the AFD driver) into its natives; here the same interface stands on `WSAPoll`: a poller
//! is the set of registered sockets and interests, and a wait polls them all. Events are
//! `struct epoll_event` as wepoll lays it out (events at 0, the socket at 8, 16 bytes).

use crate::natives::io_shared::io_error;
use crate::natives::{ret_int, ret_long, ret_void, NativeRegistry};
use std::sync::Mutex;
use windows_sys::Win32::Networking::WinSock::{WSAPoll, POLLERR, POLLHUP, POLLNVAL, POLLRDBAND, POLLRDNORM, POLLWRNORM, SOCKET, WSAPOLLFD};

const C: &str = "sun/nio/ch/WEPoll";
const CTL_ADD: i32 = 1;
const CTL_MOD: i32 = 2;
const CTL_DEL: i32 = 3;
const EPOLLIN: u32 = 1;
const EPOLLOUT: u32 = 4;
const EPOLLERR: u32 = 8;
const EPOLLHUP: u32 = 16;
const EPOLLONESHOT: u32 = 1 << 31;
/// errno values wepoll reports.
const ENOENT: i32 = 2;
const EEXIST: i32 = 17;
const EVENT_SIZE: usize = 16;
const SOCKET_OFFSET: usize = 8;

/// Registered sockets and their interests (0 while a one-shot registration is spent).
struct Poller {
    sockets: Mutex<Vec<(SOCKET, u32)>>,
}

fn poller(h: u64) -> &'static Poller {
    // SAFETY: a handle `create` made and `close` has not freed.
    unsafe { &*(h as *const Poller) }
}

fn poll_events(interest: u32) -> i16 {
    let mut e = 0;
    if interest & EPOLLIN != 0 {
        e |= POLLRDNORM | POLLRDBAND;
    }
    if interest & EPOLLOUT != 0 {
        e |= POLLWRNORM;
    }
    e as i16
}

fn epoll_events(revents: i16) -> u32 {
    let r = revents as u16 as i32;
    let mut e = 0;
    if r & (POLLRDNORM | POLLRDBAND) as i32 != 0 {
        e |= EPOLLIN;
    }
    if r & POLLWRNORM as i32 != 0 {
        e |= EPOLLOUT;
    }
    if r & (POLLERR | POLLNVAL) as i32 != 0 {
        e |= EPOLLERR;
    }
    if r & POLLHUP as i32 != 0 {
        e |= EPOLLHUP | EPOLLIN;
    }
    e
}

pub(super) fn register(r: &mut NativeRegistry) {
    // No AF_UNIX sockets: the selector's wakeup pipe is a loopback TCP connection, as on
    // Windows versions without them.
    r.add("sun/nio/ch/UnixDomainSockets", "init", "()Z", |_, _| crate::natives::ret_bool(false));
    r.add(C, "eventSize", "()I", |_, _| ret_int(EVENT_SIZE as i32));
    r.add(C, "eventsOffset", "()I", |_, _| ret_int(0));
    r.add(C, "dataOffset", "()I", |_, _| ret_int(SOCKET_OFFSET as i32));
    r.add(C, "create", "()J", |_, _| ret_long(Box::into_raw(Box::new(Poller { sockets: Mutex::new(Vec::new()) })) as i64));
    r.add(C, "close", "(J)V", |_, a| {
        // SAFETY: the poller `create` leaked, closed once.
        drop(unsafe { Box::from_raw(a[0] as *mut Poller) });
        ret_void()
    });
    // ctl(handle, op, socket, events): 0 or an errno.
    r.add(C, "ctl", "(JIJI)I", |_, a| {
        let (op, s, events) = (a[2] as i32, a[3] as SOCKET, a[5] as u32);
        let mut set = poller(a[0]).sockets.lock().unwrap();
        let at = set.iter().position(|&(x, _)| x == s);
        ret_int(match (op, at) {
            (CTL_ADD, None) => {
                set.push((s, events));
                0
            }
            (CTL_ADD, Some(_)) => EEXIST,
            (CTL_MOD, Some(i)) => {
                set[i].1 = events;
                0
            }
            (CTL_DEL, Some(i)) => {
                set.swap_remove(i);
                0
            }
            _ => ENOENT,
        })
    });
    // wait(handle, events address, max events, timeout ms): the number of events written.
    r.add(C, "wait", "(JJII)I", |t, a| {
        let (p, out, max, timeout) = (poller(a[0]), a[2] as usize, a[4] as usize, a[5] as i32);
        let mut fds: Vec<WSAPOLLFD> =
            p.sockets.lock().unwrap().iter().filter(|&&(_, e)| e & !EPOLLONESHOT != 0).map(|&(s, e)| WSAPOLLFD { fd: s, events: poll_events(e), revents: 0 }).collect();
        if fds.is_empty() {
            // Nothing to watch: wait out the timeout as an empty poll would.
            if timeout != 0 {
                crate::gc::safepoint::blocking(t, || std::thread::sleep(std::time::Duration::from_millis(if timeout < 0 { 10 } else { timeout as u64 })));
            }
            return ret_int(0);
        }
        let (ptr, n) = (fds.as_mut_ptr() as usize, fds.len() as u32);
        // SAFETY: an array of `n` poll entries, alive across the call.
        let r = crate::gc::safepoint::blocking(t, || unsafe { WSAPoll(ptr as *mut WSAPOLLFD, n, timeout) });
        if r < 0 {
            return Err(io_error(t, "WSAPoll failed"));
        }
        let mut set = p.sockets.lock().unwrap();
        let mut written = 0;
        for fd in fds.iter().filter(|f| f.revents != 0) {
            if written == max {
                break;
            }
            let events = epoll_events(fd.revents);
            // SAFETY: the caller's array of `max` events.
            unsafe {
                *((out + written * EVENT_SIZE) as *mut u32) = events;
                *((out + written * EVENT_SIZE + SOCKET_OFFSET) as *mut u64) = fd.fd as u64;
            }
            written += 1;
            if let Some(entry) = set.iter_mut().find(|(s, e)| *s == fd.fd && e & EPOLLONESHOT != 0) {
                entry.1 = EPOLLONESHOT;
            }
        }
        ret_int(written as i32)
    });
}
