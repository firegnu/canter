pub fn welcome() -> String {
    crate::old_name("one")
}

#[test]
fn welcomes_one() {
    assert_eq!(welcome(), "Hello, one!");
}
