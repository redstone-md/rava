//! The `java` command line: the same options, environment variables and argfiles.

use super::settings::{parse_size, Settings};
use super::tokens;
use std::collections::VecDeque;

const SOURCE_LAUNCHER: &str = "jdk.compiler/com.sun.tools.javac.launcher.Main";

/// What `main` to run, in the terms of `LauncherHelper.checkAndLoadMain`.
pub(super) enum Target {
    Class(String),
    Jar(String),
    Module(String),
    Source(String),
}

impl Target {
    pub fn mode(&self) -> i32 {
        match self {
            Target::Class(_) => 1,
            Target::Jar(_) => 2,
            Target::Module(_) => 3,
            Target::Source(_) => 4,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Target::Class(n) | Target::Jar(n) | Target::Module(n) => n,
            Target::Source(_) => SOURCE_LAUNCHER,
        }
    }
}

pub(super) struct Launch {
    pub target: Target,
    pub args: Vec<String>,
}

/// Something the launcher prints instead of running a program.
pub(super) enum Query {
    Help { extra: bool, to_stderr: bool },
    ListModules,
    DescribeModule(String),
    ValidateModules,
}

pub(super) struct Version {
    pub to_stderr: bool,
    pub then_exit: bool,
}

pub(super) enum LaunchError {
    /// Rejected by the launcher.
    Launcher(String),
    /// Rejected while creating the VM.
    Vm(String),
}

impl LaunchError {
    pub fn report(&self) {
        match self {
            LaunchError::Launcher(message) => eprintln!("Error: {message}"),
            LaunchError::Vm(message) => eprintln!(
                "{message}\nError: Could not create the Java Virtual Machine.\nError: A fatal exception has occurred. Program will exit."
            ),
        }
    }
}

#[derive(Default)]
pub(super) struct CommandLine {
    pub settings: Settings,
    pub version: Option<Version>,
    pub query: Option<Query>,
    pub show_settings: Option<String>,
    pub dry_run: bool,
    pub launch: Option<Launch>,
    source_version: Option<String>,
}

/// Where arguments come from: an environment variable holds options only.
#[derive(Clone, Copy, PartialEq)]
enum Origin {
    Environment(&'static str),
    Argv,
}

/// The options that take a value: spelling, canonical name, and what java says is missing.
const VALUED: &[(&str, &str, &str)] = &[
    ("-cp", "--class-path", "class path specification"),
    ("-classpath", "--class-path", "class path specification"),
    ("--class-path", "--class-path", "class path specification"),
    ("-jar", "-jar", "jar file specification"),
    ("-m", "--module", "module name"),
    ("--module", "--module", "module name"),
    ("-p", "--module-path", "module path specification"),
    ("--module-path", "--module-path", "module path specification"),
    ("--upgrade-module-path", "--upgrade-module-path", "module path specification"),
    ("--add-modules", "--add-modules", "modules to be specified"),
    ("--enable-native-access", "--enable-native-access", "modules to be specified"),
    ("--limit-modules", "--limit-modules", "modules to be specified"),
    ("--add-exports", "--add-exports", "modules to be specified"),
    ("--add-reads", "--add-reads", "modules to be specified"),
    ("--add-opens", "--add-opens", "modules to be specified"),
    ("--patch-module", "--patch-module", "modules to be specified"),
    ("-d", "--describe-module", "module name"),
    ("--describe-module", "--describe-module", "module name"),
    ("--source", "--source", "source version"),
];

/// HotSpot's `-X` options that need nothing from rava.
const ACCEPTED_X: &[&str] = &[
    "-Xbatch", "-Xcheck:", "-Xcomp", "-Xdebug", "-Xdiag", "-Xint", "-Xinternalversion", "-Xlog", "-Xmaxjitcodesize", "-Xmixed",
    "-Xmn", "-Xms", "-Xnoclassgc", "-Xrs", "-Xshare", "-Xsqnopause", "-Xss", "-Xusealtsigs", "-Xverify",
];

impl CommandLine {
    /// The command line of this process: `JAVA_TOOL_OPTIONS`, `JDK_JAVA_OPTIONS`, the arguments,
    /// then `_JAVA_OPTIONS`, each overriding the ones before.
    pub fn from_env() -> Result<Self, LaunchError> {
        let jdk = environment("JDK_JAVA_OPTIONS", "NOTE: Picked up JDK_JAVA_OPTIONS");
        let tool = environment("JAVA_TOOL_OPTIONS", "Picked up JAVA_TOOL_OPTIONS");
        let underscore = environment("_JAVA_OPTIONS", "Picked up _JAVA_OPTIONS");
        let mut line = CommandLine::default();
        line.parse(tool, Origin::Environment("JAVA_TOOL_OPTIONS"))?;
        line.parse(jdk, Origin::Environment("JDK_JAVA_OPTIONS"))?;
        line.parse(std::env::args().skip(1).collect(), Origin::Argv)?;
        line.parse(underscore, Origin::Environment("_JAVA_OPTIONS"))?;
        Ok(line)
    }

    pub fn jar(&self) -> Option<&str> {
        match &self.launch {
            Some(Launch { target: Target::Jar(jar), .. }) => Some(jar),
            _ => None,
        }
    }

    fn parse(&mut self, args: Vec<String>, origin: Origin) -> Result<(), LaunchError> {
        let mut args = VecDeque::from(args);
        let mut argfiles = true;
        while let Some(arg) = args.pop_front() {
            let arg = match arg.strip_prefix('@') {
                Some(escaped) if escaped.starts_with('@') => escaped.to_owned(),
                Some(path) if argfiles => {
                    tokens::argfile(path).map_err(LaunchError::Launcher)?.into_iter().rev().for_each(|a| args.push_front(a));
                    continue;
                }
                _ => arg,
            };
            if arg == "--disable-@files" {
                argfiles = false;
            } else if !arg.starts_with('-') {
                if let Origin::Environment(name) = origin {
                    return Err(LaunchError::Launcher(format!("Cannot specify main class in environment variable {name}")));
                }
                let target = if self.source_version.is_some() || arg.ends_with(".java") { Target::Source(arg) } else { Target::Class(arg) };
                return self.launch(target, args.into());
            } else if let Some(target) = self.option(&arg, &mut args)? {
                if let Origin::Environment(name) = origin {
                    return Err(LaunchError::Launcher(format!("Option {arg} is not allowed in environment variable {name}")));
                }
                return self.launch(target, args.into());
            }
        }
        Ok(())
    }

    /// Applies one option; returns the target when the option names it (`-jar`, `-m`).
    fn option(&mut self, arg: &str, rest: &mut VecDeque<String>) -> Result<Option<Target>, LaunchError> {
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if arg.starts_with("--") => (name, Some(value.to_owned())),
            _ => (arg, None),
        };
        if let Some(&(spelling, canonical, missing)) = VALUED.iter().find(|(spelling, ..)| *spelling == name) {
            let value = inline.or_else(|| rest.pop_front()).ok_or_else(|| LaunchError::Launcher(format!("{spelling} requires {missing}")))?;
            return Ok(self.valued(canonical, value));
        }
        self.flag(arg)?;
        Ok(None)
    }

    fn valued(&mut self, canonical: &str, value: String) -> Option<Target> {
        let s = &mut self.settings;
        match canonical {
            "--class-path" => s.class_path = Some(value),
            "-jar" => return Some(Target::Jar(value)),
            "--module" => {
                s.property("jdk.module.main", value.split('/').next().unwrap_or_default());
                return Some(Target::Module(value));
            }
            "--module-path" => s.property("jdk.module.path", value),
            "--upgrade-module-path" => s.property("jdk.module.upgrade.path", value),
            "--limit-modules" => s.property("jdk.module.limitmods", value),
            "--add-modules" => s.numbered("jdk.module.addmods", &value),
            "--enable-native-access" => s.numbered("jdk.module.enable.native.access", &value),
            "--add-exports" => s.numbered("jdk.module.addexports", &value),
            "--add-reads" => s.numbered("jdk.module.addreads", &value),
            "--add-opens" => s.numbered("jdk.module.addopens", &value),
            "--patch-module" => s.numbered("jdk.module.patch", &value),
            "--describe-module" => self.query = Some(Query::DescribeModule(value)),
            "--source" => self.source_version = Some(value),
            _ => unreachable!("{canonical}"),
        }
        None
    }

    fn flag(&mut self, arg: &str) -> Result<(), LaunchError> {
        let s = &mut self.settings;
        match arg {
            "-version" | "--version" | "-showversion" | "--show-version" => {
                self.version = Some(Version { to_stderr: !arg.starts_with("--"), then_exit: !arg.contains("show") })
            }
            "-help" | "-h" | "-?" | "--help" => self.query = Some(Query::Help { extra: false, to_stderr: arg != "--help" }),
            "-X" | "--help-extra" => self.query = Some(Query::Help { extra: true, to_stderr: arg == "-X" }),
            "--list-modules" => self.query = Some(Query::ListModules),
            "--validate-modules" => {
                s.property("jdk.module.validation", "true");
                self.query = Some(Query::ValidateModules);
            }
            "--show-module-resolution" => s.property("jdk.module.showModuleResolution", "true"),
            "--dry-run" => self.dry_run = true,
            "--enable-preview" => s.preview = true,
            "-esa" | "-enablesystemassertions" => s.assertions.switch_system(true),
            "-dsa" | "-disablesystemassertions" => s.assertions.switch_system(false),
            "-server" | "-client" | "-verbose" => {}
            _ if arg.starts_with("-XX:ActiveProcessorCount=") => {
                // -1 (HotSpot's default) leaves the count to the system.
                let n = arg["-XX:ActiveProcessorCount=".len()..].parse::<i64>().map_err(|_| LaunchError::Vm(format!("Improperly specified VM option '{}'", &arg[4..])))?;
                crate::set_active_processors(n.max(0) as usize);
            }
            _ if arg.starts_with("--illegal-access=") || arg.starts_with("-verbose:") || arg.starts_with("-splash:") || arg.starts_with("-XX:") => {}
            _ if arg.starts_with("-javaagent:") || arg.starts_with("-agentlib:") || arg.starts_with("-agentpath:") || arg.starts_with("-Xrun") => {
                return Err(LaunchError::Vm(format!("rava does not support JVM TI agents: {arg}")));
            }
            _ => return self.prefixed(arg),
        }
        Ok(())
    }

    fn prefixed(&mut self, arg: &str) -> Result<(), LaunchError> {
        let s = &mut self.settings;
        if let Some(property) = arg.strip_prefix("-D") {
            let (key, value) = property.split_once('=').unwrap_or((property, ""));
            s.property(key, value);
        } else if let Some((enabled, target)) = assertion_switch(arg) {
            s.assertions.switch(target, enabled);
        } else if let Some(size) = arg.strip_prefix("-Xmx") {
            s.max_heap = Some(parse_size(size).ok_or_else(|| LaunchError::Vm(format!("Invalid maximum heap size: {arg}")))?);
        } else if let Some(path) = arg.strip_prefix("-Xbootclasspath/a:") {
            s.boot_append.extend(std::env::split_paths(path));
        } else if arg.starts_with("-XshowSettings") {
            self.show_settings = Some(arg.to_owned());
        } else if !ACCEPTED_X.iter().any(|x| arg.starts_with(x)) {
            return Err(LaunchError::Vm(format!("Unrecognized option: {arg}")));
        }
        Ok(())
    }

    fn launch(&mut self, target: Target, mut args: Vec<String>) -> Result<(), LaunchError> {
        if let Target::Source(file) = &target {
            args.insert(0, file.clone());
            self.settings.numbered("jdk.module.addmods", "ALL-DEFAULT");
            if let Some(version) = self.source_version.take() {
                self.settings.property("jdk.internal.javac.source", version);
            }
        }
        let command = std::iter::once(target.name()).chain(args.iter().map(String::as_str)).collect::<Vec<_>>().join(" ");
        self.settings.property("sun.java.command", command);
        self.launch = Some(Launch { target, args });
        Ok(())
    }
}

/// `-ea[:target]`, `-da[:target]` and their long spellings.
fn assertion_switch(arg: &str) -> Option<(bool, Option<&str>)> {
    let (switch, target) = match arg.split_once(':') {
        Some((switch, target)) => (switch, Some(target)),
        None => (arg, None),
    };
    match switch {
        "-ea" | "-enableassertions" => Some((true, target)),
        "-da" | "-disableassertions" => Some((false, target)),
        _ => None,
    }
}

/// The options in environment variable `name`, announced on stderr as java does.
fn environment(name: &str, announcement: &str) -> Vec<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => {
            eprintln!("{announcement}: {value}");
            tokens::split(&value, false)
        }
        _ => Vec::new(),
    }
}
