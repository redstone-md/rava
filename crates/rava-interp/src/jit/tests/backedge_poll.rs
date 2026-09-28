use super::*;

#[cfg(feature = "next-gen-gc")]
#[test]
fn jit_backedge_poll_parks_worker_and_resumes_with_exact_result() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    struct PollGateGuard;
    impl Drop for PollGateGuard {
        fn drop(&mut self) {
            crate::jit::force_jit_backedge_poll_for_test(false);
        }
    }
    crate::jit::force_jit_backedge_poll_for_test(true);
    let _poll_gate = PollGateGuard;

    // Increment i until n. The backward conditional branch is the only poll site.
    let c = code(
        vec![
            0x03, 0x3c, // iconst_0; istore_1
            0x84, 0x01, 0x01, // loop: iinc 1, 1
            0x1b, 0x1a, // iload_1; iload_0
            0xa1, 0xff, 0xfb, // if_icmplt loop (-5)
            0x1b, 0xac, // iload_1; ireturn
        ],
        2,
    );
    let f = compile(&c, "(I)I", &empty_pool(), 0, &mut |_, _| (0, None))
        .expect("counted loop compiles with the stop-only poll");
    let entry = f.ptr as usize;
    let (ready_tx, ready_rx) = mpsc::channel();
    let (start_tx, start_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        crate::parallel::enter_region();
        ready_tx.send(()).unwrap();
        start_rx.recv().unwrap();
        let mut locals = vec![2_000_000, 0];
        let result = unsafe { crate::jit::call_raw(entry as *const u8, &mut locals, 0) };
        crate::parallel::leave_region();
        result
    });
    ready_rx.recv().unwrap();
    crate::parallel::request_stop_for_test();
    start_tx.send(()).unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    while crate::parallel::parked_count() == 0 && Instant::now() < deadline {
        std::thread::yield_now();
    }
    if crate::parallel::parked_count() == 0 {
        crate::parallel::resume();
        let _ = worker.join();
        panic!("compiled backward branch did not park at the requested stop");
    }
    crate::parallel::stop_the_world(1);
    let region_count = crate::parallel::region_count();
    let parked_count = crate::parallel::parked_count();
    crate::parallel::resume();
    let result = worker.join().unwrap();
    assert_eq!(region_count, 1, "worker remains in its region while parked");
    assert_eq!(parked_count, 1, "one compiled worker is parked");
    assert_eq!(result, 2_000_000, "loop result is exact after resume");
}
