//! Natives of the Windows class library: files through handles (`java.io`, `WinNTFileSystem`,
//! `sun.nio.fs.WindowsNativeDispatcher`), Winsock, processes and the platform properties.

mod fs;
mod host;
mod io;
mod misc;

pub(super) use host::host;

use super::NativeRegistry;

pub(super) fn register(r: &mut NativeRegistry) {
    fs::register(r);
    io::register(r);
    misc::register(r);
}
