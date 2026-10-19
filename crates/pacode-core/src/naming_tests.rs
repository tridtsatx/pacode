use pacode_types::SessionId;

use super::begin_title;

#[test]
fn second_claim_is_refused_until_the_guard_drops() {
    let sid = SessionId::generate();
    let first = begin_title(&sid);
    assert!(first.is_some());
    assert!(begin_title(&sid).is_none(), "request already in flight");
    assert!(
        begin_title(&SessionId::generate()).is_some(),
        "other sessions unaffected"
    );
    drop(first);
    assert!(begin_title(&sid).is_some(), "released after completion");
}
