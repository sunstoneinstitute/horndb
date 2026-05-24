use reasoner_wcoj::cancel::CancelToken;

#[test]
fn fresh_token_is_not_cancelled() {
    let t = CancelToken::new();
    assert!(!t.is_cancelled());
}

#[test]
fn cancel_propagates_to_clones() {
    let t = CancelToken::new();
    let t2 = t.clone();
    t.cancel();
    assert!(t2.is_cancelled());
}

#[test]
fn check_returns_err_after_cancel() {
    let t = CancelToken::new();
    t.cancel();
    assert!(t.check().is_err());
}
