//! `CLEAR` / `DROP GRAPH` sweep the tier one level below `apply_quads`; the
//! sweep must still reach the write-ahead log, or the batch after it cannot
//! replay and the store never opens again (SPEC-25 S3, PR #345 review).

use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::parser::parse_update;
use horndb_sparql::update::apply_update;
use horndb_storage::{Store, DEFAULT_GRAPH};

fn run(u: &str, b: &mut HornBackend) {
    apply_update(&parse_update(u).unwrap(), b).unwrap();
}

#[test]
fn drop_graph_is_logged_and_replays() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = HornBackend::with_store(Store::open(dir.path()).unwrap());
    run(
        "INSERT DATA { GRAPH <http://g/1> { <http://s> <http://p> <http://o> } \
         <http://s> <http://p> <http://o2> }",
        &mut b,
    );
    run("DROP GRAPH <http://g/1>", &mut b);
    run("INSERT DATA { <http://s> <http://p> <http://o3> }", &mut b);
    std::mem::forget(b);

    let store = Store::open(dir.path()).unwrap();
    assert_eq!(store.tier().graphs(), vec![DEFAULT_GRAPH]);
    assert_eq!(store.tier().triple_count(), 2);
}
