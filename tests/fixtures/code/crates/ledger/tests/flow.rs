//! An integration test of the crate.

fn opened_book() -> u32 {
    0
}

#[test]
fn a_book_opens() {
    assert_eq!(opened_book(), 0);
}
