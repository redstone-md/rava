//! Natives of the Windows class library: files through handles (`java.io`, `WinNTFileSystem`,
//! `sun.nio.fs.WindowsNativeDispatcher`), Winsock, processes and the platform properties.

mod args;
mod fs;
mod host;
mod io;
mod misc;
mod nio_ch;
mod nio_fs;
mod security;

pub(super) use host::host;

use super::NativeRegistry;

pub(super) fn register(r: &mut NativeRegistry) {
    fs::register(r);
    io::register(r);
    misc::register(r);
    nio_ch::register(r);
    nio_fs::register(r);
    security::register(r);
}
