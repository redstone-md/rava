//! Natives of the Windows class library: files through handles (`java.io`, `WinNTFileSystem`,
//! `sun.nio.fs.WindowsNativeDispatcher`), Winsock, the DNS configuration, the registry,
//! processes and the platform properties.

mod args;
mod async_sock;
mod awt;
mod dns;
mod fs;
mod host;
mod io;
mod iocp;
mod misc;
mod nio_ch;
mod nio_fs;
mod ntsystem;
mod process;
mod registry;
mod security;
mod selector;
mod sockopt;
mod timezone;

pub(super) use host::host;
pub(super) use timezone::{gmt_offset_id, system_zone};

use super::NativeRegistry;

pub(super) fn register(r: &mut NativeRegistry) {
    async_sock::register(r);
    awt::register(r);
    dns::register(r);
    fs::register(r);
    io::register(r);
    iocp::register(r);
    misc::register(r);
    nio_ch::register(r);
    nio_fs::register(r);
    ntsystem::register(r);
    process::register(r);
    registry::register(r);
    security::register(r);
    selector::register(r);
    sockopt::register(r);
}
