//! Option text outside argv: `JDK_JAVA_OPTIONS`-style variables and `@argfiles`.

/// Splits `text` as the JDK launcher does: whitespace separates, quotes group. In an argfile a
/// `#` outside quotes comments out the rest of the line and `\` escapes inside quotes.
pub(super) fn split(text: &str, argfile: bool) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token: Option<String> = None;
    let mut quote = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), '\\') if argfile => match chars.next() {
                Some('n') => token.get_or_insert_default().push('\n'),
                Some('t') => token.get_or_insert_default().push('\t'),
                Some('r') => token.get_or_insert_default().push('\r'),
                Some('\n') => skip_indent(&mut chars),
                Some(e) => token.get_or_insert_default().push(e),
                None => {}
            },
            (Some(_), c) => token.get_or_insert_default().push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                token.get_or_insert_default();
            }
            (None, '#') if argfile && token.is_none() => {
                chars.by_ref().find(|&c| c == '\n');
            }
            (None, c) if c.is_whitespace() => tokens.extend(token.take()),
            (None, c) => token.get_or_insert_default().push(c),
        }
    }
    tokens.extend(token);
    tokens
}

fn skip_indent(chars: &mut std::str::Chars) {
    while chars.clone().next().is_some_and(|c| c == ' ' || c == '\t') {
        chars.next();
    }
}

/// The arguments in `@path`.
pub(super) fn argfile(path: &str) -> Result<Vec<String>, String> {
    std::fs::read_to_string(path).map(|text| split(&text, true)).map_err(|e| format!("could not open `{path}': {e}"))
}

#[cfg(test)]
mod tests {
    use super::split;

    #[test]
    fn quotes_group_and_comments_skip() {
        assert_eq!(split("-cp 'a b' \"-Dx=1 2\"", false), ["-cp", "a b", "-Dx=1 2"]);
        assert_eq!(split("# c\n-ea   Main\n#x y\n", true), ["-ea", "Main"]);
        assert_eq!(split("\"a\\tb\" ''", true), ["a\tb", ""]);
    }
}
