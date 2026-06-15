#[test]
fn invalid_derive_annotations_fail_with_actionable_errors() {
    let tests = trybuild::TestCases::new();
    tests.compile_fail("tests/ui/derive/*.rs");
}
