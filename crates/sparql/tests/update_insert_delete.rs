use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::parser::parse_update;
use horndb_sparql::update::apply_update;

#[test]
fn insert_data_adds_triple() {
    let mut s = MemStore::default();
    let u = parse_update("INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> }").unwrap();
    apply_update(&u, &mut s).unwrap();
    assert_eq!(s.len(), 1);
}

#[test]
fn delete_data_removes_triple() {
    let mut s = MemStore::default();
    apply_update(
        &parse_update("INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> }").unwrap(),
        &mut s,
    )
    .unwrap();
    assert_eq!(s.len(), 1);
    apply_update(
        &parse_update("DELETE DATA { <http://ex/a> <http://ex/p> <http://ex/b> }").unwrap(),
        &mut s,
    )
    .unwrap();
    assert_eq!(s.len(), 0);
}

#[test]
fn clear_default_is_supported() {
    // `CLEAR DEFAULT` was rejected as an unsupported form in early Stage 1;
    // the graph-management increment (#52) makes it a supported no-op on an
    // empty store. Graph-management coverage lives in `update_graph_mgmt.rs`.
    let mut s = MemStore::default();
    let u = parse_update("CLEAR DEFAULT").unwrap();
    apply_update(&u, &mut s).unwrap();
    assert_eq!(s.len(), 0);
}
