//! The class library's version layer. rava's core (interpreter, compilers, collector, object
//! model) is the same for every Java release; what differs between releases is the contract
//! between the VM and the class library it runs: native methods added, removed or re-signed,
//! fields the VM fills in, launcher and stack-walking protocols. Every such difference is
//! decided here, once, from the class library that was found (by what the library itself
//! declares where that is observable, else by its release), and the rest of the VM asks this
//! layer instead of probing classes itself. Native methods of different releases coexist in
//! the registry (keyed by class, name and descriptor); the ones a release added say so where
//! they are registered. `docs/jdk-versions.md` lists the releases and their differences.

use std::path::Path;
use std::sync::OnceLock;

/// The releases rava is tested on (`tests/semantics/run.sh` with each one's `JAVA_HOME`).
pub const TESTED: [u32; 2] = [21, 25];
/// The oldest class library rava runs (its natives and protocols start there).
pub const OLDEST: u32 = 21;

/// The class library rava runs.
#[derive(Clone, Copy, Debug)]
pub struct Library {
    /// The release's feature number (`21` for 21.0.12).
    pub feature: u32,
}

static LIBRARY: OnceLock<Library> = OnceLock::new();

/// The class library of this VM (detected from `crate::java_home` if no VM set it).
pub fn library() -> Library {
    *LIBRARY.get_or_init(|| crate::java_home().map_or(Library { feature: OLDEST }, |h| detect(&h)))
}

/// Records the class library at `java_home` as this VM's.
pub fn init(java_home: &Path) -> Library {
    *LIBRARY.get_or_init(|| detect(java_home))
}

/// Reads `JAVA_VERSION` from the runtime's `release` file ("21.0.12", "25"); the oldest
/// supported release when there is none.
pub fn detect(java_home: &Path) -> Library {
    let feature = std::fs::read_to_string(java_home.join("release"))
        .ok()
        .and_then(|text| {
            let line = text.lines().find(|l| l.starts_with("JAVA_VERSION="))?;
            let v = line.trim_start_matches("JAVA_VERSION=").trim_matches('"');
            v.split(['.', '-', '+']).next()?.parse().ok()
        })
        .unwrap_or(OLDEST);
    Library { feature }
}

impl Library {
    /// StackWalker frames are `ClassFrameInfo`/`StackFrameInfo` objects naming their class or
    /// `ResolvedMethodName` in `classOrMemberName`, expanded on demand (JDK 22); before, a
    /// `StackFrameInfo` held a `MemberName` the VM initialized. Decided by the library's own
    /// classes rather than its release number, so a build that backported the change (or
    /// whose `release` file is missing or wrong) still gets the protocol it implements.
    pub fn class_frame_info(self) -> bool {
        static HAS: OnceLock<bool> = OnceLock::new();
        *HAS.get_or_init(|| crate::loader::boot_class!("java/lang/ClassFrameInfo").is_ok())
    }

    /// The offset of an instance field the class library may or may not declare (fields the
    /// VM fills in that a release added), `None` when the class or the field is absent.
    pub fn optional_field(self, cell: &'static OnceLock<u32>, class: &str, name: &str, desc: &str) -> Option<u32> {
        let off = *cell.get_or_init(|| {
            let Ok(k) = crate::loader::load(class) else { return u32::MAX };
            crate::link::find_field(k, crate::symbol::Symbol::intern(name), crate::symbol::Symbol::intern(desc)).map_or(u32::MAX, |(_, f)| f.offset)
        });
        (off != u32::MAX).then_some(off)
    }
}
