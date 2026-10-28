pub fn welcome() -> String {
    crate::old_name("two")
}

#[test]
fn welcomes_two() {
    assert_eq!(welcome(), "Hello, two!");
}
