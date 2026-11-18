//! Language features for macro expansions: "Expanded from" related information
//! on diagnostics, the expansion in hover, and the
//! `custom/textDocument/showExpansion` request.

use tsrs_ast::{Diagnostic, DiagnosticExt, new_external_diagnostic};
use tsrs_core::context::Context;
use tsrs_core::{P, TextRange, new_text_range};
use tsrs_diagnostics::Category;
use tsrs_lsproto as lsproto;
use tsrs_spanmap::{Feature, SpanMap};

use crate::LanguageService;
use crate::lsconv::Script;

pub use crate::diagnostics::is_synthesized_content_mapped_diagnostic;

/// One macro expansion, for hover and `custom/textDocument/showExpansion`.
#[derive(Clone, Debug)]
pub struct ExpansionView {
    pub label: String,
    pub text: String,
    pub range: lsproto::Range,
}

/// Whether `loc` lies inside the output of a macro invocation (where the
/// invocation, not the top of the file, is the location).
#[must_use]
pub fn inside(span_map: &SpanMap, loc: TextRange) -> bool {
    !SpanMap::expansions_containing_virtual(Some(span_map), loc).is_empty()
}

/// The original range of the innermost invocation whose output contains `loc`.
#[must_use]
pub fn invocation(span_map: &SpanMap, loc: TextRange) -> Option<TextRange> {
    SpanMap::expansions_containing_virtual(Some(span_map), loc)
        .first()
        .map(|expansion| new_text_range(expansion.original_start, expansion.original_end))
}

/// Attaches "Expanded from" related locations to a compiler diagnostic inside macro output.
///
/// Adds one related location per enclosing invocation, innermost first, pointing at the
/// code the author wrote.
#[must_use]
pub fn with_expansion_context(diag: P<Diagnostic>) -> P<Diagnostic> {
    let Some(file) = diag.file() else {
        return diag;
    };
    let Some(span_map) = file.span_map() else {
        return diag;
    };
    if !diag.source().is_empty() {
        return diag;
    }
    let Some(loc) = diag.loc() else {
        return diag;
    };
    let expansions = SpanMap::expansions_containing_virtual(Some(span_map), loc);
    if expansions.is_empty() {
        return diag;
    }
    let out = diag.clone_diagnostic();
    let source = file.content_mapper();
    let source = if source.is_empty() { "content mapper" } else { source };
    for expansion in expansions.iter().take(8) {
        out.add_related_info(new_external_diagnostic(
            Some(file),
            new_text_range(expansion.original_start, expansion.original_end),
            source,
            Category::Message,
            0,
            &format!("Expanded from {}", expansion.label),
        ));
    }
    out
}

/// The hover, plus "Expanded from" and the expanded code when the position
/// is inside a macro invocation.
pub fn provide_hover(
    ls: &LanguageService,
    ctx: &Context,
    params: &lsproto::HoverParams,
) -> Result<lsproto::HoverResponse, lsproto::Error> {
    let mut response = ls.provide_hover(ctx, params)?;
    let Some(expansion) = expansion_at(ls, &params.text_document.uri, params.position) else {
        return Ok(response);
    };
    let caps = lsproto::get_client_capabilities(ctx);
    let markdown = lsproto::preferred_markup_kind(&caps.text_document.hover.content_format)
        == lsproto::MarkupKind::MARKDOWN;
    let note = if markdown {
        format!("Expanded from {}\n```ts\n{}\n```", expansion.label, expansion.text)
    } else {
        format!("Expanded from {}\n{}", expansion.label, expansion.text)
    };
    if let Some(hover) = response.hover.as_mut() {
        if let Some(markup) = hover.contents.markup_content.as_mut() {
            markup.value.push_str(if markdown { "\n\n---\n\n" } else { "\n\n" });
            markup.value.push_str(&note);
        }
    } else {
        let kind =
            if markdown { lsproto::MarkupKind::MARKDOWN } else { lsproto::MarkupKind::PLAIN_TEXT };
        response.hover = Some(markup_hover(kind, note, expansion.range));
    }
    Ok(response)
}

fn markup_hover(kind: lsproto::MarkupKind, value: String, range: lsproto::Range) -> lsproto::Hover {
    lsproto::Hover {
        contents: lsproto::MarkupContentOrStringOrMarkedStringWithLanguageOrMarkedStrings {
            markup_content: Some(lsproto::MarkupContent { kind, value }),
            ..Default::default()
        },
        range: Some(range),
        ..Default::default()
    }
}

/// Finds the innermost macro expansion whose invocation contains `position`.
///
/// Returns its label, the virtual code it produced, and the invocation's range in the
/// author's file.
#[must_use]
pub fn expansion_at(
    ls: &LanguageService,
    uri: &lsproto::DocumentUri,
    position: lsproto::Position,
) -> Option<ExpansionView> {
    const LIMIT: usize = 4000;
    let (_, file) = ls.get_program_and_file(uri);
    for projection in ls.converters.from_lsp_position_for_source_file(file, position, Feature::All)
    {
        let script = projection.script;
        let span_map = script.span_map()?;
        let at = new_text_range(projection.position, projection.position);
        let Some(expansion) =
            SpanMap::expansions_containing_virtual(Some(span_map), at).into_iter().next()
        else {
            continue;
        };
        let text = script.text();
        let start = usize::try_from(expansion.virtual_start).ok()?;
        let end = usize::try_from(expansion.virtual_end).ok()?.min(text.len());
        let mut code = text.get(start..end)?.trim().to_string();
        if code.len() > LIMIT {
            let mut cut = LIMIT;
            while !code.is_char_boundary(cut) {
                cut -= 1;
            }
            code.truncate(cut);
            code.push_str("\n…");
        }
        // The expansion's virtual text maps back over its invocation.
        let (range, _) = ls.converters.to_lsp_range(
            &script as &dyn Script,
            new_text_range(expansion.virtual_start, expansion.virtual_end),
        );
        return Some(ExpansionView { label: expansion.label.clone(), text: code, range });
    }
    None
}

/// Handles `custom/textDocument/showExpansion` for the innermost macro expansion at `params`.
///
/// Returns a hover whose range is the invocation and whose contents are the macro and its output.
#[expect(
    clippy::unnecessary_wraps,
    reason = "Language service operations share the fallible response signature used by LSP dispatch."
)]
pub fn show_expansion(
    ls: &LanguageService,
    params: &lsproto::TextDocumentPositionParams,
) -> Result<lsproto::CustomTextDocumentShowExpansionResponse, lsproto::Error> {
    let Some(expansion) = expansion_at(ls, &params.text_document.uri, params.position) else {
        return Ok(lsproto::HoverOrNull::default());
    };
    let value = format!("Expanded from {}\n```ts\n{}\n```", expansion.label, expansion.text);
    Ok(lsproto::HoverOrNull {
        hover: Some(markup_hover(lsproto::MarkupKind::MARKDOWN, value, expansion.range)),
    })
}
