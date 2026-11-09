use run_kit::*;

#[test]
fn add_is_exact() {
    assert_eq!(add(2, 3), 5);
    assert_eq!(add(0, 7), 7);
}

#[test]
fn twice_of_two() {
    assert_eq!(twice(2), 4);
}

#[test]
fn counting() {
    leave_a_child_behind();
    assert_eq!(count_to(3), 3);
}

#[test]
fn joining() {
    assert_eq!(join("a", "b"), "ab");
}

#[test]
fn limit_is_ten() {
    assert_eq!(limit(), 10);
}
