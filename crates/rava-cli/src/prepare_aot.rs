//! Offline classpath enumeration. Never creates a VM or executes guest code.
use rava_classfile::ClassFile;
use rava_interp::{prepared, ClassCompiler};
use rava_jar::Jar;
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn run(args: &[String]) -> ExitCode {
    match build(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("prepare-aot: {error}");
            ExitCode::FAILURE
        }
    }
}

fn build(args: &[String]) -> Result<()> {
    if args.len() != 2 {
        return Err("usage: rava --prepare-aot <new-output-directory> <classpath>".into());
    }
    let roots: Vec<PathBuf> = std::env::split_paths(&args[1]).collect();
    if roots.is_empty() || roots.iter().any(|p| !p.exists()) {
        return Err("classpath must contain existing directories or JAR files".into());
    }
    let identity = prepared::identity().ok_or("cannot identify compiler")?;
    let output = PathBuf::from(&args[0]);
    std::fs::create_dir(&output)?; // Never overwrite an existing build.
    let mut builder = Builder {
        report: std::fs::File::create(output.join("methods.tsv"))?,
        archive: prepared::ArchiveWriter::create(&output.join(prepared::FILE_NAME))?,
        output,
        seen: HashSet::new(),
        compiled: 0,
        refused: 0,
        direct_leaves: 0,
        string_constants: prepared::string_constants(),
    };
    writeln!(builder.report, "class\tmethod\tdescriptor\tstatus\tobject")?;
    for root in roots {
        if root.is_dir() {
            builder.directory(&root)?;
        } else {
            let mut jar = Jar::open(&root)?;
            let mut entries = jar.entries_under("");
            entries.sort();
            for entry in entries {
                // The current loader reads base class entries, not version overlays.
                if entry.ends_with(".class") && !entry.starts_with("META-INF/") {
                    builder.class(&jar.read_entry(&entry)?.ok_or("missing JAR entry")?)?;
                }
            }
        }
    }
    builder.report.flush()?;
    builder.archive.finish()?;
    std::fs::write(
        builder.output.join("configuration"),
        if builder.string_constants {
            "string_constants=1"
        } else {
            "string_constants=0"
        },
    )?;
    std::fs::write(builder.output.join("complete"), identity)?;
    println!(
        "PREPARED classes={} compiled={} refused={} direct_leaves={}",
        builder.seen.len(),
        builder.compiled,
        builder.refused,
        builder.direct_leaves
    );
    Ok(())
}

struct Builder {
    archive: prepared::ArchiveWriter,
    output: PathBuf,
    report: std::fs::File,
    seen: HashSet<String>,
    compiled: usize,
    refused: usize,
    /// Direct-leaf artifacts, counted separately from `compiled` (which keeps its
    /// existing meaning: methods compiled through the normal whole-AOT translator).
    direct_leaves: usize,
    string_constants: bool,
}

impl Builder {
    fn directory(&mut self, path: &Path) -> Result<()> {
        let mut entries = std::fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err("symlink classpath entries are unsupported".into());
            }
            if kind.is_dir() {
                self.directory(&entry.path())?;
            } else if entry.path().extension().is_some_and(|ext| ext == "class") {
                self.class(&std::fs::read(entry.path())?)?;
            }
        }
        Ok(())
    }

    fn class(&mut self, bytes: &[u8]) -> Result<()> {
        let class = ClassFile::parse(bytes)?;
        if !self.seen.insert(class.this_class.clone()) {
            return Ok(());
        }
        let compiler = ClassCompiler::new(&class).with_string_constants(self.string_constants);
        for (index, method) in class.methods.iter().enumerate() {
            let (status, key) = if let Some(obj) = compiler.compile_method(index) {
                let key = compiler
                    .method_key(index)
                    .ok_or("missing artifact identity")?;
                self.archive.insert(&key, &obj.to_bytes())?;
                self.compiled += 1;
                ("compiled", key)
            } else {
                self.refused += 1;
                ("unsupported", String::new())
            };
            writeln!(
                self.report,
                "{}\t{}\t{}\t{status}\t{key}",
                class.this_class, method.name, method.descriptor
            )?;
            // Direct-leaf artifacts are SEPARATE from the normal object count: a
            // method can carry both (a whole-AOT caller body + a direct-call leaf
            // for one of its invokestatic sites) or only one. `compiled=` keeps its
            // exact existing meaning; leaves land under their own `direct_leaf_v2_*`
            // key namespace and their own status row.
            if let Some(leaf) = compiler.compile_direct_leaf_method(index) {
                let leaf_key = compiler
                    .direct_leaf_key(index)
                    .ok_or("missing direct-leaf artifact identity")?;
                self.archive.insert(&leaf_key, &leaf.to_bytes())?;
                self.direct_leaves += 1;
                writeln!(
                    self.report,
                    "{}\t{}\t{}\tdirect-leaf\t{leaf_key}",
                    class.this_class, method.name, method.descriptor
                )?;
            }
        }
        Ok(())
    }
}
