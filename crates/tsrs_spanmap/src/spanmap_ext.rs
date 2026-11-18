//! Span map additions for macro expansions and cross-segment virtual ranges.
//!
//! PORT: not in Go. Tracks macro expansions the mapper reported and maps
//! cross-segment virtual ranges as the union of the original pieces they cover.
//! `spanmap.rs` keeps one field and one call.

use std::cmp::Reverse;
use std::sync::OnceLock;

use tsrs_core::{P, TextRange, new_text_range};

use crate::spanmap::{Fidelity, Kind, MappedSpan, Segment, SpanMap, virtual_to_original_position};

/// One macro invocation in the original and the virtual text it expanded
/// to. `label` names the macro (and its module).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Expansion {
    pub virtual_start: u32,
    pub virtual_end: u32,
    pub original_start: u32,
    pub original_end: u32,
    pub label: String,
}

/// One name a `for syntax` import binds: `imported as local`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct SyntaxBinding {
    pub imported: String,
    pub local: String,
}

/// One `import { ... } from "m" for syntax;` of the original file. The
/// expansion consumes it, so the virtual AST does not show it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct SyntaxImport {
    /// The specifier as written, without quotes.
    pub specifier: String,
    /// The macro module it resolved to; `None` when it did not resolve.
    pub resolved: Option<String>,
    pub bindings: Vec<SyntaxBinding>,
    pub shadows_core: bool,
    /// The whole declaration in the original text.
    pub original_start: u32,
    pub original_end: u32,
}

/// What the content mapper reported beyond the segments: its expansions
/// (sorted by virtual start), the files the transform read, and the file's
/// `for syntax` imports.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct MapperInfo {
    pub expansions: Vec<Expansion>,
    pub dependencies: Vec<String>,
    pub syntax_imports: Vec<SyntaxImport>,
}

impl MapperInfo {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.expansions.is_empty() && self.dependencies.is_empty() && self.syntax_imports.is_empty()
    }
}

fn sort_expansions(expansions: &mut [Expansion]) {
    expansions.sort_by_key(|item| (item.virtual_start, Reverse(item.virtual_end)));
}

impl SpanMap {
    /// This map with the mapper's expansions attached.
    #[must_use]
    pub fn with_expansions(&self, expansions: Vec<Expansion>) -> P<SpanMap> {
        let mut info = self.mapper_info.clone();
        info.expansions = expansions;
        self.with_mapper_info(info)
    }

    /// This map with the mapper's whole report attached.
    #[must_use]
    pub fn with_mapper_info(&self, mut info: MapperInfo) -> P<SpanMap> {
        sort_expansions(&mut info.expansions);
        P::new(SpanMap {
            segments: self.segments.clone(),
            original_index: OnceLock::new(),
            mapper_info: info,
        })
    }

    /// A fresh map with the same segments and mapper report.
    #[must_use]
    pub fn copied(&self) -> P<SpanMap> {
        P::new(SpanMap {
            segments: self.segments.clone(),
            original_index: OnceLock::new(),
            mapper_info: self.mapper_info.clone(),
        })
    }

    /// The mapper's report; empty for a map without one (or no map).
    #[must_use]
    pub fn mapper_info(m: Option<&SpanMap>) -> &MapperInfo {
        static EMPTY: OnceLock<MapperInfo> = OnceLock::new();
        m.map_or_else(|| EMPTY.get_or_init(MapperInfo::default), |m| &m.mapper_info)
    }

    /// The files the transform read; empty when the mapper named none.
    #[must_use]
    pub fn dependencies(m: Option<&SpanMap>) -> &[String] {
        &SpanMap::mapper_info(m).dependencies
    }

    /// Every expansion attached to the map, by virtual start.
    #[must_use]
    pub fn expansions(m: Option<&SpanMap>) -> &[Expansion] {
        &SpanMap::mapper_info(m).expansions
    }

    /// The original file's `for syntax` imports.
    #[must_use]
    pub fn syntax_imports(m: Option<&SpanMap>) -> &[SyntaxImport] {
        &SpanMap::mapper_info(m).syntax_imports
    }

    /// Every expansion whose virtual text contains the virtual range `r`,
    /// innermost first.
    #[must_use]
    pub fn expansions_containing_virtual(m: Option<&SpanMap>, r: TextRange) -> Vec<&Expansion> {
        let Some(m) = m else {
            return Vec::new();
        };
        let (Some(start), Some(end)) = (r.source_pos(), r.source_end()) else {
            return Vec::new();
        };
        let mut found: Vec<&Expansion> = m
            .mapper_info
            .expansions
            .iter()
            .filter(|item| item.virtual_start <= start && end <= item.virtual_end)
            .filter(|item| item.virtual_start < item.virtual_end)
            .collect();
        found.sort_by_key(|item| item.virtual_end - item.virtual_start);
        found
    }

    /// Every expansion whose invocation contains original position `pos`,
    /// innermost first.
    #[must_use]
    pub fn expansions_at_original(m: Option<&SpanMap>, pos: u32) -> Vec<&Expansion> {
        let Some(m) = m else {
            return Vec::new();
        };
        let mut found: Vec<&Expansion> = m
            .mapper_info
            .expansions
            .iter()
            .filter(|item| item.original_start <= pos && pos < item.original_end)
            .collect();
        found.sort_by_key(|item| item.original_end - item.original_start);
        found
    }

    /// Every original piece a virtual range covers, in virtual order.
    ///
    /// A verbatim segment contributes the exact slice under the range, an atom
    /// its whole original span. Synthesized gaps contribute nothing. Adjacent
    /// verbatim pieces that continue each other in the original are merged.
    #[must_use]
    pub fn virtual_to_original_spans(m: Option<&SpanMap>, r: TextRange) -> Vec<MappedSpan> {
        let Some(m) = m else {
            return vec![MappedSpan { span: r, fidelity: Fidelity::Exact }];
        };
        let (start, range_end) = source_range(r);
        let end = range_end.max(start);
        if start == end {
            let (position, fidelity) = virtual_to_original_position(Some(m), start);
            if fidelity == Fidelity::None {
                return Vec::new();
            }
            return vec![MappedSpan { span: new_text_range(position, position), fidelity }];
        }
        covered_pieces(&m.segments, start, end)
    }
}

/// Maps a cross-segment virtual range to the union of every covered piece.
///
/// Go maps a cross-segment range by its two endpoints, which collapses or
/// inverts it when the covered segments' originals are out of order (macro
/// output reorders its operands). The span is the union of every covered
/// piece, and exact when it is one verbatim piece with no synthesized gap.
/// `low`/`high` are Go's endpoint mappings; `inside` is whether both
/// endpoints fall in segments.
#[must_use]
pub fn union_span(
    segments: &[Segment],
    (virtual_start, virtual_end, low, high): (u32, u32, u32, u32),
    inside: bool,
) -> (TextRange, Fidelity) {
    let pieces = covered_pieces(segments, virtual_start, virtual_end);
    let (mut orig_start, mut orig_end) = (low, high);
    if inside && let Some(first) = pieces.first() {
        let (first_pos, first_end) = source_range(first.span);
        orig_start = first_pos;
        orig_end = first_end;
    }
    for piece in &pieces {
        let (piece_pos, piece_end) = source_range(piece.span);
        orig_start = orig_start.min(piece_pos);
        orig_end = orig_end.max(piece_end);
    }
    let exact = inside
        && covered_length(segments, virtual_start, virtual_end)
            == virtual_end.saturating_sub(virtual_start)
        && matches!(pieces.as_slice(), [only] if only.fidelity == Fidelity::Exact);
    let fidelity = if exact { Fidelity::Exact } else { Fidelity::Approximate };
    (new_text_range(orig_start, orig_end), fidelity)
}

fn source_range(range: TextRange) -> (u32, u32) {
    (
        range.pos().expect_source("span maps require source ranges"),
        range.end().expect_source("span maps require source ranges"),
    )
}

fn covered_length(segments: &[Segment], start: u32, end: u32) -> u32 {
    let first = segments.partition_point(|segment| segment.virtual_end <= start);
    segments[first..]
        .iter()
        .take_while(|segment| segment.virtual_start < end)
        .map(|segment| {
            end.min(segment.virtual_end).saturating_sub(start.max(segment.virtual_start))
        })
        .sum()
}

fn covered_pieces(segments: &[Segment], start: u32, end: u32) -> Vec<MappedSpan> {
    let first = segments.partition_point(|segment| segment.virtual_end <= start);
    let mut out: Vec<MappedSpan> = Vec::new();
    for segment in &segments[first..] {
        if segment.virtual_start >= end {
            break;
        }
        let lo = start.max(segment.virtual_start);
        let hi = end.min(segment.virtual_end);
        if lo >= hi {
            continue;
        }
        let (piece_pos, piece_end, fidelity) = if segment.kind == Kind::VERBATIM {
            (
                segment.original_start + (lo - segment.virtual_start),
                segment.original_start + (hi - segment.virtual_start),
                Fidelity::Exact,
            )
        } else {
            (segment.original_start, segment.original_end, Fidelity::Atom)
        };
        if let Some(last) = out.last_mut()
            && last.fidelity == Fidelity::Exact
            && fidelity == Fidelity::Exact
        {
            let (last_pos, last_end) = source_range(last.span);
            if last_end == piece_pos {
                last.span = new_text_range(last_pos, piece_end);
                continue;
            }
        }
        out.push(MappedSpan { span: new_text_range(piece_pos, piece_end), fidelity });
    }
    out
}
