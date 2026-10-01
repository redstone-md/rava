//! The VM options a command line sets, before a JDK is chosen.

use crate::assertions::Assertions;
use crate::vm::VmOptions;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Default)]
pub(super) struct Settings {
    pub class_path: Option<String>,
    pub boot_append: Vec<PathBuf>,
    pub assertions: Assertions,
    pub max_heap: Option<usize>,
    pub preview: bool,
    properties: Vec<(String, String)>,
    numbered: HashMap<&'static str, usize>,
}

impl Settings {
    pub fn property(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.properties.push((key.into(), value.into()));
    }

    /// A repeatable module option as HotSpot passes it on: `prefix.0`, `prefix.1`, ...
    pub fn numbered(&mut self, prefix: &'static str, value: &str) {
        let n = self.numbered.entry(prefix).or_default();
        self.properties.push((format!("{prefix}.{n}"), value.into()));
        *n += 1;
    }

    /// With `jar`, the jar alone is the class path, as `java -jar` has it.
    pub fn into_vm_options(self, java_home: PathBuf, jar: Option<&str>) -> VmOptions {
        let class_path = match jar {
            Some(jar) => jar.to_owned(),
            None => expand_wildcards(&self.class_path.or_else(|| std::env::var("CLASSPATH").ok()).unwrap_or_else(|| ".".into())),
        };
        let mut options = VmOptions::new(java_home);
        options.class_path = class_path;
        options.boot_append = self.boot_append;
        options.properties = self.properties;
        options.assertions = self.assertions;
        options.preview = self.preview;
        if let Some(max) = self.max_heap {
            options.max_heap = max;
        }
        options
    }
}

/// `dir/*` entries become the jars in `dir`, in directory order, as the JDK launcher expands them.
fn expand_wildcards(class_path: &str) -> String {
    let entries: Vec<String> = class_path
        .split(':')
        .flat_map(|entry| match entry.strip_suffix('*').filter(|dir| dir.is_empty() || dir.ends_with('/')) {
            Some(dir) => jars_in(dir),
            None => vec![entry.to_owned()],
        })
        .collect();
    entries.join(":")
}

fn jars_in(dir: &str) -> Vec<String> {
    let listing = std::fs::read_dir(if dir.is_empty() { "." } else { dir });
    listing
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.ends_with(".jar") || name.ends_with(".JAR"))
        .map(|name| format!("{dir}{name}"))
        .collect()
}

/// A `-Xmx`-style size: bytes, or with a k/m/g/t suffix (either case).
pub(super) fn parse_size(s: &str) -> Option<usize> {
    let (digits, unit) = match s.char_indices().last()? {
        (i, c) if c.is_ascii_alphabetic() => (&s[..i], c.to_ascii_lowercase()),
        _ => (s, 'b'),
    };
    let shift = match unit {
        'b' => 0,
        'k' => 10,
        'm' => 20,
        'g' => 30,
        't' => 40,
        _ => return None,
    };
    digits.parse::<usize>().ok()?.checked_mul(1 << shift)
}
