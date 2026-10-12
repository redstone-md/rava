//! Natives of the Windows class library: files through handles (`java.io`, `WinNTFileSystem`,
//! `sun.nio.fs.WindowsNativeDispatcher`), Winsock, processes and the platform properties.

mod args;
mod fs;
mod host;
mod io;
mod misc;
mod nio_ch;
mod nio_fs;
mod process;
mod security;
mod selector;
mod sockopt;

pub(super) use host::host;

use super::NativeRegistry;

pub(super) fn register(r: &mut NativeRegistry) {
    fs::register(r);
    io::register(r);
    misc::register(r);
    nio_ch::register(r);
    nio_fs::register(r);
    process::register(r);
    security::register(r);
    selector::register(r);
    sockopt::register(r);
}
