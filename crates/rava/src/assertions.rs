//! `-ea` / `-da` / `-esa` / `-dsa`: which classes run their `assert` statements.

/// A class or package switch from the command line.
#[derive(Clone, Debug)]
struct Rule {
    /// Dotted name; for a package rule, the package (empty for the unnamed package).
    name: String,
    package: bool,
    enabled: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Assertions {
    user: bool,
    system: bool,
    rules: Vec<Rule>,
}

impl Assertions {
    /// `-ea[:target]` or `-da[:target]`: a class, `pkg...` or `...` (the unnamed package), or
    /// with no target, the default for application classes.
    pub fn switch(&mut self, target: Option<&str>, enabled: bool) {
        match target.filter(|t| !t.is_empty()) {
            None => self.user = enabled,
            Some(t) => match t.strip_suffix("...") {
                Some(package) => self.rules.push(Rule { name: package.into(), package: true, enabled }),
                None => self.rules.push(Rule { name: t.into(), package: false, enabled }),
            },
        }
    }

    /// `-esa` / `-dsa`: the default for system classes.
    pub fn switch_system(&mut self, enabled: bool) {
        self.system = enabled;
    }

    /// Whether `class` (dotted) runs its assertions: the last rule naming it, else the last
    /// rule for its innermost listed package, else the default for its kind (HotSpot's order).
    pub fn enabled(&self, class: &str, system: bool) -> bool {
        if let Some(r) = self.rules.iter().rev().find(|r| !r.package && r.name == class) {
            return r.enabled;
        }
        let mut package = class.rsplit_once('.').map_or("", |(p, _)| p);
        loop {
            if let Some(r) = self.rules.iter().rev().find(|r| r.package && r.name == package) {
                return r.enabled;
            }
            match package.rsplit_once('.') {
                Some((outer, _)) => package = outer,
                None if !package.is_empty() => package = "",
                None => return if system { self.system } else { self.user },
            }
        }
    }

    pub fn user_default(&self) -> bool {
        self.user
    }

    /// The class rules and the package rules, each as (names, enabled) in command-line order
    /// (`java.lang.AssertionStatusDirectives`).
    pub fn directives(&self) -> [(Vec<String>, Vec<bool>); 2] {
        [false, true].map(|package| self.rules.iter().filter(|r| r.package == package).map(|r| (r.name.clone(), r.enabled)).unzip())
    }
}
