//! How OpenGL applications reach the GPU. With `-Drava.opengl=vulkan` (or `RAVA_OPENGL=vulkan`)
//! rava asks Mesa for Zink, its OpenGL implementation over Vulkan, when a Vulkan driver is
//! installed; the default leaves the system's OpenGL driver in place (`docs/gui-roadmap.md`).
//! It only sets the Mesa and GLVND environment before anything loads `libGL`, so it serves any
//! program, and variables the user set already win.

const PROPERTY: &str = "rava.opengl";

/// Directories the Vulkan loader searches for driver manifests.
const ICD_DIRS: &[&str] = &["/usr/share/vulkan/icd.d", "/usr/local/share/vulkan/icd.d", "/etc/vulkan/icd.d"];

/// Applies the requested OpenGL route. Runs before the VM starts any thread (the environment is
/// not safe to change while other threads may read it).
pub(super) fn select(requested: Option<&str>) {
    let choice = requested.map(str::to_owned).or_else(|| std::env::var("RAVA_OPENGL").ok());
    if choice.as_deref() != Some("vulkan") {
        return;
    }
    if !vulkan_driver() {
        eprintln!("rava: {PROPERTY}=vulkan: no Vulkan driver is installed; OpenGL stays on the system driver");
        return;
    }
    // Zink for Mesa's GLX and EGL, and Mesa as the GLVND vendor even where another vendor's
    // OpenGL is installed (Zink then drives that vendor's Vulkan driver).
    for (key, value) in [("MESA_LOADER_DRIVER_OVERRIDE", "zink"), ("__GLX_VENDOR_LIBRARY_NAME", "mesa")] {
        if std::env::var_os(key).is_none() {
            std::env::set_var(key, value);
        }
    }
}

/// Whether the Vulkan loader will find a driver: manifests named by the environment, or in its
/// search directories.
fn vulkan_driver() -> bool {
    if ["VK_DRIVER_FILES", "VK_ICD_FILENAMES"].iter().any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty())) {
        return true;
    }
    ICD_DIRS.iter().any(|d| std::fs::read_dir(d).is_ok_and(|mut e| e.any(|f| f.is_ok_and(|f| f.path().extension().is_some_and(|x| x == "json")))))
}

pub(super) fn property(settings: &super::settings::Settings) -> Option<&str> {
    settings.value(PROPERTY)
}
