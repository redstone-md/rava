//! How OpenGL applications reach the GPU. With `-Drava.opengl=vulkan` (or `RAVA_OPENGL=vulkan`)
//! rava asks Mesa for Zink, its OpenGL implementation over Vulkan, when a Vulkan driver is
//! installed; the default leaves the system's OpenGL driver in place (`docs/gui-roadmap.md`).
//! It acts before anything loads OpenGL, so it serves any program, and variables the user set
//! already win.
//!
//! On Linux it sets the Mesa and GLVND environment for the system's Mesa. Windows has no Mesa of
//! its own: rava loads Mesa's `opengl32.dll` from `lib/mesa` of its installation (or the directory
//! `RAVA_MESA` names) by full path, so every later `LoadLibrary("opengl32.dll")` (GLFW's, LWJGL's)
//! gets that module instead of the system one.

const PROPERTY: &str = "rava.opengl";

/// Applies the requested OpenGL route. Runs before the VM starts any thread (the environment is
/// not safe to change while other threads may read it).
pub(super) fn select(requested: Option<&str>) {
    let choice = requested.map(str::to_owned).or_else(|| std::env::var("RAVA_OPENGL").ok());
    if choice.as_deref() != Some("vulkan") {
        return;
    }
    if let Err(why) = route::zink() {
        eprintln!("rava: {PROPERTY}=vulkan: {why}; OpenGL stays on the system driver");
    }
}

pub(super) fn property(settings: &super::settings::Settings) -> Option<&str> {
    settings.value(PROPERTY)
}

/// Sets `key` unless the user already did.
fn default_var(key: &str, value: &str) {
    if std::env::var_os(key).is_none() {
        std::env::set_var(key, value);
    }
}

#[cfg(unix)]
mod route {
    /// Directories the Vulkan loader searches for driver manifests.
    const ICD_DIRS: &[&str] = &["/usr/share/vulkan/icd.d", "/usr/local/share/vulkan/icd.d", "/etc/vulkan/icd.d"];

    pub(super) fn zink() -> Result<(), String> {
        if !vulkan_driver() {
            return Err("no Vulkan driver is installed".into());
        }
        // Zink for Mesa's GLX and EGL, and Mesa as the GLVND vendor even where another vendor's
        // OpenGL is installed (Zink then drives that vendor's Vulkan driver).
        super::default_var("MESA_LOADER_DRIVER_OVERRIDE", "zink");
        super::default_var("__GLX_VENDOR_LIBRARY_NAME", "mesa");
        Ok(())
    }

    /// Whether the Vulkan loader will find a driver: manifests named by the environment, or in
    /// its search directories.
    fn vulkan_driver() -> bool {
        if ["VK_DRIVER_FILES", "VK_ICD_FILENAMES"].iter().any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty())) {
            return true;
        }
        ICD_DIRS.iter().any(|d| std::fs::read_dir(d).is_ok_and(|mut e| e.any(|f| f.is_ok_and(|f| f.path().extension().is_some_and(|x| x == "json")))))
    }
}

#[cfg(windows)]
mod route {
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::System::LibraryLoader::{LoadLibraryExW, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR};

    pub(super) fn zink() -> Result<(), String> {
        if !Path::new(&system_dir()).join("vulkan-1.dll").is_file() {
            return Err("no Vulkan driver is installed".into());
        }
        let dir = mesa_dir().ok_or("Mesa for Windows (opengl32.dll, libgallium_wgl.dll) is not in lib/mesa of rava's installation; RAVA_MESA can name its directory")?;
        // Mesa's Gallium driver choice, read when the first OpenGL context is made.
        super::default_var("GALLIUM_DRIVER", "zink");
        let path = dir.join("opengl32.dll");
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: a NUL-terminated wide path; the module stays loaded for the process's life.
        // Its own dependencies (libgallium_wgl.dll) come from its directory.
        let h = unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS) };
        if h.is_null() {
            return Err(format!("{}: {}", path.display(), std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// `RAVA_MESA`, else `lib/mesa` beside the `bin` directory of the running executable.
    fn mesa_dir() -> Option<PathBuf> {
        let dir = match std::env::var_os("RAVA_MESA") {
            Some(d) => PathBuf::from(d),
            None => std::env::current_exe().ok()?.parent()?.parent()?.join("lib").join("mesa"),
        };
        dir.join("opengl32.dll").is_file().then_some(dir)
    }

    fn system_dir() -> String {
        let mut buf = [0u16; 260];
        // SAFETY: a buffer of its stated length.
        let n = unsafe { windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
        String::from_utf16_lossy(&buf[..n.min(buf.len())])
    }
}
