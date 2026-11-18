//! Integration tests for `ts_ext_typegpu`.

mod overload_check;
mod tsover;
mod typegpu;

#[test]
fn the_diff_names_the_files_and_not_a_temp_directory() {
    let text = ts_ext_typegpu::structure::unified("a\nb\n", "a\nc\n");
    assert!(text.starts_with("--- theirs.ts\n+++ ours.ts\n"), "{text}");
    assert!(!text.contains("/var/"), "{text}");
    assert!(!text.contains("2026-"), "{text}");
    assert!(text.contains("-b\n"), "{text}");
    assert!(text.contains("+c\n"), "{text}");
}
