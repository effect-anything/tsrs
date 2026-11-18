//! Operator overload rewrite. Not part of the typescript-go port.
//!
//! This is the `tsover` half, written into this compiler instead of shipping
//! a forked `tsc`. `+`, `-`, `*`, `/`, `%`, and `**` become `__tsover_*`
//! calls. The runtime then dispatches `Symbol.operatorPlus` and the rest.
//! `TypeGPU` and `typenet` are consumers. This file does not know about GPU
//! functions, metadata, or WGSL.

use tsrs_ast::{
    ContentMapperSourceFileInfo, ExternalModuleIndicatorOptions, Kind as SyntaxKind, Node,
    SourceFile, SourceFileParseOptions, get_source_file_of_node, is_function_like_declaration,
    new_external_diagnostic,
};
use tsrs_core::arena::Region;
use tsrs_core::tspath::Path;
use tsrs_core::{P, ScriptKind, TextRange, alloc_str};
use tsrs_diagnostics::Category;
use tsrs_parser::{parse_source_file, parse_source_file_owned};
use tsrs_scanner::skip_trivia;
use tsrs_spanmap::{self as spanmap, Feature, Kind, Segment, SpanMap};

/// The content-mapper identity this rewrite attaches to a file it changed.
const MAPPER: &str = crate::MAPPER_NAME;
const NESTING: &str = "Nesting 'use gpu' functions is not allowed";

/// Declarations for the six calls. Check needs them when the file does not
/// already import a library that declares them. Build output must not
/// include them.
pub const PRELUDE: &str = "\
declare function __tsover_add(a: number, b: number): number;\n\
declare function __tsover_sub(a: number, b: number): number;\n\
declare function __tsover_mul(a: number, b: number): number;\n\
declare function __tsover_div(a: number, b: number): number;\n\
declare function __tsover_mod(a: number, b: number): number;\n\
declare function __tsover_pow(a: number, b: number): number;\n";

#[inline]
pub(crate) fn pos_u32(node: P<Node>) -> u32 {
    node.pos().source_offset().unwrap_or(0)
}

#[inline]
pub(crate) fn end_u32(node: P<Node>) -> u32 {
    node.end().source_offset().unwrap_or(0)
}

/// The calls the pinned `TypeGPU` plugin emits: `+ - * / %` and their
/// assignments. `**` is not in its table, so a `"use gpu"` file keeps it and
/// the tinyest AST keeps the operator for the shader printer.
#[must_use]
pub fn name(kind: SyntaxKind) -> Option<(&'static str, bool)> {
    let (name, assign) = match kind {
        SyntaxKind::PlusToken => ("__tsover_add", false),
        SyntaxKind::MinusToken => ("__tsover_sub", false),
        SyntaxKind::AsteriskToken => ("__tsover_mul", false),
        SyntaxKind::SlashToken => ("__tsover_div", false),
        SyntaxKind::PercentToken => ("__tsover_mod", false),
        SyntaxKind::PlusEqualsToken => ("__tsover_add", true),
        SyntaxKind::MinusEqualsToken => ("__tsover_sub", true),
        SyntaxKind::AsteriskEqualsToken => ("__tsover_mul", true),
        SyntaxKind::SlashEqualsToken => ("__tsover_div", true),
        SyntaxKind::PercentEqualsToken => ("__tsover_mod", true),
        _ => return None,
    };
    Some((name, assign))
}

/// [`name`] plus `**`, which `tsover-runtime` installs as `__tsover_pow`.
/// Only a `"use tsover"` file gets it.
#[must_use]
pub fn name_with_pow(kind: SyntaxKind) -> Option<(&'static str, bool)> {
    name(kind).or(match kind {
        SyntaxKind::AsteriskAsteriskToken => Some(("__tsover_pow", false)),
        SyntaxKind::AsteriskAsteriskEqualsToken => Some(("__tsover_pow", true)),
        _ => None,
    })
}

/// True when this file asked for the operator rewrite.
#[must_use]
pub fn enabled(code: &str) -> bool {
    code.contains("use tsover") || code.contains("use gpu")
}

/// Virtual text, span-map segments, and original-file diagnostics for a
/// host outside `tsc`. `None` when `code` has no directive.
#[must_use]
pub fn rewrite_text(filename: &str, code: &str) -> Option<Rewrite> {
    if !enabled(code) {
        return None;
    }
    let region = Region::new(64 << 10);
    let _guard = region.enter();
    let file_name =
        if filename.starts_with('/') { filename.to_owned() } else { format!("/{filename}") };
    let opts = SourceFileParseOptions {
        path: Path::from(file_name.clone()),
        file_name,
        external_module_indicator_options: ExternalModuleIndicatorOptions::default(),
    };
    let kind = if std::path::Path::new(filename)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("tsx") || ext.eq_ignore_ascii_case("jsx"))
    {
        ScriptKind::TSX
    } else {
        ScriptKind::TS
    };
    let file = parse_source_file_owned(opts, code.to_owned(), kind);
    let (virtual_code, segments, nesting) = walk_file(file, code)?;
    Some(Rewrite {
        code: virtual_code,
        segments,
        diagnostics: nesting
            .into_iter()
            .map(|(start, end)| RewriteDiagnostic { message: NESTING.to_owned(), start, end })
            .collect(),
    })
}

/// Result of [`rewrite_text`]. Offsets are bytes of the input text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rewrite {
    pub code: String,
    pub segments: Vec<Segment>,
    pub diagnostics: Vec<RewriteDiagnostic>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewriteDiagnostic {
    pub message: String,
    pub start: u32,
    pub end: u32,
}

/// Rewrite `file` when its text contains `"use gpu"` or `"use tsover"`,
/// allocating the returned [`SourceFile`] in the caller's active region.
#[must_use]
pub fn rewrite_after_parse(opts: &SourceFileParseOptions, file: P<SourceFile>) -> P<SourceFile> {
    let current = file.text();
    if rewriting() || !enabled(current) {
        return file;
    }
    let Some((code, segments, nesting)) = walk_file(file, current) else {
        return file;
    };
    let _guard = RewriteGuard::enter();
    let code = format!("{PRELUDE}{code}");
    let shift = u32::try_from(PRELUDE.len()).unwrap_or(u32::MAX);
    let segments = segments
        .into_iter()
        .map(|mut segment| {
            segment.virtual_start += shift;
            segment.virtual_end += shift;
            segment
        })
        .collect::<Vec<_>>();
    let parsed = parse_source_file(opts.clone(), &code, file.script_kind());
    if !file.content_mapper().is_empty() {
        let prior = file.span_map();
        let composed = compose_maps(prior.as_deref(), &segments);
        let original = file.original_text();
        if parsed.content_mapper().is_empty() {
            parsed.set_content_mapper_info(ContentMapperSourceFileInfo {
                content_mapper: file.content_mapper(),
                transform_identity: file.content_mapper(),
                parse_options: opts.clone(),
                virtual_file_name: alloc_str(&opts.file_name),
                original_text: original,
                span_map: Some(composed),
                diagnostic_directives: &[],
                supplemental_source_files: &[],
                canonical_source_file: None,
            });
        }
        if file.is_bound() {
            tsrs_binder::bind_source_file(parsed);
        }
        return parsed;
    }
    if !nesting.is_empty() {
        let mut diagnostics = parsed.diagnostics().to_vec();
        for (start, end) in nesting {
            diagnostics.push(new_external_diagnostic(
                Some(parsed),
                TextRange::new(start, end),
                MAPPER,
                Category::Error,
                0,
                NESTING,
            ));
        }
        parsed.set_diagnostics(&diagnostics);
    }
    parsed.set_content_mapper_info(ContentMapperSourceFileInfo {
        content_mapper: MAPPER,
        transform_identity: MAPPER,
        parse_options: opts.clone(),
        virtual_file_name: alloc_str(&opts.file_name),
        original_text: current,
        span_map: Some(spanmap::new(&segments)),
        diagnostic_directives: &[],
        supplemental_source_files: &[],
        canonical_source_file: None,
    });
    if file.is_bound() {
        tsrs_binder::bind_source_file(parsed);
    }
    parsed
}

fn compose_maps(prior: Option<&SpanMap>, next: &[Segment]) -> P<SpanMap> {
    let Some(prior) = prior else {
        return spanmap::new(next);
    };
    let mut composed = Vec::new();
    let prior_segments = prior.segments();
    for segment in next {
        if segment.kind == Kind::VERBATIM {
            split_verbatim_through(segment, &prior_segments, &mut composed);
        } else {
            push_atom_through(prior, segment, &mut composed);
        }
    }
    spanmap::new(&composed)
}

fn split_verbatim_through(segment: &Segment, prior: &[Segment], out: &mut Vec<Segment>) {
    let origin = segment.original_start;
    let end = segment.original_end;
    let mut cursor = origin;
    for prev in prior {
        if prev.virtual_end <= cursor {
            continue;
        }
        if prev.virtual_start >= end {
            break;
        }
        if prev.virtual_start > cursor {
            cursor = prev.virtual_start;
        }
        let from = cursor.max(prev.virtual_start);
        let to = end.min(prev.virtual_end);
        if to <= from {
            continue;
        }
        let shift = from - origin;
        let len = to - from;
        let (original_start, original_end, kind) = if prev.kind == Kind::VERBATIM {
            let start = prev.original_start + (from - prev.virtual_start);
            (start, start + len, Kind::VERBATIM)
        } else {
            (prev.original_start, prev.original_end, Kind::ATOM)
        };
        let features = if kind == Kind::VERBATIM {
            segment.features
        } else {
            segment.features.difference(Feature::Rename | Feature::LinkedEditing)
        };
        out.push(Segment {
            virtual_start: segment.virtual_start + shift,
            virtual_end: segment.virtual_start + shift + len,
            original_start,
            original_end,
            kind,
            features,
        });
        cursor = to;
        if cursor >= end {
            break;
        }
    }
}

fn push_atom_through(prior: &SpanMap, segment: &Segment, out: &mut Vec<Segment>) {
    let (original, fidelity) = prior
        .virtual_to_original_span(TextRange::new(segment.original_start, segment.original_end));
    if fidelity.is_none() {
        return;
    }
    out.push(Segment {
        virtual_start: segment.virtual_start,
        virtual_end: segment.virtual_end,
        original_start: original.source_pos().unwrap_or(0),
        original_end: original.source_end().unwrap_or(0),
        kind: Kind::ATOM,
        features: segment.features.difference(Feature::Rename | Feature::LinkedEditing),
    });
}

/// Parse without the `"use gpu"` operator rewrite. The build wrapper needs
/// the original function text, not the rewritten operators.
#[must_use]
pub fn parse_without_rewrite(
    opts: &SourceFileParseOptions,
    text: &str,
    kind: ScriptKind,
) -> P<SourceFile> {
    let _guard = RewriteGuard::enter();
    parse_source_file(opts.clone(), text, kind)
}

fn walk_file(
    file: P<SourceFile>,
    original: &str,
) -> Option<(String, Vec<Segment>, Vec<(u32, u32)>)> {
    let mut walk = Walk { allow_pow: original.contains("use tsover"), ..Walk::default() };
    walk.visit(file.as_node());
    if walk.edits.is_empty() && walk.nesting.is_empty() {
        return None;
    }
    let (code, segments) = apply_edits(original, &walk.edits);
    Some((code, segments, walk.nesting))
}

#[derive(Default)]
struct Walk {
    allow_pow: bool,
    depth: u32,
    edits: Vec<Edit>,
    nesting: Vec<(u32, u32)>,
}

#[derive(Clone)]
struct Edit {
    start: u32,
    end: u32,
    text: String,
    op_start: u32,
    op_end: u32,
    /// Empty for an edit rebuilt inside a copied operand. The outer edit
    /// carries the pieces that name each original token.
    pieces: Vec<Piece>,
}

impl Walk {
    fn visit(&mut self, node: P<Node>) {
        if node.kind() == SyntaxKind::StringLiteral && is_use_tsover(node) {
            if let Some(root) = get_source_file_of_node(node) {
                root.as_node().for_each_child(&mut |child| {
                    self.visit_gpu(child);
                    false
                });
            }
            return;
        }
        if node.kind() == SyntaxKind::StringLiteral
            && is_use_gpu(node)
            && let Some(body) = gpu_body(node)
        {
            if self.depth > 0 {
                self.nesting.push((pos_u32(node), end_u32(node)));
            } else {
                self.depth += 1;
                note_nested(body, &mut self.nesting);
                body.for_each_child(&mut |child| {
                    self.visit_gpu(child);
                    false
                });
                self.depth -= 1;
            }
            return;
        }
        node.for_each_child(&mut |child| {
            self.visit(child);
            false
        });
    }

    fn visit_gpu(&mut self, node: P<Node>) {
        if node.kind() == SyntaxKind::StringLiteral && is_use_gpu(node) {
            return;
        }
        if node.kind() == SyntaxKind::BinaryExpression {
            let bin = node.as_binary_expression();
            let op_kind = bin.operator_token.kind();
            let overloaded = if self.allow_pow { name_with_pow(op_kind) } else { name(op_kind) };
            if let Some((call_name, assign)) = overloaded {
                self.rewrite_binary(node, call_name, assign);
                return;
            }
        }
        node.for_each_child(&mut |child| {
            self.visit_gpu(child);
            false
        });
    }

    fn rewrite_binary(&mut self, node: P<Node>, call_name: &str, assign: bool) {
        let bin = node.as_binary_expression();
        let left_node = bin.left;
        let right_node = bin.right();
        let op = bin.operator_token;
        let before = self.edits.len();
        self.visit_gpu(left_node);
        self.visit_gpu(right_node);
        let nested = self.edits.split_off(before);
        let left = rewritten_slice(node, left_node, &nested);
        let right = rewritten_slice(node, right_node, &nested);
        let text = if assign {
            format!("{left} = {call_name}({left}, {right})")
        } else {
            format!("{call_name}({left}, {right})")
        };
        let source = get_source_file_of_node(node).map_or("", |sf| sf.text());
        let start = skip_trivia(source, pos_u32(node));
        let left_start = skip_trivia(source, pos_u32(left_node));
        let left_end = end_u32(left_node);
        let right_start = skip_trivia(source, pos_u32(right_node));
        let right_end = end_u32(right_node);
        let op_start = skip_trivia(source, pos_u32(op));
        let op_end = end_u32(op);
        let mut pieces = Vec::new();
        if assign {
            push_piece(&mut pieces, &left, left_start, left_end, &nested);
            pieces.push(Piece::Atom {
                text: format!(" = {call_name}("),
                original_start: op_start,
                original_end: op_end,
            });
            push_piece(&mut pieces, &left, left_start, left_end, &nested);
            pieces.push(Piece::Atom {
                text: ", ".to_owned(),
                original_start: op_start,
                original_end: op_end,
            });
            push_piece(&mut pieces, &right, right_start, right_end, &nested);
            pieces.push(Piece::Atom {
                text: ")".to_owned(),
                original_start: op_start,
                original_end: op_end,
            });
        } else {
            pieces.push(Piece::Atom {
                text: format!("{call_name}("),
                original_start: op_start,
                original_end: op_end,
            });
            push_piece(&mut pieces, &left, left_start, left_end, &nested);
            pieces.push(Piece::Atom {
                text: ", ".to_owned(),
                original_start: op_start,
                original_end: op_end,
            });
            push_piece(&mut pieces, &right, right_start, right_end, &nested);
            pieces.push(Piece::Atom {
                text: ")".to_owned(),
                original_start: op_start,
                original_end: op_end,
            });
        }
        self.edits.push(Edit { start, end: end_u32(node), text, op_start, op_end, pieces });
    }
}

#[derive(Clone)]
enum Piece {
    Copied { text: String, original_start: u32, original_end: u32, nested: Vec<Edit> },
    Atom { text: String, original_start: u32, original_end: u32 },
}

fn push_piece(pieces: &mut Vec<Piece>, text: &str, start: u32, end: u32, nested: &[Edit]) {
    let local = nested
        .iter()
        .filter(|edit| edit.start >= start && edit.end <= end)
        .cloned()
        .collect::<Vec<_>>();
    pieces.push(Piece::Copied {
        text: text.to_owned(),
        original_start: start,
        original_end: end,
        nested: local,
    });
}

fn apply_edits(original: &str, edits: &[Edit]) -> (String, Vec<Segment>) {
    let mut ordered = edits.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|edit| edit.start);
    let mut code = String::new();
    let mut segments = Vec::new();
    let mut cursor = 0u32;
    for edit in ordered {
        if edit.start < cursor || edit.end < edit.start {
            continue;
        }
        push_verbatim(&mut code, &mut segments, original, cursor, edit.start);
        if edit.pieces.is_empty() {
            let v0 = u32::try_from(code.len()).unwrap_or(u32::MAX);
            code.push_str(&edit.text);
            let v1 = u32::try_from(code.len()).unwrap_or(u32::MAX);
            if v1 > v0 {
                segments.push(Segment {
                    virtual_start: v0,
                    virtual_end: v1,
                    original_start: edit.op_start,
                    original_end: edit.op_end,
                    kind: Kind::ATOM,
                    features: Feature::All,
                });
            }
        } else {
            push_pieces(&mut code, &mut segments, &edit.pieces);
        }
        cursor = edit.end;
    }
    push_verbatim(
        &mut code,
        &mut segments,
        original,
        cursor,
        u32::try_from(original.len()).unwrap_or(u32::MAX),
    );
    (code, segments)
}

fn pieces_text(nested: &[Edit]) -> String {
    let mut out = String::new();
    for edit in nested {
        out.push_str(&edit.text);
    }
    out
}

fn push_pieces(code: &mut String, segments: &mut Vec<Segment>, pieces: &[Piece]) {
    for piece in pieces {
        match piece {
            Piece::Atom { text, original_start, original_end } => {
                let v0 = u32::try_from(code.len()).unwrap_or(u32::MAX);
                code.push_str(text);
                let v1 = u32::try_from(code.len()).unwrap_or(u32::MAX);
                if v1 > v0 {
                    segments.push(Segment {
                        virtual_start: v0,
                        virtual_end: v1,
                        original_start: *original_start,
                        original_end: *original_end,
                        kind: Kind::ATOM,
                        features: Feature::All,
                    });
                }
            }
            Piece::Copied { text, original_start, original_end, nested } => {
                if nested.is_empty() {
                    let v0 = u32::try_from(code.len()).unwrap_or(u32::MAX);
                    code.push_str(text);
                    let v1 = u32::try_from(code.len()).unwrap_or(u32::MAX);
                    if v1 > v0 {
                        segments.push(Segment {
                            virtual_start: v0,
                            virtual_end: v1,
                            original_start: *original_start,
                            original_end: *original_end,
                            kind: Kind::VERBATIM,
                            features: Feature::All,
                        });
                    }
                } else if pieces_text(nested) == *text {
                    for edit in nested {
                        push_pieces(code, segments, &edit.pieces);
                    }
                    let _ = (original_start, original_end);
                } else {
                    let v0 = u32::try_from(code.len()).unwrap_or(u32::MAX);
                    code.push_str(text);
                    let v1 = u32::try_from(code.len()).unwrap_or(u32::MAX);
                    if v1 > v0 {
                        segments.push(Segment {
                            virtual_start: v0,
                            virtual_end: v1,
                            original_start: *original_start,
                            original_end: *original_end,
                            kind: Kind::ATOM,
                            features: Feature::All,
                        });
                    }
                }
            }
        }
    }
}

fn push_verbatim(
    code: &mut String,
    segments: &mut Vec<Segment>,
    original: &str,
    from: u32,
    to: u32,
) {
    if to <= from {
        return;
    }
    let v0 = u32::try_from(code.len()).unwrap_or(u32::MAX);
    let end = (to as usize).min(original.len());
    let start = (from as usize).min(end);
    code.push_str(&original[start..end]);
    let v1 = u32::try_from(code.len()).unwrap_or(u32::MAX);
    if let Some(prev) = segments.last_mut()
        && prev.kind == Kind::VERBATIM
        && prev.virtual_end == v0
        && prev.original_end == from
    {
        prev.virtual_end = v1;
        prev.original_end = to;
        return;
    }
    segments.push(Segment {
        virtual_start: v0,
        virtual_end: v1,
        original_start: from,
        original_end: to,
        kind: Kind::VERBATIM,
        features: Feature::All,
    });
}

fn rewritten_slice(owner: P<Node>, node: P<Node>, nested: &[Edit]) -> String {
    let source = get_source_file_of_node(owner).map_or("", |sf| sf.text());
    let origin = skip_trivia(source, pos_u32(node));
    let end = end_u32(node);
    let raw_start = origin as usize;
    let raw_end = (end.max(origin) as usize).min(source.len());
    let raw = source[raw_start.min(raw_end)..raw_end].to_owned();
    let local = nested
        .iter()
        .filter(|edit| edit.start >= origin && edit.end <= end)
        .map(|edit| Edit {
            start: edit.start - origin,
            end: edit.end - origin,
            text: edit.text.clone(),
            op_start: edit.op_start,
            op_end: edit.op_end,
            pieces: edit.pieces.clone(),
        })
        .collect::<Vec<_>>();
    if local.is_empty() {
        return raw;
    }
    let raw_len = u32::try_from(raw.len()).unwrap_or(u32::MAX);
    if local.len() == 1 && local[0].end >= raw_len && local[0].start > 0 {
        let start = (local[0].start as usize).min(raw.len());
        let prefix = &raw[..start];
        if call_argument_starts_with(&local[0].text, prefix) {
            return local[0].text.clone();
        }
    }
    apply_edits(&raw, &local).0
}

fn call_argument_starts_with(call: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return false;
    }
    let Some(open) = call.find('(') else {
        return false;
    };
    call[open + 1..].trim_start().starts_with(prefix)
}

fn note_nested(body: P<Node>, nesting: &mut Vec<(u32, u32)>) {
    body.for_each_child(&mut |child| {
        if is_function_like_declaration(child)
            && let Some(inner) = child.body()
            && inner.kind() == SyntaxKind::Block
        {
            for &statement in inner.statements() {
                if statement.kind() != SyntaxKind::ExpressionStatement {
                    break;
                }
                if let Some(literal) = statement.expression()
                    && literal.kind() == SyntaxKind::StringLiteral
                    && is_use_gpu(literal)
                {
                    nesting.push((pos_u32(literal), end_u32(literal)));
                }
            }
        }
        false
    });
}

pub(crate) fn is_use_gpu(node: P<Node>) -> bool {
    is_named_directive(node, "gpu")
}

/// Record a nested `"use gpu"` without rewriting the file. Check and LSP
/// keep the author's operators.
pub fn report_nesting(file: P<SourceFile>) {
    let text = file.text();
    if !enabled(text) {
        return;
    }
    let Some((_, _, nesting)) = walk_file(file, text) else {
        return;
    };
    if nesting.is_empty() {
        return;
    }
    let _owner = tsrs_core::arena::enter_owner(std::ptr::from_ref::<SourceFile>(&*file) as usize);
    let mut diagnostics = file.diagnostics().to_vec();
    for (start, end) in nesting {
        diagnostics.push(new_external_diagnostic(
            Some(file),
            TextRange::new(start, end),
            MAPPER,
            Category::Error,
            0,
            NESTING,
        ));
    }
    file.set_diagnostics(&diagnostics);
}

fn is_use_tsover(node: P<Node>) -> bool {
    is_named_directive(node, "tsover")
}

pub(crate) fn is_named_directive(node: P<Node>, name: &str) -> bool {
    if node.kind() != SyntaxKind::StringLiteral {
        return false;
    }
    let directive = format!("use {name}");
    let quoted = format!("\"{directive}\"");
    let single = format!("'{directive}'");
    let text = node.text();
    text == directive || text == quoted || text == single
}

fn gpu_body(literal: P<Node>) -> Option<P<Node>> {
    let statement = literal.parent()?;
    if statement.kind() != SyntaxKind::ExpressionStatement {
        return None;
    }
    let body = statement.parent()?;
    if body.kind() != SyntaxKind::Block {
        return None;
    }
    for &stmt in body.statements() {
        if stmt == statement {
            return Some(body);
        }
        if stmt.kind() != SyntaxKind::ExpressionStatement {
            return None;
        }
        let expr = stmt.expression()?;
        if expr.kind() != SyntaxKind::StringLiteral {
            return None;
        }
    }
    None
}

std::thread_local! {
    static REWRITING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn rewriting() -> bool {
    REWRITING.with(std::cell::Cell::get)
}

struct RewriteGuard;

impl RewriteGuard {
    fn enter() -> Self {
        REWRITING.with(|flag| flag.set(true));
        Self
    }
}

impl Drop for RewriteGuard {
    fn drop(&mut self) {
        REWRITING.with(|flag| flag.set(false));
    }
}
