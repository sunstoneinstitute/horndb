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
fn unsupported_update_form_errors() {
    let mut s = MemStore::default();
    let u = parse_update("CLEAR DEFAULT").unwrap();
    let err = apply_update(&u, &mut s).unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("unsupported"));
}
