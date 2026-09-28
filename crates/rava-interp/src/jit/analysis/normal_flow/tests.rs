use super::*;

fn live(bc: &[u8]) -> Vec<usize> {
    let flow = NormalFlow::compute(bc).expect("valid normal CFG");
    (0..bc.len()).filter(|&pc| flow.contains(pc)).collect()
}

#[test]
fn return_and_throw_do_not_reach_trailing_handler_bytes() {
    assert_eq!(live(&[0x04, 0xac, 0x4b, 0x05, 0xac]), vec![0, 1]);
    assert_eq!(live(&[0x01, 0xbf, 0x4b, 0xb1]), vec![0, 1]);
}

#[test]
fn conditional_keeps_both_paths_and_goto_skips_dead_gap() {
    assert_eq!(live(&[0x1a, 0x99, 0, 5, 0x04, 0xac, 0x05, 0xac]),
        vec![0, 1, 4, 5, 6, 7]);
    assert_eq!(live(&[0xa7, 0, 5, 0x04, 0xac, 0x05, 0xac]), vec![0, 5, 6]);
}

#[test]
fn backward_edges_settle_and_can_reach_lower_pc_blocks() {
    // entry -> 5 -> 3 -> return, with no fall-through from entry's goto.
    assert_eq!(live(&[0xa7, 0, 5, 0x04, 0xac, 0xa7, 0xff, 0xfe]), vec![0, 3, 4, 5]);
    assert_eq!(live(&[0xa7, 0, 0]), vec![0]);
}

#[test]
fn switch_visits_default_and_all_cases_without_fallthrough() {
    let mut bc = vec![0x1a, 0xab, 0, 0];
    for value in [22i32, 1, 7, 24] { bc.extend(value.to_be_bytes()); }
    // switch ends at 20; 20..22 is a dead gap, targets are 23 and 25.
    bc.extend([0x04, 0xac, 0x00, 0x05, 0xac, 0x06, 0xac]);
    assert_eq!(live(&bc), vec![0, 1, 23, 24, 25, 26]);
}

#[test]
fn invalid_targets_truncation_and_live_falloff_are_refused() {
    for bc in [vec![], vec![0xa7], vec![0xa7, 0, 1], vec![0xa7, 0xff, 0xff],
        vec![0x11, 0], vec![0x03], vec![0x03, 0x99, 0xff, 0xff]]
    {
        assert!(NormalFlow::compute(&bc).is_none(), "invalid bytecode {bc:?}");
    }
}
