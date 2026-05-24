mod fixtures;

use fixtures::synthetic_rules::{build_plans, full_rematerialize, SC, TYPE};
use horndb_incremental::Zset;

#[test]
fn fixtures_compile_and_basic_closure_works() {
    let plans = build_plans();
    assert_eq!(plans.len(), 3);

    // 3-class hierarchy: A sc B, B sc C. (instance, type, A).
    // Reference closure should derive (instance, type, B) and (instance, type, C),
    // and (A, sc, C).
    let asserted = Zset::from_iter([((10, SC, 20), 1), ((20, SC, 30), 1), ((1, TYPE, 10), 1)]);
    let closure = full_rematerialize(&asserted);

    assert_eq!(closure.get(&(10, SC, 30)), 1);
    assert_eq!(closure.get(&(1, TYPE, 20)), 1);
    assert_eq!(closure.get(&(1, TYPE, 30)), 1);
}
