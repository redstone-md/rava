//! Native formatting tests.

use super::{format_java, Value};

fn f(fmt: &str, args: &[Value]) -> String {
    format_java(fmt, args).unwrap_or_else(|(_, m)| m)
}

/// Every expectation below is the output of the real JVM under `Locale.ROOT`
/// (`java.util.Formatter`), which is the locale `format_java` models.
#[test]
fn conversions_match_java_formatter() {
    // Width + the left-justify flag — the pair reported as printing nothing.
    assert_eq!(f("|%-16s|", &[Value::Str("left".into())]), "|left            |");
    assert_eq!(f("|%16s|", &[Value::Str("right".into())]), "|           right|");
    // Integers, zero-pad, hex.
    assert_eq!(f("%d", &[Value::Int(42)]), "42");
    assert_eq!(f("%08x", &[Value::Int(48879)]), "0000beef");
    // ',' grouping was parsed and then dropped.
    assert_eq!(f("%,d", &[Value::Int(1234567)]), "1,234,567");
    assert_eq!(f("%,d", &[Value::Int(-1234567)]), "-1,234,567");
    assert_eq!(f("%,d", &[Value::Int(999)]), "999");
    // '%.Ns' truncation was ignored.
    assert_eq!(f("%.2s", &[Value::Str("abcdef".into())]), "ab");
    assert_eq!(f("%5.2s", &[Value::Str("abcdef".into())]), "   ab");
}

/// `%f` rounds HALF_UP over the shortest decimal form in Java; Rust's `{:.*}`
/// rounds the exact binary value half-to-even and disagreed on common values.
#[test]
fn float_rounding_is_java_half_up() {
    assert_eq!(f("%.1f", &[Value::Double(0.25)]), "0.3"); // Rust {:.1} gives 0.2
    assert_eq!(f("%.1f", &[Value::Double(9.95)]), "10.0"); // carry past the leading digit
    assert_eq!(f("%.1f", &[Value::Double(3.14159)]), "3.1");
    assert_eq!(f("%.0f", &[Value::Double(0.5)]), "1");
    assert_eq!(f("%.3f", &[Value::Double(2.5)]), "2.500");
    // HALF_UP over the SHORTEST form ("1.005"), not over the exact binary value
    // (1.00499…) — Rust's {:.2} gives -1.00 here, the JVM gives -1.01.
    assert_eq!(f("%.2f", &[Value::Double(-1.005)]), "-1.01");
    assert_eq!(f("%f", &[Value::Double(1.0)]), "1.000000");
    assert_eq!(f("%-8.2f|", &[Value::Double(1.0 / 3.0)]), "0.33    |");
    assert_eq!(f("%,.2f", &[Value::Double(1234567.891)]), "1,234,567.89");
    assert_eq!(f("%.1f", &[Value::Double(f64::NAN)]), "NaN");
    assert_eq!(f("%.1f", &[Value::Double(f64::INFINITY)]), "Infinity");
}

/// Positional (`%N$s`) and literal conversions must survive the flag changes.
#[test]
fn positional_and_literals_unchanged() {
    assert_eq!(
        f("%2$s|%1$s", &[Value::Str("one".into()), Value::Str("two".into())]),
        "two|one"
    );
    assert_eq!(f("%b|%c|%%", &[Value::Int(1), Value::Int('z' as i32)]), "true|z|%");
    assert_eq!(f("a%nb", &[]), "a\nb");
}
