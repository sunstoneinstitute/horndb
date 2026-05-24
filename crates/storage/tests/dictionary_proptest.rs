use horndb_storage::Dictionary;
use oxrdf::{NamedNode, Term};
use proptest::prelude::*;

fn arb_uri() -> impl Strategy<Value = Term> {
    "[a-z]{1,16}"
        .prop_map(|s| Term::NamedNode(NamedNode::new(format!("http://example.org/{s}")).unwrap()))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn intern_then_lookup_round_trips(uris in proptest::collection::vec(arb_uri(), 1..50)) {
        let dict = Dictionary::new();
        let mut ids = Vec::with_capacity(uris.len());
        for u in &uris {
            ids.push(dict.intern(u).unwrap());
        }
        for (id, u) in ids.iter().zip(uris.iter()) {
            let looked_up = dict.lookup(*id);
            prop_assert_eq!(looked_up.as_ref(), Some(u));
        }
    }

    #[test]
    fn duplicate_interns_collapse(u in arb_uri()) {
        let dict = Dictionary::new();
        let a = dict.intern(&u).unwrap();
        let b = dict.intern(&u).unwrap();
        prop_assert_eq!(a, b);
        prop_assert_eq!(dict.len(), 1);
    }
}
