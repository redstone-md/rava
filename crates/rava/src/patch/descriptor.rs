//! A patch's descriptor, `rava-patch.txt` at the root of its jar: one directive per line,
//! `#` starts a comment.
//!
//! ```text
//! id parallel-levels
//! version 1
//! open net/minecraft/server/level/ServerChunkCache mainThread
//! redirect a/Target.method(I)V b/Callee.call(J)V -> my/patch/Hooks.call
//! redirect a/Target.* b/Callee.call(J)V -> my/patch/Hooks.call
//! ```

use super::rewrite::MemberRef;

/// An edit of one class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Edit {
    /// Make the class (no member) or its members of that name public and not final.
    Open(Option<String>),
    /// In `method desc` (every method for `*`), call `to_owner.to_name` (static) instead of
    /// `call`.
    Redirect { method: String, desc: String, call: MemberRef, to_owner: String, to_name: String },
}

#[derive(Debug, Default)]
pub(crate) struct Descriptor {
    pub id: String,
    pub version: String,
    /// Target class → its edits, in descriptor order.
    pub edits: Vec<(String, Edit)>,
}

/// `owner.name(desc)` → its parts.
fn member(s: &str) -> Result<MemberRef, String> {
    let paren = s.find('(').ok_or_else(|| format!("{s}: expected owner.name(descriptor)"))?;
    let dot = s[..paren].rfind('.').ok_or_else(|| format!("{s}: expected owner.name(descriptor)"))?;
    Ok(MemberRef { owner: s[..dot].to_string(), name: s[dot + 1..paren].to_string(), desc: s[paren..].to_string() })
}

impl Descriptor {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let mut d = Descriptor::default();
        for (n, line) in text.lines().enumerate() {
            let words: Vec<&str> = line.split('#').next().unwrap_or("").split_whitespace().collect();
            let err = |m: &str| format!("line {}: {m}", n + 1);
            match words.as_slice() {
                [] => {}
                ["id", id] => d.id = id.to_string(),
                ["version", v] => d.version = v.to_string(),
                ["open", class] => d.edits.push((class.to_string(), Edit::Open(None))),
                ["open", class, m] => d.edits.push((class.to_string(), Edit::Open(Some(m.to_string())))),
                ["redirect", target, call, "->", to] => {
                    let target = match target.strip_suffix(".*") {
                        Some(owner) => MemberRef { owner: owner.to_string(), name: "*".into(), desc: String::new() },
                        None => member(target).map_err(|e| err(&e))?,
                    };
                    let call = member(call).map_err(|e| err(&e))?;
                    let dot = to.rfind('.').ok_or_else(|| err("expected Class.method after ->"))?;
                    if call.name == "<init>" {
                        return Err(err("constructor calls cannot be redirected"));
                    }
                    let edit = Edit::Redirect {
                        method: target.name,
                        desc: target.desc,
                        call,
                        to_owner: to[..dot].to_string(),
                        to_name: to[dot + 1..].to_string(),
                    };
                    d.edits.push((target.owner, edit));
                }
                _ => return Err(err(&format!("unknown directive: {}", line.trim()))),
            }
        }
        if d.id.is_empty() || d.version.is_empty() {
            return Err("missing `id` or `version`".into());
        }
        Ok(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_directives() {
        let d = Descriptor::parse("id p # name\nversion 2\nopen a/B f\nredirect a/B.m(I)V c/D.n(J)V -> e/F.g\n").unwrap();
        assert_eq!((d.id.as_str(), d.version.as_str()), ("p", "2"));
        assert_eq!(d.edits[0], ("a/B".into(), Edit::Open(Some("f".into()))));
        let Edit::Redirect { method, desc, call, to_owner, to_name } = &d.edits[1].1 else { panic!() };
        assert_eq!((method.as_str(), desc.as_str(), to_owner.as_str(), to_name.as_str()), ("m", "(I)V", "e/F", "g"));
        assert_eq!(call, &MemberRef { owner: "c/D".into(), name: "n".into(), desc: "(J)V".into() });
        let d = Descriptor::parse("id p\nversion 1\nredirect a/B.* c/D.n(J)V -> e/F.g\n").unwrap();
        assert!(matches!(&d.edits[0].1, Edit::Redirect { method, .. } if method == "*"));
        assert!(Descriptor::parse("open a/B").is_err());
        assert!(Descriptor::parse("id x").is_err());
        assert!(Descriptor::parse("id x\nversion 1\nfrobnicate").is_err());
    }
}
