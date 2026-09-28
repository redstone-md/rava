//! java.time and the date formatting beside it.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::natives::*;

/// Minimal java.time: Instant/Clock/Duration backed by epoch milliseconds.
pub(crate) fn time_dispatch(vm: &mut Vm, owner: &str, name: &str, args: &[Value]) -> Option<Option<Value>> {
    let make = |vm: &mut Vm, class: &str, millis: i64| -> Value {
        let h = vm.heap.alloc(class);
        vm.heap.put_field(h, "$millis", Value::Long(millis));
        Value::Ref(h)
    };
    let millis_of = |vm: &Vm, v: Option<&Value>| -> i64 {
        match v { Some(Value::Ref(h)) => vm.heap.get_field(*h, "$millis").as_long().unwrap_or(0), _ => 0 }
    };
    match owner {
        "java/time/Instant" => match name {
            "now" => Some(Some(make(vm, "java/time/Instant", epoch_millis()))),
            "ofEpochMilli" => Some(Some(make(vm, "java/time/Instant", args.first().and_then(Value::as_long).unwrap_or(0)))),
            "ofEpochSecond" => Some(Some(make(vm, "java/time/Instant", args.first().and_then(Value::as_long).unwrap_or(0) * 1000))),
            "toEpochMilli" => Some(Some(Value::Long(millis_of(vm, args.first())))),
            "getEpochSecond" => Some(Some(Value::Long(millis_of(vm, args.first()) / 1000))),
            "getNano" => Some(Some(Value::Int(((millis_of(vm, args.first()) % 1000) * 1_000_000) as i32))),
            "toString" => Some(Some(Value::Str(format!("{}ms", millis_of(vm, args.first())).into()))),
            "plusMillis" => Some(Some(make(vm, "java/time/Instant", millis_of(vm, args.first()) + args.get(1).and_then(Value::as_long).unwrap_or(0)))),
            "minusMillis" => Some(Some(make(vm, "java/time/Instant", millis_of(vm, args.first()) - args.get(1).and_then(Value::as_long).unwrap_or(0)))),
            "plusSeconds" => Some(Some(make(vm, "java/time/Instant", millis_of(vm, args.first()) + args.get(1).and_then(Value::as_long).unwrap_or(0) * 1000))),
            "minusSeconds" => Some(Some(make(vm, "java/time/Instant", millis_of(vm, args.first()) - args.get(1).and_then(Value::as_long).unwrap_or(0) * 1000))),
            "isAfter" => Some(Some(Value::Int((millis_of(vm, args.first()) > millis_of(vm, args.get(1))) as i32))),
            "isBefore" => Some(Some(Value::Int((millis_of(vm, args.first()) < millis_of(vm, args.get(1))) as i32))),
            "compareTo" => Some(Some(Value::Int(millis_of(vm, args.first()).cmp(&millis_of(vm, args.get(1))) as i32))),
            "truncatedTo" => Some(Some(args.first().cloned().unwrap_or(Value::Null))),
            _ => None,
        },
        "java/time/Clock" => match name {
            "systemUTC" | "systemDefaultZone" | "system" => Some(Some(Value::Ref(vm.heap.alloc("java/time/Clock")))),
            "millis" => Some(Some(Value::Long(epoch_millis()))),
            "instant" => Some(Some(make(vm, "java/time/Instant", epoch_millis()))),
            _ => None,
        },
        "java/time/format/DateTimeFormatter" => match name {
            "ofPattern" | "ofLocalizedDateTime" | "ofLocalizedTime" | "ofLocalizedDate" => {
                let h = vm.heap.alloc("java/time/format/DateTimeFormatter");
                if let Some(Value::Str(p)) = args.first() { vm.heap.put_field(h, "$pattern", Value::Str(p.clone())); }
                Some(Some(Value::Ref(h)))
            }
            "withZone" | "withLocale" | "withChronology" => Some(Some(args.first().cloned().unwrap_or(Value::Null))),
            "format" => Some(Some(Value::Str(format!("{}ms", millis_of(vm, args.get(1))).into()))),
            "formatTo" => {
                let text = format!("{}ms", millis_of(vm, args.get(1)));
                if let Some(Value::Ref(ap)) = args.get(2) {
                    vm.heap.sb_append(*ap, &text);
                }
                Some(None)
            }
            _ => None,
        },
        "java/time/ZonedDateTime" | "java/time/LocalDateTime" | "java/time/LocalDate"
        | "java/time/LocalTime" | "java/time/OffsetDateTime" | "java/time/Instant2" => {
            match name {
                "now" => Some(Some(make(vm, owner, epoch_millis()))),
                "parse" | "of" | "from" | "ofInstant" => {
                    let h = vm.heap.alloc(owner);
                    vm.heap.put_field(h, "$millis", Value::Long(epoch_millis()));
                    if let Some(Value::Str(s)) = args.first() {
                        vm.heap.put_field_syn(h, crate::heap::Syn::Str, Value::Str(s.clone()));
                    }
                    Some(Some(Value::Ref(h)))
                }
                "ofEpochSecond" => Some(Some(make(vm, owner, args.first().and_then(Value::as_long).unwrap_or(0) * 1000))),
                "toInstant" => Some(Some(make(vm, "java/time/Instant", millis_of(vm, args.first())))),
                "toEpochSecond" => Some(Some(Value::Long(millis_of(vm, args.first()) / 1000))),
                "toEpochMilli" | "toInstant2" => Some(Some(Value::Long(millis_of(vm, args.first())))),
                "toString" | "format" => {
                    let s = match vm.heap.get_field(match args.first() { Some(Value::Ref(h)) => *h, _ => 0 }, "$str") {
                        Value::Str(s) => s.to_string(),
                        _ => format!("{}ms", millis_of(vm, args.first())),
                    };
                    Some(Some(Value::Str(s.into())))
                }
                "isBefore" => Some(Some(Value::Int((millis_of(vm, args.first()) < millis_of(vm, args.get(1))) as i32))),
                "isAfter" => Some(Some(Value::Int((millis_of(vm, args.first()) > millis_of(vm, args.get(1))) as i32))),
                "isEqual" => Some(Some(Value::Int((millis_of(vm, args.first()) == millis_of(vm, args.get(1))) as i32))),
                "compareTo" => Some(Some(Value::Int(millis_of(vm, args.first()).cmp(&millis_of(vm, args.get(1))) as i32))),
                "atZone" | "withZoneSameInstant" | "truncatedTo" | "toLocalDateTime"
                | "toLocalDate" | "toLocalTime" | "withNano" => Some(Some(args.first().cloned().unwrap_or(Value::Null))),
                _ if name.starts_with("plus") => Some(Some(make(vm, owner, millis_of(vm, args.first()) + args.get(1).and_then(Value::as_long).unwrap_or(0)))),
                _ if name.starts_with("minus") => Some(Some(make(vm, owner, millis_of(vm, args.first()) - args.get(1).and_then(Value::as_long).unwrap_or(0)))),
                _ if name.starts_with("get") => Some(Some(Value::Int(0))),
                _ => None,
            }
        }
        "java/util/Calendar" | "java/util/GregorianCalendar" => match name {
            "getInstance" => Some(Some(make(vm, "java/util/GregorianCalendar", epoch_millis()))),
            "<init>" => {
                if let Some(Value::Ref(r)) = args.first() {
                    vm.heap.put_field(*r, "$millis", Value::Long(epoch_millis()));
                }
                Some(None)
            }
            "setTime" | "setTimeInMillis" => {
                if let Some(Value::Ref(r)) = args.first() {
                    let m = match args.get(1) { Some(Value::Long(l)) => *l, o => millis_of(vm, o) };
                    vm.heap.put_field(*r, "$millis", Value::Long(m));
                }
                Some(None)
            }
            "getTime" => Some(Some(make(vm, "java/util/Date", millis_of(vm, args.first())))),
            "getTimeInMillis" => Some(Some(Value::Long(millis_of(vm, args.first())))),
            // get(field): Calendar field constants — YEAR=1, MONTH=2 (0-based!),
            // DAY_OF_MONTH=5, HOUR_OF_DAY=11, MINUTE=12, SECOND=13.
            "get" => {
                let (y, mo, d) = civil_from_days(millis_of(vm, args.first()).div_euclid(86_400_000));
                let secs = millis_of(vm, args.first()).div_euclid(1000).rem_euclid(86400);
                let v = match args.get(1).and_then(Value::as_int).unwrap_or(0) {
                    1 => y as i32,
                    2 => mo as i32 - 1, // Calendar.MONTH is 0-based
                    5 => d as i32,
                    11 => (secs / 3600) as i32,
                    12 => ((secs % 3600) / 60) as i32,
                    13 => (secs % 60) as i32,
                    _ => 0,
                };
                Some(Some(Value::Int(v)))
            }
            "set" | "add" | "setTimeZone" | "setLenient" | "clear" | "roll" => Some(None),
            _ => None,
        },
        "java/text/SimpleDateFormat" | "java/text/DateFormat" => match name {
            "<init>" => {
                if let Some(Value::Ref(r)) = args.first() {
                    let pat = match args.get(1) { Some(Value::Str(s)) => s.to_string(), _ => "EEE MMM dd HH:mm:ss yyyy".to_string() };
                    vm.heap.put_field(*r, "$pattern", Value::Str(pat.into()));
                }
                Some(None)
            }
            "applyPattern" => {
                if let (Some(Value::Ref(r)), Some(Value::Str(s))) = (args.first(), args.get(1)) {
                    vm.heap.put_field(*r, "$pattern", Value::Str(s.clone()));
                }
                Some(None)
            }
            "format" => {
                let pat = str_field_of(vm, args.first(), "$pattern");
                let pat = if pat.is_empty() { "yyyy-MM-dd HH:mm:ss".to_string() } else { pat };
                Some(Some(Value::Str(format_date(&pat, millis_of(vm, args.get(1))).into())))
            }
            "toPattern" => Some(Some(Value::Str(str_field_of(vm, args.first(), "$pattern").into()))),
            "parse" => Some(Some(make(vm, "java/util/Date", epoch_millis()))),
            "setTimeZone" | "setLenient" | "setCalendar" | "setDateFormatSymbols" => Some(None),
            _ => None,
        },
        "java/util/Date" => match name {
            "from" => Some(Some(make(vm, "java/util/Date", millis_of(vm, args.first())))),
            "<init>" => {
                if let Some(Value::Ref(r)) = args.first() {
                    let m = args.get(1).and_then(Value::as_long).unwrap_or_else(epoch_millis);
                    vm.heap.put_field(*r, "$millis", Value::Long(m));
                }
                Some(None)
            }
            "getTime" => Some(Some(Value::Long(millis_of(vm, args.first())))),
            "setTime" => { if let Some(Value::Ref(r)) = args.first() { vm.heap.put_field(*r, "$millis", args.get(1).cloned().unwrap_or(Value::Long(0))); } Some(None) }
            "toInstant" => Some(Some(make(vm, "java/time/Instant", millis_of(vm, args.first())))),
            "before" => Some(Some(Value::Int((millis_of(vm, args.first()) < millis_of(vm, args.get(1))) as i32))),
            "after" => Some(Some(Value::Int((millis_of(vm, args.first()) > millis_of(vm, args.get(1))) as i32))),
            "compareTo" => Some(Some(Value::Int(millis_of(vm, args.first()).cmp(&millis_of(vm, args.get(1))) as i32))),
            "toString" => Some(Some(Value::Str(format!("{}ms", millis_of(vm, args.first())).into()))),
            _ => None,
        },
        "java/time/Duration" => match name {
            "ofMillis" => Some(Some(make(vm, "java/time/Duration", args.first().and_then(Value::as_long).unwrap_or(0)))),
            "ofSeconds" => Some(Some(make(vm, "java/time/Duration", args.first().and_then(Value::as_long).unwrap_or(0) * 1000))),
            "ofNanos" => Some(Some(make(vm, "java/time/Duration", args.first().and_then(Value::as_long).unwrap_or(0) / 1_000_000))),
            "ofMinutes" => Some(Some(make(vm, "java/time/Duration", args.first().and_then(Value::as_long).unwrap_or(0) * 60_000))),
            "ofHours" => Some(Some(make(vm, "java/time/Duration", args.first().and_then(Value::as_long).unwrap_or(0) * 3_600_000))),
            "ofDays" => Some(Some(make(vm, "java/time/Duration", args.first().and_then(Value::as_long).unwrap_or(0) * 86_400_000))),
            "zero" | "ZERO" => Some(Some(make(vm, "java/time/Duration", 0))),
            "toMillis" => Some(Some(Value::Long(millis_of(vm, args.first())))),
            "toNanos" => Some(Some(Value::Long(millis_of(vm, args.first()) * 1_000_000))),
            "getSeconds" | "toSeconds" => Some(Some(Value::Long(millis_of(vm, args.first()) / 1000))),
            "toMinutes" => Some(Some(Value::Long(millis_of(vm, args.first()) / 60_000))),
            "toHours" => Some(Some(Value::Long(millis_of(vm, args.first()) / 3_600_000))),
            "isZero" => Some(Some(Value::Int((millis_of(vm, args.first()) == 0) as i32))),
            "isNegative" => Some(Some(Value::Int((millis_of(vm, args.first()) < 0) as i32))),
            "plus" => Some(Some(make(vm, "java/time/Duration", millis_of(vm, args.first()) + millis_of(vm, args.get(1))))),
            "minus" => Some(Some(make(vm, "java/time/Duration", millis_of(vm, args.first()) - millis_of(vm, args.get(1))))),
            "between" => Some(Some(make(vm, "java/time/Duration", millis_of(vm, args.get(1)) - millis_of(vm, args.first())))),
            "toString" => Some(Some(Value::Str(format!("PT{}S", millis_of(vm, args.first()) as f64 / 1000.0).into()))),
            _ => None,
        },
        _ => None,
    }
}

/// Format epoch-millis (UTC) per a SimpleDateFormat pattern subset
/// (y/M/d/H/m/s/S/E, single-quote literals).
pub(crate) fn format_date(pat: &str, millis: i64) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    const DOW: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"]; // epoch day 0 = Thu
    let secs = millis.div_euclid(1000);
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (y, mo, d) = civil_from_days(days);
    let ms = millis.rem_euclid(1000);
    let dow = DOW[days.rem_euclid(7) as usize];
    let b = pat.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'\'' {
            i += 1;
            if i < b.len() && b[i] == b'\'' { out.push('\''); i += 1; continue; }
            while i < b.len() && b[i] != b'\'' { out.push(b[i] as char); i += 1; }
            i += 1; // closing quote
            continue;
        }
        let mut j = i;
        while j < b.len() && b[j] == c { j += 1; }
        let n = j - i;
        match c {
            b'y' => out.push_str(&if n <= 2 { format!("{:02}", y.rem_euclid(100)) } else { format!("{y:04}") }),
            b'M' => out.push_str(&if n >= 3 { MONTHS[(mo - 1) as usize].to_string() } else { format!("{mo:02}") }),
            b'd' => out.push_str(&format!("{d:02}")),
            b'H' => out.push_str(&format!("{h:02}")),
            b'm' => out.push_str(&format!("{mi:02}")),
            b's' => out.push_str(&format!("{s:02}")),
            b'S' => out.push_str(&format!("{ms:03}")),
            b'E' => out.push_str(dow),
            b'z' | b'Z' => out.push_str("UTC"),
            _ => for _ in 0..n { out.push(c as char); },
        }
        i = j;
    }
    out
}
