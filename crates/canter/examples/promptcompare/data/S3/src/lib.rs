pub fn inclusive_count(start: u32, end: u32) -> u32 {
    end - start
}

#[test]
fn counts_both_endpoints() {
    assert_eq!(inclusive_count(2, 4), 3);
}
