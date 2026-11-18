// Go `internal/spanmap`: the map between a content mapper's virtual text and the original text it was produced
// from (notes/contentmappers.md).

mod spanmap;
pub mod spanmap_ext;

pub use spanmap::*;
pub use spanmap_ext::{Expansion, MapperInfo, SyntaxBinding, SyntaxImport};

/// Internal content-mapper marker used by built-in post-passes that still emit JavaScript.
pub const MAPPER_NAME: &str = "\x00ts-rust-ext";

/// True when this compiler still emits JavaScript for the file.
///
/// An external content mapper owns its runtime output. An internal post-pass
/// uses the same marker only for the span map, so its name is not an owner.
#[must_use]
pub fn emits_javascript(content_mapper: &str) -> bool {
    content_mapper.is_empty() || content_mapper == MAPPER_NAME
}
