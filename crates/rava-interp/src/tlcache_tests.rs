//! tlcache_tests.

use super::tlcache;

/// Within one thread a put then get round-trips (the cache works at all).
#[test]
fn field_cache_round_trips_on_same_thread() {
    let site = (0xAAAA_0001usize, 7u16);
    assert_eq!(tlcache::field_get(site), None);
    tlcache::field_put(site, 42, 3);
    assert_eq!(tlcache::field_get(site), Some((42, 3)));
    // overwrite (slot self-correction)
    tlcache::field_put(site, 42, 9);
    assert_eq!(tlcache::field_get(site), Some((42, 9)));
}

/// The whole point of the refactor: one thread's cache entry is INVISIBLE to
/// another thread (no shared mutable map → no data race during concurrent gen).
#[test]
fn caches_are_isolated_per_thread() {
    let site = (0xBBBB_0002usize, 11u16);
    // main thread writes its own entry
    tlcache::field_put(site, 100, 1);
    // also seed a getstatic + call entry to cover those maps
    let gsite = (0xBBBB_0003usize, 5u16);
    tlcache::getstatic_put(gsite, 1, super::Value::Int(123));
    let worker = std::thread::spawn(move || {
        // the worker must NOT observe the main thread's entries
        let f = tlcache::field_get(site);
        let g = tlcache::getstatic_get(gsite, 1);
        // and its own writes stay local
        tlcache::field_put(site, 200, 2);
        (f, g, tlcache::field_get(site))
    });
    let (worker_saw_field, worker_saw_gs, worker_own) = worker.join().unwrap();
    assert_eq!(worker_saw_field, None, "field cache leaked across threads");
    assert!(worker_saw_gs.is_none(), "getstatic cache leaked across threads");
    assert_eq!(worker_own, Some((200, 2)), "worker lost its own entry");
    // main thread's entry is unchanged by the worker's write to the same site
    assert_eq!(tlcache::field_get(site), Some((100, 1)));
}
