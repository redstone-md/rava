//! `diff`: the self or inclusive time of two recordings per frame, in CPU seconds, sorted by
//! how much more the second spends.

use crate::model::Profile;
use crate::report::{totals, View};
use crate::symbols::Symbols;

pub fn diff(a: &Profile, b: &Profile, v: &View, inclusive: bool, sym: &mut Symbols) {
    let (ta, tb) = (totals(a, v, sym), totals(b, v, sym));
    let (ma, mb) = if inclusive { (&ta.inclusive, &tb.inclusive) } else { (&ta.own, &tb.own) };
    let mut keys: Vec<&String> = ma.keys().chain(mb.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut rows: Vec<(f64, f64, &String)> = keys.into_iter().map(|k| (ma.get(k).copied().unwrap_or(0.0), mb.get(k).copied().unwrap_or(0.0), k)).collect();
    rows.sort_by(|x, y| (y.1 - y.0).abs().total_cmp(&(x.1 - x.0).abs()));
    println!("total {:.2} vs {:.2} {} ({})", ta.total, tb.total, v.unit(), if inclusive { "inclusive" } else { "self" });
    println!("{:>9} {:>9} {:>9}  frame", "a", "b", "b - a");
    for (x, y, k) in rows.into_iter().take(v.top) {
        println!("{x:9.2} {y:9.2} {:+9.2}  {k}", y - x);
    }
}
