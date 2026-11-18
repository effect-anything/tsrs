#![expect(
    clippy::assert_is_empty,
    clippy::clone_on_ref_ptr,
    clippy::items_after_statements,
    clippy::iter_on_single_items,
    clippy::needless_raw_string_hashes,
    clippy::range_plus_one,
    clippy::single_char_pattern,
    reason = "sweetener port"
)]

mod catalog;
mod source_reader;
mod sweetener;
mod sweetener_cache;
mod sweetener_expand;
mod sweetener_incremental;
mod sweetener_literal_origins;
mod unit_tests;
mod upstream_gaps;
