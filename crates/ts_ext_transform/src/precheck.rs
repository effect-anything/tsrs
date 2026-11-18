//! Passes that run after parse and content mapping, before check, plus the
//! single-funnel post-emit JavaScript and source-map rewrite.
//!
//! A content mapper owns the runtime JS of a foreign file. These passes do
//! not. They inspect or rewrite ordinary TypeScript and attach diagnostics or
//! a span map so diagnostics land on the author's text. Emit still belongs to
//! this compiler.

use rustc_hash::FxHashMap;
use tsrs_ast::{Diagnostic, DiagnosticExt, SourceFile};
use tsrs_core::P;
use tsrs_core::tspath::ComparePathsOptions;
use tsrs_sourcemap::{Generator, RawSourceMap, decode_mappings, new_generator};
use tsrs_spanmap::{Feature, Kind, Segment};
pub use tsrs_spanmap::{MAPPER_NAME, emits_javascript};

/// The program's parse hook: run the internal passes on a file the loader
/// just parsed (content mapping has already happened) and hand it back.
///
/// Operator overloads are not rewritten here. Check and LSP type the
/// author's `+` from `Symbol.operatorPlus`. Emit rewrites the calls.
/// The passes only report external diagnostics in place: the returned
/// `P<SourceFile>` (and any binding attached to it) stays in the caller's
/// active region.
#[must_use]
pub fn checked(file: Option<P<SourceFile>>) -> Option<P<SourceFile>> {
    if let Some(file) = file {
        crate::tsover::report_nesting(file);
        crate::typegpu::report_refusals(file);
    }
    file
}

/// Point the related information of a mapper diagnostic at the mapped file,
/// as its primary location is.
pub fn retarget_related(diagnostic: P<Diagnostic>, file: Option<P<SourceFile>>) {
    if diagnostic.related_information().is_empty() {
        return;
    }
    let related = diagnostic
        .related_information()
        .iter()
        .map(|item| {
            let cloned = item.clone_diagnostic();
            cloned.set_file(file);
            cloned
        })
        .collect::<Vec<_>>();
    diagnostic.set_related_info(&related);
}

/// Runtime wrappers for emitted JavaScript. Check does not call this.
///
/// Each file is parsed inside a local scratch [`tsrs_core::arena::Region`]
/// that is freed before returning.
#[must_use]
pub fn emit_javascript(filename: &str, code: &str) -> String {
    emit_output(filename, code).code
}

/// Single-funnel post-emit hook for `Emitter::print_source_file`: rewrites
/// emitted JavaScript (`tsover` + `TypeGPU` metadata) and composes `map_json`
/// through the rewrite when the emitted text changes.
///
/// Returns `None` when `source_file` is a declaration file or when `js_text`
/// is unchanged by the rewrite.
#[must_use]
pub fn rewrite_emitted_file(
    source_file: P<SourceFile>,
    js_text: &str,
    map_json: Option<&str>,
) -> Option<(String, Option<String>)> {
    if source_file.is_declaration_file() {
        return None;
    }
    let next = emit_output(source_file.file_name(), js_text);
    if next.code == js_text {
        return None;
    }
    let next_map = map_json.map(|map_text| rewrite_map_text(map_text, js_text, &next));
    Some((next.code, next_map))
}

/// Single-funnel post-emit hook operating directly on `Emitter::print_source_file`'s
/// output path and optional [`Generator`].
///
/// Fast-paths ordinary files without parsing or serializing `source_map_generator`.
#[must_use]
pub fn rewrite_emitted_with_generator(
    js_file_path: &str,
    js_text: &str,
    source_map_generator: Option<&mut Generator>,
) -> Option<(String, Option<Generator>)> {
    if is_declaration_output(js_file_path)
        || (!crate::tsover::enabled(js_text) && crate::typegpu::is_pruned(js_text))
    {
        return None;
    }
    let next = emit_output(js_file_path, js_text);
    if next.code == js_text {
        return None;
    }
    let next_generator = source_map_generator.and_then(|generator| {
        let raw = generator.raw_source_map();
        if next.segments.is_empty() {
            clear_generator(&raw)
        } else {
            compose_generator(&raw, js_text, &next.code, &next.segments)
        }
    });
    Some((next.code, next_generator))
}

/// Rewrites `writer` and `source_map_generator` in place inside `Emitter::print_source_file`.
pub fn rewrite_emitted_source_map(
    source_file: P<SourceFile>,
    js_file_path: &str,
    writer: &mut (dyn tsrs_printer::EmitTextWriter + 'static),
    source_map_generator: &mut Generator,
) {
    if source_file.is_declaration_file() || is_declaration_output(js_file_path) {
        return;
    }
    let printed_js = writer.string();
    if let Some((rewritten_js, next_generator)) =
        rewrite_emitted_with_generator(js_file_path, &printed_js, Some(source_map_generator))
    {
        writer.clear();
        writer.raw_write(&rewritten_js);
        if let Some(next_generator) = next_generator {
            *source_map_generator = next_generator;
        }
    }
}

/// Rewrites emitted `text` in place inside `Emitter::print_source_file` when no source map is active.
pub fn rewrite_emitted_no_map(source_file: P<SourceFile>, js_file_path: &str, text: &mut String) {
    if !source_file.is_declaration_file()
        && let Some((rewritten_js, _)) = rewrite_emitted_with_generator(js_file_path, text, None)
    {
        *text = rewritten_js;
    }
}

/// Rewrites one emitted JavaScript file (`js`) and its optional source map
/// (`map`), given the file `path` and `original` source text.
///
/// When `js` is unchanged (or `path` is a declaration output), both `js` and
/// `map` pass through untouched.
#[must_use]
pub fn emit_output_file(
    path: &str,
    original: &str,
    js: &str,
    map: Option<&str>,
) -> (String, Option<String>) {
    let _ = original;
    let next = emit_output(path, js);
    if next.code == js {
        return (next.code, map.map(str::to_owned));
    }
    let next_map = map.map(|map_text| rewrite_map_text(map_text, js, &next));
    (next.code, next_map)
}

/// Rewrites API emit outputs `(file name, text)` in emitter order.
///
/// Each JavaScript file is rewritten and its `.map` composed through that
/// rewrite against the text the emitter wrote.
#[must_use]
pub fn rewrite_writes(writes: Vec<(String, String)>) -> Vec<(String, String)> {
    let javascript: FxHashMap<String, String> = writes
        .iter()
        .filter(|(name, _)| {
            !std::path::Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("map"))
        })
        .map(|(name, text)| (name.clone(), text.clone()))
        .collect();
    writes
        .into_iter()
        .map(|(name, text)| {
            if is_source_map(&name) {
                let Some(js) = name.strip_suffix(".map").and_then(|key| javascript.get(key)) else {
                    return (name, text);
                };
                let js_name = name.trim_end_matches(".map");
                let (_, mapped) = emit_output_file(js_name, js, js, Some(&text));
                return (name, mapped.unwrap_or(text));
            }
            if is_javascript_output(&name) {
                let emitted = emit_javascript(&name, &text);
                return (name, emitted);
            }
            (name, text)
        })
        .collect()
}

fn rewrite_map_text(map_text: &str, original_js: &str, next: &EmittedOutput) -> String {
    if next.segments.is_empty() {
        return clear_source_map(map_text).unwrap_or_else(|| map_text.to_owned());
    }
    compose_source_map(map_text, original_js, &next.code, &next.segments)
        .unwrap_or_else(|| map_text.to_owned())
}

/// The rewritten JavaScript and the span segments that produced it.
///
/// `segments` is empty when the text is unchanged. A source map of the
/// original emit composes through them (`compose_source_map`) so a column
/// in the rewritten file still names the author's location.
pub struct EmittedOutput {
    pub code: String,
    /// Empty when `code` is `emit_javascript` of the input with no recorded
    /// shift. The input text is then its own line map.
    pub segments: Vec<Segment>,
}

#[must_use]
pub fn emit_output(filename: &str, code: &str) -> EmittedOutput {
    if is_declaration_output(filename) {
        return EmittedOutput { code: code.to_owned(), segments: Vec::new() };
    }
    let len_i32 = i32::try_from(code.len()).unwrap_or(i32::MAX);
    let (rewritten, segments) = match crate::tsover::rewrite_text(filename, code) {
        Some(rewrite) => (rewrite.code, to_tuples(&rewrite.segments)),
        None => (code.to_owned(), vec![(0, len_i32, 0, len_i32, 0)]),
    };
    let wrapped = crate::typegpu::embed_metadata_js(filename, &rewritten);
    if wrapped == code {
        return EmittedOutput { code: wrapped, segments: Vec::new() };
    }
    // The wrap inserts text, collapses whitespace and hoists marked
    // function statements. Token alignment carries every kept token,
    // including a hoisted one, so the composed map stays exact.
    let segments = crate::align::retarget(&rewritten, &wrapped, &segments);
    EmittedOutput { code: wrapped, segments: from_tuples(&segments) }
}

fn to_tuples(segments: &[Segment]) -> Vec<crate::align::Tuple> {
    segments
        .iter()
        .map(|segment| {
            (
                i32::try_from(segment.virtual_start).unwrap_or(i32::MAX),
                i32::try_from(segment.virtual_end).unwrap_or(i32::MAX),
                i32::try_from(segment.original_start).unwrap_or(i32::MAX),
                i32::try_from(segment.original_end).unwrap_or(i32::MAX),
                i32::from(segment.kind != Kind::VERBATIM),
            )
        })
        .collect()
}

fn from_tuples(tuples: &[crate::align::Tuple]) -> Vec<Segment> {
    tuples
        .iter()
        .map(|&(virtual_start, virtual_end, original_start, original_end, kind)| Segment {
            virtual_start: u32::try_from(virtual_start.max(0)).unwrap_or(0),
            virtual_end: u32::try_from(virtual_end.max(0)).unwrap_or(0),
            original_start: u32::try_from(original_start.max(0)).unwrap_or(0),
            original_end: u32::try_from(original_end.max(0)).unwrap_or(0),
            kind: if kind == 0 { Kind::VERBATIM } else { Kind::ATOM },
            features: Feature::All,
        })
        .collect()
}

fn clear_source_map(text: &str) -> Option<String> {
    let value = tsrs_core::json::unmarshal(text).ok()?;
    let map = RawSourceMap::from_json(&value).ok()?;
    clear_generator(&map).map(|mut generator| generator.string())
}

fn clear_generator(map: &RawSourceMap) -> Option<Generator> {
    if map.version != 3 || map.mappings.is_empty() {
        return None;
    }
    let mut generator =
        new_generator(&map.file, &map.source_root, "", ComparePathsOptions::default());
    let mut sources = Vec::with_capacity(map.sources.len());
    for source in &map.sources {
        sources.push(generator.add_source(source));
    }
    for name in &map.names {
        generator.add_name(name);
    }
    if let Some(contents) = &map.sources_content {
        for (index, content) in contents.iter().enumerate() {
            if let Some(content) = content
                && let Some(&source_idx) = sources.get(index)
            {
                generator.set_source_content(source_idx, content).ok()?;
            }
        }
    }
    Some(generator)
}

fn is_source_map(path: &str) -> bool {
    let path = path.rsplit(['/', '\\']).next().unwrap_or(path);
    path.ends_with(".js.map")
        || path.ends_with(".jsx.map")
        || path.ends_with(".mjs.map")
        || path.ends_with(".cjs.map")
}

/// Moves every generated position in `map_json` from the original emit text
/// into the rewritten text.
fn compose_source_map(
    map_json: &str,
    original_js: &str,
    rewritten_js: &str,
    segments: &[Segment],
) -> Option<String> {
    let value = tsrs_core::json::unmarshal(map_json).ok()?;
    let map = RawSourceMap::from_json(&value).ok()?;
    compose_generator(&map, original_js, rewritten_js, segments)
        .map(|mut generator| generator.string())
}

fn compose_generator(
    map: &RawSourceMap,
    original_js: &str,
    rewritten_js: &str,
    segments: &[Segment],
) -> Option<Generator> {
    if map.version != 3 {
        return None;
    }
    let original_lines = compute_ecma_line_starts(original_js);
    let mut decoder = decode_mappings(&map.mappings);
    let decoded = decoder.values().collect::<Vec<_>>();
    if decoder.error().is_some() {
        return None;
    }
    let mut moved_mappings = Vec::with_capacity(decoded.len());
    for mapping in decoded {
        let generated = line_character_to_offset(
            &original_lines,
            original_js,
            mapping.generated_line,
            mapping.generated_character,
        )?;
        let Some(moved) = original_offset_to_rewritten(segments, generated) else {
            continue;
        };
        let moved = operator_point(segments, moved);
        moved_mappings.push((moved, mapping));
    }
    moved_mappings.sort_by_key(|(moved, _)| *moved);
    moved_mappings.dedup_by_key(|(moved, _)| *moved);

    let rewritten_lines = compute_ecma_line_starts(rewritten_js);
    let mut generator =
        new_generator(&map.file, &map.source_root, "", ComparePathsOptions::default());
    let mut sources = Vec::with_capacity(map.sources.len());
    for source in &map.sources {
        sources.push(generator.add_source(source));
    }
    let mut names = Vec::with_capacity(map.names.len());
    for name in &map.names {
        names.push(generator.add_name(name));
    }
    for (moved, mapping) in moved_mappings {
        let (line, column) = offset_to_line_character(&rewritten_lines, rewritten_js, moved);
        if mapping.is_source_mapping() {
            let source_idx = mapping.source_index.get()? as usize;
            let source = *sources.get(source_idx)?;
            let source_line = mapping.source_line?;
            let source_character = mapping.source_character?;
            if let Some(name_idx) = mapping.name_index.get() {
                let name = *names.get(name_idx as usize)?;
                generator
                    .add_named_source_mapping(
                        line,
                        column,
                        source,
                        source_line,
                        source_character,
                        name,
                    )
                    .ok()?;
            } else {
                generator
                    .add_source_mapping(line, column, source, source_line, source_character)
                    .ok()?;
            }
        } else {
            generator.add_generated_mapping(line, column).ok()?;
        }
    }
    if let Some(contents) = &map.sources_content {
        for (index, content) in contents.iter().enumerate() {
            if let Some(content) = content
                && let Some(&source_idx) = sources.get(index)
            {
                generator.set_source_content(source_idx, content).ok()?;
            }
        }
    }
    Some(generator)
}

fn operator_point(segments: &[Segment], generated: u32) -> u32 {
    let Some(index) = segments
        .iter()
        .position(|segment| generated >= segment.virtual_start && generated < segment.virtual_end)
    else {
        return generated;
    };
    if segments[index].kind != Kind::ATOM {
        return generated;
    }
    let mut start = index;
    while start > 0
        && segments[start - 1].kind == Kind::ATOM
        && segments[start - 1].virtual_end == segments[start].virtual_start
        && segments[start - 1].original_start == segments[start].original_start
    {
        start -= 1;
    }
    segments[start].virtual_start
}

fn original_offset_to_rewritten(segments: &[Segment], offset: u32) -> Option<u32> {
    let mut fallback = None;
    let mut inside = None;
    for segment in segments {
        let covers = offset >= segment.original_start && offset < segment.original_end;
        let boundary = offset == segment.original_end;
        if segment.kind == Kind::VERBATIM {
            if covers || (boundary && fallback.is_none() && inside.is_none()) {
                inside = Some(segment.virtual_start + (offset - segment.original_start));
            }
        } else if covers && fallback.is_none() {
            fallback = Some(segment.virtual_start);
        }
    }
    fallback.or(inside)
}

fn line_character_to_offset(
    line_starts: &[u32],
    text: &str,
    line: u32,
    character: u32,
) -> Option<u32> {
    let start = *line_starts.get(line as usize)? as usize;
    let next = line_starts.get(line as usize + 1).map_or(text.len(), |&next| next as usize);
    let line_text = text.get(start..next.min(text.len()))?;
    let mut seen = 0u32;
    for (index, ch) in line_text.char_indices() {
        if seen >= character {
            return u32::try_from(start + index).ok();
        }
        seen += u32::try_from(ch.len_utf16()).unwrap_or(1);
    }
    u32::try_from(start + line_text.len()).ok()
}

#[must_use]
pub fn offset_to_line_character(line_starts: &[u32], text: &str, offset: u32) -> (u32, u32) {
    let offset = (offset as usize).min(text.len());
    let idx = line_starts.partition_point(|&start| (start as usize) <= offset).saturating_sub(1);
    let start = line_starts.get(idx).copied().unwrap_or(0) as usize;
    let column = utf16_len(text.get(start..offset).unwrap_or(""));
    (u32::try_from(idx).unwrap_or(0), column)
}

#[must_use]
pub fn compute_ecma_line_starts(text: &str) -> Vec<u32> {
    let mut result = vec![0];
    let bytes = text.as_bytes();
    let mut pos = 0;
    while pos < bytes.len() {
        let b = bytes[pos];
        if b < 0x80 {
            pos += 1;
            if b == b'\r' {
                if bytes.get(pos) == Some(&b'\n') {
                    pos += 1;
                }
                result.push(u32::try_from(pos).unwrap_or(u32::MAX));
            } else if b == b'\n' {
                result.push(u32::try_from(pos).unwrap_or(u32::MAX));
            }
        } else {
            let ch = text[pos..].chars().next().unwrap_or('\0');
            pos += ch.len_utf8();
            if ch == '\u{2028}' || ch == '\u{2029}' {
                result.push(u32::try_from(pos).unwrap_or(u32::MAX));
            }
        }
    }
    result
}

#[must_use]
pub fn utf16_len(text: &str) -> u32 {
    u32::try_from(text.encode_utf16().count()).unwrap_or(u32::MAX)
}

#[must_use]
pub fn is_declaration_output(path: &str) -> bool {
    let path = path.rsplit(['/', '\\']).next().unwrap_or(path);
    path.ends_with(".d.ts") || path.ends_with(".d.mts") || path.ends_with(".d.cts")
}

fn is_javascript_output(path: &str) -> bool {
    let path = path.rsplit(['/', '\\']).next().unwrap_or(path);
    if is_declaration_output(path) {
        return false;
    }
    std::path::Path::new(path).extension().is_some_and(|ext| {
        ext.eq_ignore_ascii_case("js")
            || ext.eq_ignore_ascii_case("jsx")
            || ext.eq_ignore_ascii_case("mjs")
            || ext.eq_ignore_ascii_case("cjs")
    })
}
