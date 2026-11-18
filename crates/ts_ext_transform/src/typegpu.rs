//! GPU function wrapper after content mapping and before check, plus the
//! `unplugin-typegpu` runtime metadata pass at emit/build time.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use tsrs_ast::{
    ExternalModuleIndicatorOptions, Kind as SyntaxKind, ModifierFlags, Node, NodeFlags, NodeList,
    SourceFile, SourceFileParseOptions, TokenFlags, get_source_file_of_node,
    is_function_like_declaration, is_prologue_directive, new_external_diagnostic,
};
use tsrs_core::arena::Region;
use tsrs_core::tspath::Path;
use tsrs_core::{P, ScriptKind, ScriptTarget, TextRange, alloc_str, jsnum};
use tsrs_diagnostics::Category;
use tsrs_parser::parse_source_file_owned;
use tsrs_printer::{EmitContext, PrintHandlers, PrinterOptions, new_emit_context, new_printer};
use tsrs_scanner::{get_leading_comment_ranges, skip_trivia};

use super::tsover::{end_u32, is_use_gpu, pos_u32};

pub const MAPPER_NAME: &str = "typegpu";

pub(crate) fn is_pruned(code: &str) -> bool {
    !code.contains("use gpu") && !code.contains("tgpu") && !code.contains("typegpu")
}

/// Compiler emit hook (`earlyPruneRegex` + `autoNaming: true`).
#[must_use]
pub fn embed_metadata(filename: &str, code: &str) -> String {
    if is_pruned(code) {
        return code.to_owned();
    }
    embed_metadata_unpruned(filename, code, true)
}

/// Build output for a caller that already chose the file.
#[must_use]
pub fn embed_metadata_unpruned(filename: &str, code: &str, auto_name: bool) -> String {
    embed_metadata_reports(filename, code, auto_name).0
}

#[derive(Clone, Debug)]
pub struct EmbedFlags<'a> {
    pub auto_naming: bool,
    pub early_pruning: bool,
    pub force_tgpu_alias: Option<&'a str>,
}

/// A body the transpiler cannot print. Offsets are UTF-8 bytes of the
/// text that was walked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub message: String,
    pub start: u32,
    pub end: u32,
}

#[must_use]
pub fn embed_metadata_reports(
    filename: &str,
    code: &str,
    auto_name: bool,
) -> (String, Vec<String>) {
    let (code, refusals) = embed_metadata_with(
        filename,
        code,
        &EmbedFlags { auto_naming: auto_name, early_pruning: false, force_tgpu_alias: None },
    );
    (code, refusals.into_iter().map(|item| item.message).collect())
}

/// [`embed_metadata_reports`] with the plugin switches.
#[must_use]
pub fn embed_metadata_with(
    filename: &str,
    code: &str,
    flags: &EmbedFlags<'_>,
) -> (String, Vec<Refusal>) {
    if flags.early_pruning && is_pruned(code) {
        return (code.to_owned(), Vec::new());
    }
    let region = Region::new(64 << 10);
    let _guard = region.enter();
    let auto_name = flags.auto_naming;
    let file = parse_in_region(filename, code);
    let mut aliases = BTreeSet::new();
    gather_aliases(file.as_node(), &mut aliases);
    if let Some(alias) = flags.force_tgpu_alias.filter(|alias| !alias.is_empty()) {
        aliases.insert(alias.to_owned());
    }
    let mut edits = Vec::new();
    let unsupported = collect_wraps(file.as_node(), code, &aliases, auto_name, &mut edits);
    if edits.is_empty() {
        return (code.to_owned(), unsupported);
    }
    (apply_wraps(code, edits), unsupported)
}

fn parse_in_region(filename: &str, code: &str) -> P<SourceFile> {
    let file_name = "/input.ts".to_owned();
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
    parse_source_file_owned(opts, code.to_owned(), kind)
}

/// Compiler emit hook. Declaration files are not runtime JS.
#[must_use]
pub fn embed_metadata_js(filename: &str, code: &str) -> String {
    if filename.ends_with(".d.ts") || filename.ends_with(".d.mts") || filename.ends_with(".d.cts") {
        return code.to_owned();
    }
    embed_metadata(filename, code)
}

/// Metadata `getEmbeddedTypegpuMetadata` would collect, in source order.
#[derive(Clone, Debug, PartialEq)]
pub struct EmbeddedMetadata {
    pub version: f64,
    pub name: Option<String>,
}

/// Walk a program the way the plugin's metadata collector does.
#[must_use]
pub fn embedded_metadata(filename: &str, code: &str) -> Vec<EmbeddedMetadata> {
    let region = Region::new(64 << 10);
    let _guard = region.enter();
    let file = parse_in_region(filename, code);
    let mut found = Vec::new();
    collect_embedded(file.as_node(), &mut found);
    found
}

fn collect_embedded(node: P<Node>, found: &mut Vec<EmbeddedMetadata>) {
    if let Some(meta) = embedded_of(node) {
        found.push(meta);
    }
    node.for_each_child(&mut |child| {
        collect_embedded(child, found);
        false
    });
}

fn embedded_of(node: P<Node>) -> Option<EmbeddedMetadata> {
    if !is_function_like_declaration(node) {
        return None;
    }
    let wrapped = skip_transparent_up(node);
    let assignment = wrapped.parent()?;
    if assignment.kind() != SyntaxKind::BinaryExpression {
        return None;
    }
    let assign_bin = assignment.as_binary_expression();
    if assign_bin.operator_token.kind() != SyntaxKind::EqualsToken || assign_bin.right() != wrapped
    {
        return None;
    }
    let assigned = skip_transparent_up(assignment);
    let call = assigned.parent()?;
    if call.kind() != SyntaxKind::CallExpression
        || call.arguments().first().copied() != Some(assigned)
        || !is_metadata_set_call(call)
    {
        return None;
    }
    let meta = *call.arguments().get(1)?;
    if meta.kind() != SyntaxKind::ObjectLiteralExpression {
        return None;
    }
    let version =
        object_prop(meta, "v").filter(|value| value.kind() == SyntaxKind::NumericLiteral)?;
    let name_node = object_prop(meta, "name")?;
    if object_prop(meta, "ast").is_none() || object_prop(meta, "externals").is_none() {
        return None;
    }
    let name = (name_node.kind() == SyntaxKind::StringLiteral).then(|| name_node.text().to_owned());
    Some(EmbeddedMetadata { version: version.text().parse().unwrap_or(0.0), name })
}

fn skip_transparent_up(mut node: P<Node>) -> P<Node> {
    while let Some(parent) = transparent_parent(node) {
        node = parent;
    }
    node
}

fn transparent_parent(node: P<Node>) -> Option<P<Node>> {
    let parent = node.parent()?;
    if !matches!(
        parent.kind(),
        SyntaxKind::ParenthesizedExpression
            | SyntaxKind::AsExpression
            | SyntaxKind::SatisfiesExpression
            | SyntaxKind::TypeAssertionExpression
            | SyntaxKind::NonNullExpression
    ) {
        return None;
    }
    (parent.expression() == Some(node)).then_some(parent)
}

fn is_metadata_set_call(node: P<Node>) -> bool {
    let Some(callee_raw) = node.expression() else {
        return false;
    };
    let callee = unwrap_parens(callee_raw);
    if !matches!(
        callee.kind(),
        SyntaxKind::PropertyAccessExpression | SyntaxKind::ElementAccessExpression
    ) {
        return false;
    }
    if member_property_name(callee).as_deref() != Some("set") {
        return false;
    }
    callee.expression().is_some_and(contains_metadata_access)
}

fn contains_metadata_access(node: P<Node>) -> bool {
    if is_metadata_access(node) {
        return true;
    }
    let mut found = false;
    node.for_each_child(&mut |child| {
        if contains_metadata_access(child) {
            found = true;
        }
        false
    });
    found
}

fn is_metadata_access(node: P<Node>) -> bool {
    matches!(
        node.kind(),
        SyntaxKind::PropertyAccessExpression | SyntaxKind::ElementAccessExpression
    ) && member_property_name(node).as_deref() == Some("__TYPEGPU_META__")
}

fn member_property_name(node: P<Node>) -> Option<String> {
    match node.kind() {
        SyntaxKind::PropertyAccessExpression => {
            let name = node.as_property_access_expression().name;
            (name.kind() == SyntaxKind::Identifier).then(|| name.text().to_owned())
        }
        SyntaxKind::ElementAccessExpression => {
            let arg = node.as_element_access_expression().argument_expression;
            (arg.kind() == SyntaxKind::StringLiteral).then(|| arg.text().to_owned())
        }
        _ => None,
    }
}

fn object_prop(object: P<Node>, expected: &str) -> Option<P<Node>> {
    for &prop in object.properties() {
        if prop.kind() != SyntaxKind::PropertyAssignment {
            continue;
        }
        let key = prop.name()?;
        let name =
            if key.kind() == SyntaxKind::Identifier || key.kind() == SyntaxKind::StringLiteral {
                key.text()
            } else {
                continue;
            };
        if name == expected {
            return prop.initializer();
        }
    }
    None
}

struct WrapEdit {
    remove_start: usize,
    remove_end: usize,
    insert_at: usize,
    text: String,
    export_name: Option<String>,
    export_default: bool,
}

fn collect_wraps(
    root: P<Node>,
    code: &str,
    aliases: &BTreeSet<String>,
    auto_name: bool,
    edits: &mut Vec<WrapEdit>,
) -> Vec<Refusal> {
    let mut work = Build::new(code, aliases.clone(), auto_name);
    walk_wraps(root, &mut work, edits);
    work.unsupported
}

/// Refusals in author text. Check and LSP call this. It does not rewrite.
#[must_use]
pub fn refusals(filename: &str, code: &str) -> Vec<Refusal> {
    if !super::tsover::enabled(code) {
        return Vec::new();
    }
    let region = Region::new(64 << 10);
    let _guard = region.enter();
    let file = parse_in_region(filename, code);
    collect_refusals(file.as_node(), code)
}

/// Attach a refusal to the author file so check and LSP can show it.
pub fn report_refusals(file: P<SourceFile>) {
    let text = file.text();
    if !super::tsover::enabled(text) {
        return;
    }
    let found = collect_refusals(file.as_node(), text);
    if found.is_empty() {
        return;
    }
    let _owner = tsrs_core::arena::enter_owner(std::ptr::from_ref::<SourceFile>(&*file) as usize);
    let mut diagnostics = file.diagnostics().to_vec();
    for item in found {
        diagnostics.push(new_external_diagnostic(
            Some(file),
            TextRange::new(item.start, item.end),
            MAPPER_NAME,
            Category::Error,
            0,
            &item.message,
        ));
    }
    file.set_diagnostics(&diagnostics);
}

fn collect_refusals(root: P<Node>, code: &str) -> Vec<Refusal> {
    let mut found = Vec::new();
    walk_refusals(root, code, &mut found);
    found
}

fn walk_refusals(node: P<Node>, code: &str, found: &mut Vec<Refusal>) {
    if is_function_like_declaration(node) && function_marked(node) && !already_wrapped(node) {
        let ast = transpile_marked(node, code);
        if let Some(message) = ast.unsupported {
            let (start, end) = ast.span.unwrap_or((pos_u32(node), end_u32(node)));
            found.push(refusal(message, start, end));
        }
        return;
    }
    node.for_each_child(&mut |child| {
        walk_refusals(child, code, found);
        false
    });
}

fn refusal(message: String, start: u32, end: u32) -> Refusal {
    Refusal { message, start, end: end.max(start) }
}

fn walk_wraps(node: P<Node>, work: &mut Build, edits: &mut Vec<WrapEdit>) {
    if is_function_like_declaration(node) && function_marked(node) && !already_wrapped(node) {
        if let Some(edit) = edit_for_function(node, work, true) {
            edits.push(edit);
        }
        return;
    }
    if is_shell_implementation(node, work)
        && let Some(&first_arg) = node.arguments().first()
    {
        let arg = unwrap_parens(first_arg);
        if is_function_like_declaration(arg)
            && !function_marked(arg)
            && !already_wrapped(arg)
            && !function_has_meta(arg, &work.code)
            && let Some(edit) = edit_for_function(arg, work, false)
        {
            edits.push(edit);
        }
    }
    if node.kind() == SyntaxKind::CallExpression {
        for &arg_raw in node.arguments() {
            let arg = unwrap_parens(arg_raw);
            if is_function_like_declaration(arg)
                && function_marked(arg)
                && !already_wrapped(arg)
                && let Some(edit) = edit_for_function(arg, work, true)
            {
                edits.push(edit);
            }
        }
    }
    if is_function_like_declaration(node)
        && edits.iter().any(|edit| {
            byte_at(&work.code, pos_u32(node)).is_some_and(|at| edit.remove_start == at)
        })
    {
        return;
    }
    if work.auto_name
        && let Some((label, expression)) = labelled(node)
        && contains_resource(expression, work)
        && !already_autonamed(expression)
        && let Some(edit) = edit_for_autoname(expression, &label, work)
    {
        edits.push(edit);
        walk_wraps(expression, work, edits);
        return;
    }
    node.for_each_child(&mut |child| {
        walk_wraps(child, work, edits);
        false
    });
}

fn edit_for_function(node: P<Node>, work: &mut Build, marked: bool) -> Option<WrapEdit> {
    let mut start = byte_at(&work.code, skip_trivia(&work.code, pos_u32(node)))?;
    let end = byte_at(&work.code, end_u32(node))?;
    let parent = node.parent();
    if parent.is_some_and(|p| p.kind() == SyntaxKind::ExportAssignment) {
        while start > 0 && work.code.as_bytes()[start - 1].is_ascii_whitespace() {
            start -= 1;
        }
    }
    let wrapper = printed_wrapper(node, work, marked);
    let anonymous_default = node.kind() == SyntaxKind::FunctionDeclaration
        && parent.is_some_and(|p| p.kind() == SyntaxKind::SourceFile)
        && node.modifier_flags().intersects(ModifierFlags::Default)
        && !declaration_names_itself(&work.code[start..end]);
    if anonymous_default {
        let slice = &work.code[start..end];
        if let Some(at) = slice.find("function") {
            start += at;
        }
    }
    let own_name = if anonymous_default { None } else { node.name() };
    if node.kind() == SyntaxKind::FunctionDeclaration
        && let Some(own) = own_name
        && own.kind() == SyntaxKind::Identifier
        && let Some(parent_node) = parent
    {
        return hoist_edit(node, parent_node, work, start, end, &wrapper);
    }
    Some(WrapEdit {
        remove_start: start,
        remove_end: end,
        insert_at: start,
        text: {
            let is_export_assign = parent.is_some_and(|p| p.kind() == SyntaxKind::ExportAssignment);
            let column = if let Some(parent_node) = parent
                && is_export_assign
            {
                byte_at(&work.code, skip_trivia(&work.code, pos_u32(parent_node))).unwrap_or(start)
            } else {
                work.code[..start].rfind('\n').map_or(0, |at| at + 1)
            };
            let body = indent_continuations(&wrapper, column_at(&work.code, column));
            let close = if anonymous_default { ";" } else { "" };
            if is_export_assign { format!(" {body}{close}") } else { format!("{body}{close}") }
        },
        export_name: None,
        export_default: false,
    })
}

fn hoist_edit(
    node: P<Node>,
    parent: P<Node>,
    work: &Build,
    start: usize,
    end: usize,
    wrapper: &str,
) -> Option<WrapEdit> {
    let name = node.name()?.text();
    let comments = leading_comments(node, &work.code);
    let comment_start = comments.first().map_or(start, |span| span.0).min(start);
    let comment_text = if comment_start < start {
        let raw = &work.code[comment_start..start];
        let line = work.code[..comment_start].rfind('\n').map_or(0, |at| at + 1);
        let indent = &work.code[line..comment_start];
        let text = if indent.chars().all(|ch| ch == ' ') {
            format!("{indent}{raw}")
        } else {
            raw.to_owned()
        };
        text.trim_start_matches('\n').to_owned()
    } else {
        String::new()
    };
    let named_export = is_named_export(parent);
    let export_modifier = !named_export
        && parent.kind() == SyntaxKind::SourceFile
        && node.modifier_flags().intersects(ModifierFlags::Export)
        && !node.modifier_flags().intersects(ModifierFlags::Default);
    let export_assignment = parent.kind() == SyntaxKind::ExportAssignment
        && !parent.as_export_assignment().is_export_equals;
    let default_modifier = parent.kind() == SyntaxKind::SourceFile
        && node.modifier_flags().intersects(ModifierFlags::Default);
    let default_export = export_assignment || default_modifier;
    let remove_start = if named_export || export_assignment {
        byte_at(&work.code, skip_trivia(&work.code, pos_u32(parent)))?.min(comment_start)
    } else {
        comment_start
    };
    let insert_at = if export_assignment {
        remove_start
    } else {
        scope_top(node, &work.code).unwrap_or(remove_start)
    };
    let column = if insert_at == 0 {
        0
    } else {
        let brace_line = work.code[..insert_at].rfind('\n').map_or(0, |at| at + 1);
        column_at(&work.code, brace_line) + 2
    };
    let wrapper = indent_continuations(wrapper, column);
    let decl = format!("{comment_text}const {name} = {wrapper};");
    let text = if default_export && !default_modifier {
        let sep = if decl.ends_with('\n') { "" } else { "\n" };
        format!("{decl}{sep}export default {name};")
    } else {
        decl
    };
    let text = if insert_at == remove_start {
        text
    } else {
        let next = work.code[end..].find(|ch: char| !ch.is_whitespace()).map_or(end, |at| end + at);
        let line = work.code[..next].rfind('\n').map_or(0, |at| at + 1);
        let indent = &work.code[line..next];
        if indent.chars().all(|ch| ch == ' ') && !indent.is_empty() {
            format!("{text}\n{indent}")
        } else if !text.ends_with('\n') {
            format!("{text}\n")
        } else {
            text
        }
    };
    Some(WrapEdit {
        remove_start,
        remove_end: end,
        insert_at,
        text,
        export_name: (named_export || export_modifier || default_modifier).then(|| name.to_owned()),
        export_default: default_modifier,
    })
}

fn scope_top(node: P<Node>, code: &str) -> Option<usize> {
    let mut scope = node.parent()?;
    if is_named_export(scope) || scope.kind() == SyntaxKind::ExportAssignment {
        scope = scope.parent()?;
    }
    let after_prologue = |scope: P<Node>| -> Option<usize> {
        let mut end = None;
        for &statement in scope.statements() {
            if !is_prologue_directive(statement) {
                break;
            }
            end = Some(end_u32(statement) as usize);
        }
        let end = end?.min(code.len());
        let line = code[end..].find('\n').map_or(code.len(), |at| end + at + 1);
        Some(line)
    };
    if scope.kind() == SyntaxKind::SourceFile {
        return Some(after_prologue(scope).unwrap_or(0));
    }
    if !matches!(scope.kind(), SyntaxKind::Block | SyntaxKind::ModuleBlock) {
        return None;
    }
    if let Some(at) = after_prologue(scope) {
        let at = at + code[at..].len() - code[at..].trim_start_matches([' ', '\t']).len();
        return Some(at);
    }
    let open = pos_u32(scope) as usize;
    let brace = code.get(open..)?.find('{')? + open + 1;
    let body_end = (end_u32(scope) as usize).min(code.len());
    let mut at = brace;
    while at < body_end && code.as_bytes().get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    Some(at)
}

fn printed_wrapper(node: P<Node>, work: &mut Build, marked: bool) -> String {
    let wrapped = wrap_function(node, work, marked);
    let printed = print_expression(work.emit, wrapped);
    let printed = printed.trim().trim_end_matches(';').trim_end().to_owned();
    let author = node.body().map_or_else(|| "{}".to_owned(), |body| slice_node(&work.code, body));
    let printed =
        printed.replacen("{ __tgpu_body__; }", &author, 1).replacen("__tgpu_body__", &author, 1);
    printed
        .replace("/*#__PURE__*/\n($ =>", "/*#__PURE__*/($ =>")
        .replace("/*#__PURE__*/\n($=>", "/*#__PURE__*/($ =>")
        .replace("() =>\n", "() => ")
        .replace("() =>\r\n", "() => ")
        .replace("() => { }", "() => {}")
        .replace("( /*#__PURE__*/", "(/*#__PURE__*/")
        .replace(")( /*#__PURE__*/", ")(/*#__PURE__*/")
        .replace("() => ({ ", "() => ({\n")
}

fn print_expression(emit: P<EmitContext>, node: P<Node>) -> String {
    let factory = &emit.factory;
    let statement = factory.new_expression_statement(node);
    let eof = factory.new_token(SyntaxKind::EndOfFile);
    let text = get_source_file_of_node(node).map_or("", |sf| sf.text());
    let file_name = "/input.ts".to_owned();
    let opts = SourceFileParseOptions {
        path: Path::from(file_name.clone()),
        file_name,
        external_module_indicator_options: ExternalModuleIndicatorOptions::default(),
    };
    let root = factory.new_source_file(opts, text, factory.new_node_list(vec![statement]), eof);
    let mut printer = new_printer(
        PrinterOptions { target: ScriptTarget::ES_NEXT, ..PrinterOptions::default() },
        PrintHandlers::default(),
        Some(emit),
    );
    babel_indent(&printer.emit(root, Some(root.as_source_file_p())))
}

fn edit_for_autoname(expression: P<Node>, name: &str, work: &Build) -> Option<WrapEdit> {
    let start = byte_at(&work.code, skip_trivia(&work.code, pos_u32(expression)))?;
    let end = byte_at(&work.code, end_u32(expression))?;
    let inner = work.code[start..end].trim();
    let text =
        format!("/*#__PURE__*/(globalThis.__TYPEGPU_AUTONAME__ ?? (a => a))({inner}, \"{name}\")");
    Some(WrapEdit {
        remove_start: start,
        remove_end: end,
        insert_at: start,
        text,
        export_name: None,
        export_default: false,
    })
}

fn column_at(code: &str, at: usize) -> usize {
    let at = at.min(code.len());
    let line = code[..at].rfind('\n').map_or(0, |index| index + 1);
    at - line
}

fn indent_continuations(text: &str, column: usize) -> String {
    if column == 0 {
        return text.to_owned();
    }
    let pad = " ".repeat(column);
    let mut lines = text.lines();
    let Some(first) = lines.next() else {
        return String::new();
    };
    let mut out = first.to_owned();
    for line in lines {
        out.push('\n');
        if !line.is_empty() {
            out.push_str(&pad);
        }
        out.push_str(line);
    }
    out
}

fn byte_at(code: &str, pos: u32) -> Option<usize> {
    let at = pos as usize;
    (at <= code.len()).then_some(at)
}

fn compose_nested_edits(code: &str, edits: &mut Vec<WrapEdit>) {
    let mut drop_flags = vec![false; edits.len()];
    for index in 0..edits.len() {
        if drop_flags[index] {
            continue;
        }
        for other in index + 1..edits.len() {
            if edits[index].remove_start == edits[other].remove_start
                && edits[index].remove_end == edits[other].remove_end
            {
                drop_flags[other] = true;
            }
        }
    }
    for outer in 0..edits.len() {
        if !edits[outer].text.contains("__TYPEGPU_AUTONAME__") {
            continue;
        }
        let mut nested = Vec::new();
        for (inner, item) in edits.iter().enumerate() {
            if outer == inner || drop_flags[inner] {
                continue;
            }
            let (start, end) = (edits[outer].remove_start, edits[outer].remove_end);
            let (inner_start, inner_end) = (item.remove_start, item.remove_end);
            if inner_start < start || inner_end > end || (inner_start == start && inner_end == end)
            {
                continue;
            }
            nested.push((inner_start, inner_end, inner));
            drop_flags[inner] = true;
        }
        if nested.is_empty() {
            continue;
        }
        nested.sort_by_key(|item| item.0);
        let start = edits[outer].remove_start;
        let end = edits[outer].remove_end.min(code.len());
        let mut inner_text = String::new();
        let mut cursor = start;
        for (inner_start, inner_end, index) in nested {
            if inner_start > cursor && inner_start <= end {
                inner_text.push_str(&code[cursor..inner_start]);
            }
            inner_text.push_str(&edits[index].text);
            cursor = inner_end;
        }
        if cursor < end {
            inner_text.push_str(&code[cursor..end]);
        }
        let mut collapsed = String::new();
        let mut pending_break = false;
        for ch in inner_text.chars() {
            if ch == '\n' || (pending_break && ch == ' ') {
                pending_break = true;
                continue;
            }
            if pending_break {
                if ch == '.' {
                    pending_break = false;
                } else {
                    collapsed.push('\n');
                    pending_break = false;
                }
            }
            collapsed.push(ch);
        }
        let name = edits[outer].text.rsplit('"').nth(1).unwrap_or("").to_owned();
        edits[outer].text = format!(
            "/*#__PURE__*/(globalThis.__TYPEGPU_AUTONAME__ ?? (a => a))({collapsed}, \"{name}\")"
        );
    }
    let mut index = 0;
    edits.retain(|_| {
        let keep = !drop_flags[index];
        index += 1;
        keep
    });
}

fn apply_wraps(code: &str, mut edits: Vec<WrapEdit>) -> String {
    compose_nested_edits(code, &mut edits);
    let exports = edits
        .iter()
        .filter_map(|edit| edit.export_name.clone().map(|name| (name, edit.export_default)))
        .collect::<Vec<_>>();
    let mut covered = vec![false; code.len()];
    let mut inserts: Vec<(usize, String)> = Vec::new();
    for edit in &edits {
        if edit.remove_end > code.len() || edit.remove_start > edit.remove_end {
            continue;
        }
        for byte in covered.iter_mut().take(edit.remove_end).skip(edit.remove_start) {
            *byte = true;
        }
        inserts.push((edit.insert_at, edit.text.clone()));
    }
    inserts.sort_by(|left, right| right.0.cmp(&left.0).then(right.1.len().cmp(&left.1.len())));
    let mut at_insert: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (at, text) in inserts {
        at_insert.entry(at).or_default().push(text);
    }
    let mut out = String::new();
    let mut index = 0;
    while index < code.len() {
        if let Some(texts) = at_insert.remove(&index) {
            for text in texts.into_iter().rev() {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(&text);
            }
        }
        if covered[index] {
            index += 1;
        } else {
            let ch = code[index..].chars().next().unwrap_or('\0');
            out.push(ch);
            index += ch.len_utf8();
        }
    }
    if let Some(texts) = at_insert.remove(&code.len()) {
        for text in texts.into_iter().rev() {
            out.push_str(&text);
        }
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    let lines = out.lines().map(str::trim_end).collect::<Vec<_>>();
    let mut kept = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let prev = kept.last().map(|prev: &String| prev.trim().to_owned());
        let blank = line.trim().is_empty();
        let before_close = blank
            && lines[index + 1..]
                .iter()
                .find(|next| !next.trim().is_empty())
                .is_some_and(|next| next.trim() == "}");
        let after_open = blank && prev.as_deref().is_some_and(|prev| prev.ends_with('{'));
        if after_open {
            continue;
        }
        if !before_close {
            kept.push((*line).to_owned());
        }
    }
    out = kept.join("\n");
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    for (name, default) in exports {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        if default {
            let _ = writeln!(out, "export default {name};");
        } else {
            let _ = writeln!(out, "export {{ {name} }};");
        }
    }
    join_export_default(&out)
}

fn join_export_default(text: &str) -> String {
    let mut flat = String::new();
    let mut rest = text;
    let marker = ")(/*#__PURE__*/";
    while let Some(at) = rest.find(marker) {
        let head = &rest[..at];
        let line = head.rfind('\n').unwrap_or(0);
        if head[line..].trim() == ")" {
            let above = head[..line].trim_end();
            let open = above.rfind("({").map(|pos| pos + 1);
            if let Some(open) = open {
                let object =
                    above[open..].chars().filter(|ch| !ch.is_whitespace()).collect::<String>();
                flat.push_str(&above[..open]);
                flat.push_str(&object);
                flat.push_str(marker);
                rest = &rest[at + marker.len()..];
                continue;
            }
        }
        flat.push_str(&rest[..at + marker.len()]);
        rest = &rest[at + marker.len()..];
    }
    flat.push_str(rest);
    let text = flat;
    let mut lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
    let mut index = 0;
    while index + 1 < lines.len() {
        let trimmed = lines[index].trim_end();
        let next = lines[index + 1].trim_start();
        let export_default = trimmed.trim() == "export default";
        let join = export_default
            || (trimmed.ends_with('=') && !trimmed.trim_end().ends_with("const"))
            || trimmed.ends_with('(');
        if join {
            let indent = lines[index].chars().take_while(|c| *c == ' ').count();
            let head = if export_default { "export default" } else { trimmed.trim() };
            lines[index] = format!("{}{head} {next}", " ".repeat(indent));
            lines.remove(index + 1);
            continue;
        }
        index += 1;
    }
    let mut joined = lines.join("\n");
    if text.ends_with('\n') && !joined.ends_with('\n') {
        joined.push('\n');
    }
    joined.replace("( /*#__PURE__*/", "(/*#__PURE__*/")
}

struct Build {
    unsupported: Vec<Refusal>,
    emit: P<EmitContext>,
    code: String,
    aliases: BTreeSet<String>,
    auto_name: bool,
}

impl Build {
    fn new(code: &str, aliases: BTreeSet<String>, auto_name: bool) -> Self {
        Self {
            unsupported: Vec::new(),
            emit: new_emit_context(),
            code: code.to_owned(),
            aliases,
            auto_name,
        }
    }

    fn id(&self, text: &str) -> P<Node> {
        self.emit.factory.new_identifier(alloc_str(text))
    }

    fn str_lit(&self, text: &str, flags: TokenFlags) -> P<Node> {
        self.emit.factory.new_string_literal(alloc_str(text), flags)
    }

    fn num_lit(&self, text: &str) -> P<Node> {
        self.emit.factory.new_numeric_literal(alloc_str(text), TokenFlags::None)
    }

    fn list(&self, nodes: &[P<Node>]) -> P<NodeList> {
        self.emit.factory.new_node_list(nodes.to_vec())
    }

    fn empty(&self) -> P<NodeList> {
        self.list(&[])
    }
}

/// Babel's generator indents by 2. The compiler printer indents by 4.
fn babel_indent(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        let spaces = line.chars().take_while(|c| *c == ' ').count();
        let trimmed = spaces / 4 * 2 + spaces % 4;
        lines.push(format!("{}{}", " ".repeat(trimmed), line[spaces..].trim_end()));
    }
    compact_metadata(&mut lines);
    let mut spaced = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        spaced.push(line.clone());
        let trimmed = line.trim();
        let directive = trimmed == "'use gpu';"
            || trimmed == "\"use gpu\";"
            || trimmed == "'worklet';"
            || trimmed == "\"worklet\";";
        if directive && lines.get(index + 1).is_some_and(|next| !next.trim().is_empty()) {
            spaced.push(String::new());
        }
    }
    let mut out = spaced.join("\n");
    out =
        out.replace("/*#__PURE__*/ ", "/*#__PURE__*/").replace("( /*#__PURE__*/", "(/*#__PURE__*/");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

fn compact_metadata(lines: &mut Vec<String>) {
    let mut index = 0;
    while index + 3 < lines.len() {
        let open = lines[index].trim();
        let key1 = lines[index + 1].trim();
        let key2 = lines[index + 2].trim();
        let close = lines[index + 3].trim();
        if open == "{"
            && key1.starts_with("type:")
            && key2.starts_with("name:")
            && (close == "}," || close == "}")
        {
            let indent = lines[index].chars().take_while(|c| *c == ' ').count();
            let comma = if close.ends_with(',') { "," } else { "" };
            lines[index] = format!("{}{{ {key1} {key2} }}{comma}", " ".repeat(indent));
            lines.drain(index + 1..index + 4);
            continue;
        }
        index += 1;
    }
    index = 0;
    while index < lines.len() {
        if lines[index].trim() != "params: [" {
            index += 1;
            continue;
        }
        let mut objects = Vec::new();
        let mut cursor = index + 1;
        while cursor < lines.len() && lines[cursor].trim().starts_with('{') {
            objects.push(lines[cursor].trim().trim_end_matches(',').to_owned());
            cursor += 1;
        }
        let Some(close) = lines.get(cursor) else {
            break;
        };
        if (close.trim() != "]" && close.trim() != "],") || objects.is_empty() {
            index += 1;
            continue;
        }
        let indent = " ".repeat(lines[index].chars().take_while(|c| *c == ' ').count());
        let mut replacement = vec![format!("{indent}params: [{{")];
        for (at, object) in objects.iter().enumerate() {
            let body = object.trim_start_matches('{').trim().trim_end_matches('}').trim();
            let mut keys = body.split(", ").filter(|part| !part.is_empty());
            let first = keys.next().unwrap_or("");
            let second = keys.next().unwrap_or("");
            let closer = if at + 1 == objects.len() {
                format!("{indent}}}],")
            } else {
                format!("{indent}}}, {{")
            };
            replacement.push(format!("{indent}  {first},"));
            replacement.push(format!("{indent}  {second}"));
            replacement.push(closer);
        }
        lines.splice(index..=cursor, replacement);
        index += 1;
    }
}

fn is_named_export(node: P<Node>) -> bool {
    if node.kind() != SyntaxKind::ExportDeclaration {
        return false;
    }
    if node.module_specifier().is_some() {
        return false;
    }
    let ed = node.as_export_declaration();
    ed.export_clause.is_none_or(|clause| clause.kind() != SyntaxKind::NamedExports)
}

fn already_wrapped(node: P<Node>) -> bool {
    if function_has_meta(node, "") {
        return true;
    }
    let mut cursor = node;
    for _ in 0..6 {
        let Some(parent) = cursor.parent() else {
            break;
        };
        cursor = parent;
        if cursor.kind() == SyntaxKind::CallExpression
            && slice_node_raw(cursor).contains("__TYPEGPU_META__")
        {
            return true;
        }
    }
    false
}

fn function_has_meta(node: P<Node>, code: &str) -> bool {
    if slice_node_raw(node).contains("__TYPEGPU_META__") {
        return true;
    }
    let Some(start) = byte_at(code, pos_u32(node)) else {
        return false;
    };
    let Some(end) = byte_at(code, end_u32(node)) else {
        return false;
    };
    code.get(start..end).is_some_and(|slice| slice.contains("__TYPEGPU_META__"))
}

fn slice_node_raw(node: P<Node>) -> String {
    let Some(file) = get_source_file_of_node(node) else {
        return String::new();
    };
    slice_node(file.text(), node)
}

fn wrap_function(node: P<Node>, work: &mut Build, marked: bool) -> P<Node> {
    let name = function_name(node, work);
    let ast = transpile_marked(node, &work.code);
    if let Some(message) = ast.unsupported.clone() {
        let (start, end) = ast.span.unwrap_or((pos_u32(node), end_u32(node)));
        work.unsupported.push(refusal(message, start, end));
    }
    let _ = marked;
    let inner = function_expression(node, work);
    let meta = metadata_node(work, &name, &ast);
    pure_wrapper(work, inner, meta)
}

fn function_expression(node: P<Node>, work: &Build) -> P<Node> {
    let placeholder = work.emit.factory.new_expression_statement(work.id("__tgpu_body__"));
    let body = work.emit.factory.new_block(work.list(&[placeholder]), false);
    let params = rebuild_params(node, work);
    if node.kind() == SyntaxKind::ArrowFunction {
        work.emit.factory.new_arrow_function(
            None,
            None,
            params,
            None,
            None,
            Some(work.emit.factory.new_token(SyntaxKind::EqualsGreaterThanToken)),
            Some(body),
        )
    } else {
        let name = if !function_names_itself(node, work) {
            None
        } else if let Some(own) = node.name()
            && own.kind() == SyntaxKind::Identifier
        {
            Some(work.id(own.text()))
        } else {
            None
        };
        work.emit.factory.new_function_expression(
            None,
            None,
            name,
            None,
            params,
            None,
            None,
            Some(body),
        )
    }
}

fn rebuild_params(node: P<Node>, work: &Build) -> Option<P<NodeList>> {
    let parameter_list = node.parameter_list()?;
    let params = parameter_list
        .nodes()
        .iter()
        .map(|&param| {
            let id = if let Some(name) = param.name()
                && name.kind() == SyntaxKind::Identifier
            {
                work.id(name.text())
            } else {
                work.id("_")
            };
            let type_node =
                param.type_node().map(|ty| type_from_slice(work, &slice_node(&work.code, ty)));
            work.emit.factory.new_parameter_declaration(None, None, id, None, type_node, None)
        })
        .collect::<Vec<_>>();
    Some(work.list(&params))
}

fn type_from_slice(work: &Build, text: &str) -> P<Node> {
    let parts = text.split('|').map(str::trim).filter(|part| !part.is_empty()).collect::<Vec<_>>();
    if parts.len() > 1 {
        let mut node = type_atom(work, parts[0]);
        for part in &parts[1..] {
            node = work.emit.factory.new_union_type_node(work.list(&[node, type_atom(work, part)]));
        }
        return node;
    }
    type_atom(work, text.trim())
}

fn type_atom(work: &Build, text: &str) -> P<Node> {
    let kind = match text {
        "number" => SyntaxKind::NumberKeyword,
        "string" => SyntaxKind::StringKeyword,
        "boolean" => SyntaxKind::BooleanKeyword,
        "undefined" => SyntaxKind::UndefinedKeyword,
        "void" => SyntaxKind::VoidKeyword,
        "any" => SyntaxKind::AnyKeyword,
        "unknown" => SyntaxKind::UnknownKeyword,
        "never" => SyntaxKind::NeverKeyword,
        "bigint" => SyntaxKind::BigIntKeyword,
        "object" => SyntaxKind::ObjectKeyword,
        "symbol" => SyntaxKind::SymbolKeyword,
        _ => {
            return work.emit.factory.new_type_reference_node(work.id(text), None);
        }
    };
    work.emit.factory.new_keyword_type_node(kind)
}

fn pure_wrapper(work: &Build, function: P<Node>, metadata: P<Node>) -> P<Node> {
    let dollar = work.id("$");
    let global = work.id("globalThis");
    let meta_name = work.id("__TYPEGPU_META__");
    let map = work.emit.factory.new_new_expression(work.id("WeakMap"), None, Some(work.empty()));
    let access =
        work.emit.factory.new_property_access_expression(global, None, meta_name, NodeFlags::None);
    let assign = work.emit.factory.new_binary_expression(
        None,
        access,
        None,
        work.emit.factory.new_token(SyntaxKind::QuestionQuestionEqualsToken),
        map,
    );
    let set = work.emit.factory.new_property_access_expression(
        assign,
        None,
        work.id("set"),
        NodeFlags::None,
    );
    let slot = work.emit.factory.new_property_access_expression(
        dollar,
        None,
        work.id("f"),
        NodeFlags::None,
    );
    let stored = work.emit.factory.new_binary_expression(
        None,
        slot,
        None,
        work.emit.factory.new_token(SyntaxKind::EqualsToken),
        function,
    );
    let set_call = work.emit.factory.new_call_expression(
        set,
        None,
        None,
        work.list(&[stored, metadata]),
        NodeFlags::None,
    );
    let f = work.emit.factory.new_property_access_expression(
        dollar,
        None,
        work.id("f"),
        NodeFlags::None,
    );
    let and = work.emit.factory.new_binary_expression(
        None,
        set_call,
        None,
        work.emit.factory.new_token(SyntaxKind::AmpersandAmpersandToken),
        f,
    );
    let arrow = work.emit.factory.new_arrow_function(
        None,
        None,
        Some(work.list(&[
            work.emit.factory.new_parameter_declaration(None, None, dollar, None, None, None),
        ])),
        None,
        None,
        Some(work.emit.factory.new_token(SyntaxKind::EqualsGreaterThanToken)),
        Some(and),
    );
    let call = work.emit.factory.new_call_expression(
        arrow,
        None,
        None,
        work.list(&[work.emit.factory.new_object_literal_expression(work.empty(), false)]),
        NodeFlags::None,
    );
    mark_pure(work, call)
}

fn mark_pure(work: &Build, node: P<Node>) -> P<Node> {
    work.emit.add_synthetic_leading_comment(
        node,
        SyntaxKind::MultiLineCommentTrivia,
        "#__PURE__",
        false,
    );
    node
}

fn metadata_node(work: &Build, name: &str, ast: &Tinyest) -> P<Node> {
    let name_node =
        if name.is_empty() { work.id("undefined") } else { work.str_lit(name, TokenFlags::None) };
    let props = [
        prop(work, "v", work.num_lit("2")),
        prop(work, "name", name_node),
        prop(work, "ast", ast_node(work, ast)),
        prop(work, "externals", externals_node(work, ast)),
    ];
    work.emit.factory.new_object_literal_expression(work.list(&props), true)
}

fn prop(work: &Build, name: &str, value: P<Node>) -> P<Node> {
    work.emit.factory.new_property_assignment(None, work.id(name), None, None, value)
}

fn ast_node(work: &Build, ast: &Tinyest) -> P<Node> {
    let params = ast
        .params
        .iter()
        .map(|name| {
            work.emit.factory.new_object_literal_expression(
                work.list(&[
                    prop(work, "type", work.str_lit("i", TokenFlags::None)),
                    prop(work, "name", work.str_lit(name, TokenFlags::None)),
                ]),
                true,
            )
        })
        .collect::<Vec<_>>();
    let body = json_to_node(work, &ast.body);
    work.emit.factory.new_object_literal_expression(
        work.list(&[
            prop(
                work,
                "params",
                work.emit
                    .factory
                    .new_array_literal_expression(work.list(&params), !params.is_empty()),
            ),
            prop(work, "body", body),
        ]),
        true,
    )
}

fn externals_node(work: &Build, ast: &Tinyest) -> P<Node> {
    let props = ast
        .externals
        .iter()
        .map(|name| {
            let chain = external_access(work, name);
            let arrow = work.emit.factory.new_arrow_function(
                None,
                None,
                Some(work.empty()),
                None,
                None,
                Some(work.emit.factory.new_token(SyntaxKind::EqualsGreaterThanToken)),
                Some(chain),
            );
            work.emit.factory.new_property_assignment(
                None,
                work.str_lit(name, TokenFlags::None),
                None,
                None,
                arrow,
            )
        })
        .collect::<Vec<_>>();
    work.emit.factory.new_object_literal_expression(work.list(&props), !props.is_empty())
}

fn external_access(work: &Build, name: &str) -> P<Node> {
    let mut parts = name.split('.');
    let first = parts.next().unwrap_or(name);
    let mut node = if first == "this" {
        work.emit.factory.new_keyword_expression(SyntaxKind::ThisKeyword)
    } else {
        work.id(first)
    };
    for part in parts {
        let prop_name = if let Some(private) = part.strip_prefix('#') {
            work.emit.factory.new_private_identifier(alloc_str(&format!("#{private}")))
        } else {
            work.id(part)
        };
        node = work.emit.factory.new_property_access_expression(
            node,
            None,
            prop_name,
            NodeFlags::None,
        );
    }
    node
}

fn json_to_node(work: &Build, text: &str) -> P<Node> {
    let mut parser = JsonNodes { work, rest: text.trim() };
    parser.value()
}

struct JsonNodes<'a> {
    work: &'a Build,
    rest: &'a str,
}

impl JsonNodes<'_> {
    fn value(&mut self) -> P<Node> {
        self.rest = self.rest.trim_start();
        if let Some(rest) = self.rest.strip_prefix('[') {
            self.rest = rest;
            let mut items = Vec::new();
            loop {
                self.rest = self.rest.trim_start();
                if let Some(rest) = self.rest.strip_prefix(']') {
                    self.rest = rest;
                    break;
                }
                items.push(self.value());
                self.rest = self.rest.trim_start();
                if let Some(rest) = self.rest.strip_prefix(',') {
                    self.rest = rest;
                }
            }
            return self
                .work
                .emit
                .factory
                .new_array_literal_expression(self.work.list(&items), false);
        }
        if let Some(rest) = self.rest.strip_prefix('"') {
            self.rest = rest;
            let end = self.rest.find('"').unwrap_or(self.rest.len());
            let text = &self.rest[..end];
            let node = self.work.str_lit(text, TokenFlags::None);
            self.rest = &self.rest[end..];
            if self.rest.starts_with('"') {
                self.rest = &self.rest[1..];
            }
            return node;
        }
        if let Some(rest) = self.rest.strip_prefix("null") {
            self.rest = rest;
            return self.work.emit.factory.new_keyword_expression(SyntaxKind::NullKeyword);
        }
        if let Some(rest) = self.rest.strip_prefix("true") {
            self.rest = rest;
            return self.work.emit.factory.new_keyword_expression(SyntaxKind::TrueKeyword);
        }
        if let Some(rest) = self.rest.strip_prefix("false") {
            self.rest = rest;
            return self.work.emit.factory.new_keyword_expression(SyntaxKind::FalseKeyword);
        }
        if let Some(rest) = self.rest.strip_prefix('{') {
            self.rest = rest;
            let mut props = Vec::new();
            loop {
                self.rest = self.rest.trim_start();
                if let Some(rest) = self.rest.strip_prefix('}') {
                    self.rest = rest;
                    break;
                }
                let key_end = self
                    .rest
                    .find(|ch: char| ch == ':' || ch.is_whitespace())
                    .unwrap_or(self.rest.len());
                let key = self.rest[..key_end].to_owned();
                self.rest = self.rest[key_end..].trim_start();
                if let Some(rest) = self.rest.strip_prefix(':') {
                    self.rest = rest;
                }
                let value = self.value();
                props.push(self.work.emit.factory.new_property_assignment(
                    None,
                    self.work.id(&key),
                    None,
                    None,
                    value,
                ));
                self.rest = self.rest.trim_start();
                if let Some(rest) = self.rest.strip_prefix(',') {
                    self.rest = rest;
                }
            }
            return self
                .work
                .emit
                .factory
                .new_object_literal_expression(self.work.list(&props), true);
        }
        let end = self
            .rest
            .find(|ch: char| !ch.is_ascii_digit() && ch != '.' && ch != '-')
            .unwrap_or(self.rest.len());
        let text = if end == 0 { "0" } else { &self.rest[..end] };
        let node = self.work.num_lit(text);
        self.rest = &self.rest[end.min(self.rest.len())..];
        node
    }
}

const RESOURCE_CONSTRUCTORS: &[&str] = &[
    "bindGroupLayout",
    "vertexLayout",
    "privateVar",
    "workgroupVar",
    "immediateVar",
    "const",
    "slot",
    "accessor",
    "mutableAccessor",
    "comptime",
    "fn",
    "vertexFn",
    "fragmentFn",
    "computeFn",
    "createBuffer",
    "createMutable",
    "createReadonly",
    "createUniform",
    "createQuerySet",
    "createPipeline",
    "createComputePipeline",
    "createGuardedComputePipeline",
    "createRenderPipeline",
    "createTexture",
    "createSampler",
    "createComparisonSampler",
    "struct",
    "unstruct",
    "createView",
];

fn labelled(node: P<Node>) -> Option<(String, P<Node>)> {
    match node.kind() {
        SyntaxKind::VariableDeclaration => {
            let name = node.name()?;
            let init = node.initializer()?;
            (name.kind() == SyntaxKind::Identifier).then(|| (name.text().to_owned(), init))
        }
        SyntaxKind::BinaryExpression => {
            let bin = node.as_binary_expression();
            if bin.operator_token.kind() != SyntaxKind::EqualsToken {
                return None;
            }
            let name = try_identifier(bin.left)?;
            Some((name, bin.right()))
        }
        SyntaxKind::PropertyAssignment => {
            let name = node.name()?;
            let init = node.initializer()?;
            (name.kind() == SyntaxKind::Identifier).then(|| (name.text().to_owned(), init))
        }
        SyntaxKind::PropertyDeclaration => {
            let name = try_identifier(node.name()?)?;
            let init = node.initializer()?;
            Some((name, init))
        }
        _ => None,
    }
}

fn try_identifier(node: P<Node>) -> Option<String> {
    match node.kind() {
        SyntaxKind::Identifier => Some(node.text().to_owned()),
        SyntaxKind::PrivateIdentifier => Some(node.text().trim_start_matches('#').to_owned()),
        SyntaxKind::PropertyAccessExpression => {
            try_identifier(node.as_property_access_expression().name)
        }
        _ => None,
    }
}

fn contains_resource(node: P<Node>, work: &Build) -> bool {
    if node.kind() == SyntaxKind::TaggedTemplateExpression {
        return contains_resource(node.as_tagged_template_expression().tag, work);
    }
    if node.kind() != SyntaxKind::CallExpression {
        return false;
    }
    if is_shell_implementation(node, work) {
        return true;
    }
    let Some(callee_raw) = node.expression() else {
        return false;
    };
    let callee = unwrap_parens(callee_raw);
    if callee.kind() == SyntaxKind::Identifier {
        return RESOURCE_CONSTRUCTORS.contains(&callee.text());
    }
    if callee.kind() == SyntaxKind::PropertyAccessExpression {
        let pa = callee.as_property_access_expression();
        let property = pa.name;
        if property.kind() == SyntaxKind::Identifier {
            if property.text() == "$name" {
                return false;
            }
            if RESOURCE_CONSTRUCTORS.contains(&property.text()) {
                return true;
            }
        }
        return contains_resource(pa.expression, work);
    }
    false
}

fn already_autonamed(node: P<Node>) -> bool {
    node.parent().is_some_and(|parent| {
        parent.kind() == SyntaxKind::CallExpression
            && slice_node_raw(parent).contains("__TYPEGPU_AUTONAME__")
    })
}

fn gather_aliases(node: P<Node>, aliases: &mut BTreeSet<String>) {
    if node.kind() == SyntaxKind::ImportDeclaration
        && let Some(spec) = node.module_specifier()
        && spec.kind() == SyntaxKind::StringLiteral
        && spec.text() == "typegpu"
        && let Some(clause) = node.import_clause()
    {
        if let Some(name) = clause.name()
            && name.kind() == SyntaxKind::Identifier
        {
            aliases.insert(name.text().to_owned());
        }
        if let Some(named) = clause.as_import_clause().named_bindings {
            if named.kind() == SyntaxKind::NamespaceImport
                && let Some(local) = named.name()
            {
                aliases.insert(format!("{}.tgpu", local.text()));
            } else if named.kind() == SyntaxKind::NamedImports {
                for &spec_node in named.elements() {
                    let Some(local) = spec_node.name() else {
                        continue;
                    };
                    let imported = if let Some(prop_name) = spec_node.property_name()
                        && prop_name.kind() == SyntaxKind::Identifier
                    {
                        prop_name.text()
                    } else {
                        local.text()
                    };
                    if imported == "tgpu" {
                        aliases.insert(local.text().to_owned());
                    }
                }
            }
        }
    }
    node.for_each_child(&mut |child| {
        gather_aliases(child, aliases);
        false
    });
}

fn is_tgpu_alias(node: P<Node>, work: &Build) -> bool {
    let mut path = String::new();
    let mut tail = unwrap_parens(node);
    loop {
        if tail.kind() == SyntaxKind::ElementAccessExpression {
            let elem = tail.as_element_access_expression();
            if elem.argument_expression.kind() == SyntaxKind::StringLiteral
                && elem.argument_expression.text() == "~unstable"
            {
                tail = unwrap_parens(elem.expression);
                continue;
            }
        }
        if tail.kind() == SyntaxKind::PropertyAccessExpression {
            let pa = tail.as_property_access_expression();
            let property = pa.name;
            if property.kind() != SyntaxKind::Identifier {
                break;
            }
            path = if path.is_empty() {
                property.text().to_owned()
            } else {
                format!("{}.{path}", property.text())
            };
            tail = unwrap_parens(pa.expression);
        } else if tail.kind() == SyntaxKind::Identifier {
            path = if path.is_empty() {
                tail.text().to_owned()
            } else {
                format!("{}.{path}", tail.text())
            };
            break;
        } else {
            break;
        }
    }
    work.aliases.contains(&path)
}

fn is_shell_implementation(node: P<Node>, work: &Build) -> bool {
    if node.kind() != SyntaxKind::CallExpression || node.arguments().len() != 1 {
        return false;
    }
    let Some(callee_raw) = node.expression() else {
        return false;
    };
    let callee = unwrap_parens(callee_raw);
    if callee.kind() != SyntaxKind::CallExpression {
        return false;
    }
    let Some(member_raw) = callee.expression() else {
        return false;
    };
    let member = unwrap_parens(member_raw);
    if member.kind() != SyntaxKind::PropertyAccessExpression {
        return false;
    }
    let pa = member.as_property_access_expression();
    let property = pa.name;
    if property.kind() != SyntaxKind::Identifier || !SHELLS.contains(&property.text()) {
        return false;
    }
    is_tgpu_alias(pa.expression, work)
}

fn function_marked(node: P<Node>) -> bool {
    let Some(body) = node.body() else {
        return false;
    };
    if body.kind() != SyntaxKind::Block || !body.can_have_statements() {
        return false;
    }
    let Some(statements) = body.statement_list() else {
        return false;
    };
    for &statement in statements.nodes() {
        if statement.kind() != SyntaxKind::ExpressionStatement {
            break;
        }
        let Some(expr) = statement.expression() else {
            break;
        };
        if is_use_gpu(expr) {
            return true;
        }
        if expr.kind() != SyntaxKind::StringLiteral {
            break;
        }
    }
    false
}

fn declaration_names_itself(slice: &str) -> bool {
    let Some(at) = slice.find("function") else {
        return true;
    };
    let after = slice[at + "function".len()..].trim_start();
    after.starts_with(|ch: char| ch == '_' || ch.is_ascii_alphabetic())
}

fn function_names_itself(node: P<Node>, work: &Build) -> bool {
    if node.kind() == SyntaxKind::ArrowFunction {
        return false;
    }
    let Some(start) = byte_at(&work.code, skip_trivia(&work.code, pos_u32(node))) else {
        return false;
    };
    let Some(end) = byte_at(&work.code, end_u32(node)) else {
        return false;
    };
    declaration_names_itself(&work.code[start..end])
}

fn function_name(node: P<Node>, work: &Build) -> String {
    if function_names_itself(node, work)
        && let Some(own) = node.name()
        && own.kind() == SyntaxKind::Identifier
    {
        return own.text().to_owned();
    }
    let Some(parent) = node.parent() else {
        return String::new();
    };
    match parent.kind() {
        SyntaxKind::VariableDeclaration
        | SyntaxKind::PropertyAssignment
        | SyntaxKind::BindingElement => {
            if let Some(name) = parent.name()
                && name.kind() == SyntaxKind::Identifier
            {
                return name.text().to_owned();
            }
        }
        SyntaxKind::BinaryExpression => {
            let bin = parent.as_binary_expression();
            if bin.operator_token.kind() == SyntaxKind::EqualsToken
                && bin.left.kind() == SyntaxKind::Identifier
            {
                return bin.left.text().to_owned();
            }
        }
        _ => {}
    }
    String::new()
}

const SHELLS: &[&str] = &["fn", "vertexFn", "fragmentFn", "computeFn"];

fn unwrap_parens(mut node: P<Node>) -> P<Node> {
    while node.kind() == SyntaxKind::ParenthesizedExpression {
        let Some(inner) = node.expression() else {
            break;
        };
        node = inner;
    }
    node
}

fn leading_comments(node: P<Node>, code: &str) -> Vec<(usize, usize)> {
    let text = get_source_file_of_node(node).map_or_else(|| alloc_str(code), |sf| sf.text());
    get_leading_comment_ranges(text, pos_u32(node))
        .filter_map(|range| {
            let start = range.pos() as usize;
            let end = range.end() as usize;
            (end <= code.len() && start <= end).then_some((start, end))
        })
        .collect()
}

#[derive(Clone, Debug, Default)]
struct Tinyest {
    params: Vec<String>,
    body: String,
    externals: Vec<String>,
    unsupported: Option<String>,
    span: Option<(u32, u32)>,
}

fn transpile_marked(node: P<Node>, code: &str) -> Tinyest {
    let params = node.parameter_list().map_or_else(Vec::new, |list| {
        list.nodes()
            .iter()
            .filter_map(|param| {
                let name = param.name()?;
                (name.kind() == SyntaxKind::Identifier).then(|| name.text().to_owned())
            })
            .collect::<Vec<_>>()
    });
    let mut ctx =
        TinyCtx { declared: params.clone(), externals: Vec::new(), unsupported: None, span: None };
    let statements = match node.body() {
        Some(body) if body.kind() == SyntaxKind::Block => body
            .statements()
            .iter()
            .copied()
            .filter(|&statement| !is_directive(statement))
            .filter_map(|statement| transpile_statement(statement, code, &mut ctx))
            .collect::<Vec<_>>(),
        Some(body) => vec![format!("[10, {}]", transpile_expr(body, code, &mut ctx))],
        None => Vec::new(),
    };
    if let Some(message) = ctx.unsupported {
        return Tinyest {
            params,
            body: "[0, []]".to_owned(),
            externals: Vec::new(),
            unsupported: Some(message),
            span: ctx.span,
        };
    }
    Tinyest {
        params,
        body: format!("[0, [{}]]", statements.join(", ")),
        externals: ctx.externals,
        unsupported: None,
        span: None,
    }
}

fn refuse(ctx: &mut TinyCtx, code: &str, node: P<Node>, message: String) {
    if ctx.unsupported.is_some() {
        return;
    }
    ctx.unsupported = Some(message);
    let start = skip_trivia(code, pos_u32(node));
    let end = end_u32(node).max(start);
    ctx.span = Some((start, end));
}

struct TinyCtx {
    declared: Vec<String>,
    externals: Vec<String>,
    unsupported: Option<String>,
    span: Option<(u32, u32)>,
}

fn is_directive(statement: P<Node>) -> bool {
    statement.kind() == SyntaxKind::ExpressionStatement
        && statement.expression().is_some_and(|expr| expr.kind() == SyntaxKind::StringLiteral)
}

fn transpile_statement(node: P<Node>, code: &str, ctx: &mut TinyCtx) -> Option<String> {
    match node.kind() {
        SyntaxKind::EmptyStatement | SyntaxKind::NotEmittedStatement => None,
        SyntaxKind::ExpressionStatement => {
            node.expression().map(|expr| transpile_expr(expr, code, ctx))
        }
        SyntaxKind::ReturnStatement => Some(match node.expression() {
            None => "[10]".to_owned(),
            Some(expr) => format!("[10, {}]", transpile_expr(expr, code, ctx)),
        }),
        SyntaxKind::VariableStatement => {
            let list = node.as_variable_statement().declaration_list;
            if !list.flags().intersects(NodeFlags::Const | NodeFlags::Let) {
                refuse(ctx, code, node, "`var` declarations are not supported.".to_owned());
                return None;
            }
            let &decl = list.as_variable_declaration_list().declarations.nodes().first()?;
            let id = decl.name()?;
            let name = if id.kind() == SyntaxKind::Identifier {
                id.text().to_owned()
            } else {
                slice_node(code, id)
            };
            ctx.declared.push(name.clone());
            let kind = if list.flags().intersects(NodeFlags::Const) { 13 } else { 12 };
            Some(match decl.initializer() {
                None => format!("[{kind}, \"{name}\"]"),
                Some(init) => format!("[{kind}, \"{name}\", {}]", transpile_expr(init, code, ctx)),
            })
        }
        SyntaxKind::IfStatement => {
            let ifs = node.as_if_statement();
            let test = transpile_expr(ifs.expression, code, ctx);
            let then = statement_or_block(ifs.then_statement, code, ctx);
            Some(match ifs.else_statement {
                None => format!("[11, {test}, {then}]"),
                Some(else_) => {
                    format!("[11, {test}, {then}, {}]", statement_or_block(else_, code, ctx))
                }
            })
        }
        SyntaxKind::WhileStatement => {
            let expr = node.expression()?;
            Some(format!(
                "[15, {}, {}]",
                transpile_expr(expr, code, ctx),
                statement_or_block(node.statement(), code, ctx)
            ))
        }
        SyntaxKind::ForStatement => {
            let fors = node.as_for_statement();
            let init_js = match fors.initializer {
                None => "null".to_owned(),
                Some(init) if init.kind() == SyntaxKind::VariableDeclarationList => {
                    transpile_for_init(init, code, ctx)
                }
                Some(init) => transpile_expr(init, code, ctx),
            };
            let test_js = fors
                .condition
                .map_or_else(|| "null".to_owned(), |test| transpile_expr(test, code, ctx));
            let update_js = fors
                .incrementor
                .map_or_else(|| "null".to_owned(), |upd| transpile_expr(upd, code, ctx));
            Some(format!(
                "[14, {init_js}, {test_js}, {update_js}, {}]",
                statement_or_block(fors.statement(), code, ctx)
            ))
        }
        SyntaxKind::ContinueStatement => Some("[16]".to_owned()),
        SyntaxKind::BreakStatement => Some("[17]".to_owned()),
        SyntaxKind::ForOfStatement => {
            let for_of = node.as_for_in_or_of_statement();
            let init = for_of.initializer;
            let declaration = if init.kind() == SyntaxKind::VariableDeclarationList {
                let vdl = init.as_variable_declaration_list();
                let kind = if init.flags().intersects(NodeFlags::Const) { 13 } else { 12 };
                match vdl.declarations.nodes().first().copied() {
                    None => format!("[{kind}]"),
                    Some(decl) => {
                        let name = match decl.name() {
                            Some(id) if id.kind() == SyntaxKind::Identifier => id.text().to_owned(),
                            Some(id) => slice_node(code, id),
                            None => String::new(),
                        };
                        ctx.declared.push(name.clone());
                        format!("[{kind}, \"{name}\"]")
                    }
                }
            } else {
                transpile_expr(init, code, ctx)
            };
            Some(format!(
                "[18, {declaration}, {}, {}]",
                transpile_expr(for_of.expression, code, ctx),
                statement_or_block(for_of.statement(), code, ctx)
            ))
        }
        SyntaxKind::SwitchStatement => {
            let sw = node.as_switch_statement();
            let mut clauses = Vec::new();
            for &clause in sw.case_block.as_case_block().clauses.nodes() {
                let test = if clause.kind() == SyntaxKind::DefaultClause {
                    "null".to_owned()
                } else {
                    clause
                        .expression()
                        .map_or_else(|| "null".to_owned(), |expr| transpile_expr(expr, code, ctx))
                };
                let mut statements = Vec::new();
                for &statement in clause.statements() {
                    if let Some(text) = transpile_statement(statement, code, ctx) {
                        statements.push(text);
                    }
                }
                clauses.push(format!("[{test}, [{}]]", statements.join(", ")));
            }
            Some(format!(
                "[19, {}, [{}]]",
                transpile_expr(sw.expression, code, ctx),
                clauses.join(", ")
            ))
        }
        SyntaxKind::Block => Some(transpile_block(node, code, ctx)),
        _ => {
            refuse(ctx, code, node, unsupported_message(node.kind()));
            None
        }
    }
}

fn unsupported_message(kind: SyntaxKind) -> String {
    match kind {
        SyntaxKind::ArrowFunction => "Arrow functions are not supported inside TGSL.".to_owned(),
        SyntaxKind::SpreadElement
        | SyntaxKind::SpreadAssignment
        | SyntaxKind::OmittedExpression => "Spread elements are not supported in TGSL.".to_owned(),
        SyntaxKind::MethodDeclaration | SyntaxKind::GetAccessor | SyntaxKind::SetAccessor => {
            "Object method elements are not supported in TGSL.".to_owned()
        }
        SyntaxKind::DoStatement => "Unsupported JS functionality: DoWhileStatement".to_owned(),
        SyntaxKind::TemplateExpression
        | SyntaxKind::NoSubstitutionTemplateLiteral
        | SyntaxKind::TaggedTemplateExpression => {
            "Unsupported JS functionality: TemplateLiteral".to_owned()
        }
        other => format!("Unsupported JS functionality: {other:?}"),
    }
}

fn transpile_for_init(list: P<Node>, code: &str, ctx: &mut TinyCtx) -> String {
    let Some(&decl) = list.as_variable_declaration_list().declarations.nodes().first() else {
        return "null".to_owned();
    };
    let Some(id) = decl.name() else {
        return "null".to_owned();
    };
    let name = id.text().to_owned();
    ctx.declared.push(name.clone());
    match decl.initializer() {
        None => format!("[12, \"{name}\"]"),
        Some(init) => format!("[12, \"{name}\", {}]", transpile_expr(init, code, ctx)),
    }
}

fn statement_or_block(node: P<Node>, code: &str, ctx: &mut TinyCtx) -> String {
    if node.kind() == SyntaxKind::Block {
        transpile_block(node, code, ctx)
    } else {
        transpile_statement(node, code, ctx).unwrap_or_else(|| "[0, []]".to_owned())
    }
}

fn transpile_block(node: P<Node>, code: &str, ctx: &mut TinyCtx) -> String {
    let statements = node
        .statements()
        .iter()
        .copied()
        .filter(|&statement| !is_directive(statement))
        .filter_map(|statement| transpile_statement(statement, code, ctx))
        .collect::<Vec<_>>();
    format!("[0, [{}]]", statements.join(", "))
}

fn transpile_expr(node: P<Node>, code: &str, ctx: &mut TinyCtx) -> String {
    let mut node = unwrap_parens(node);
    while matches!(
        node.kind(),
        SyntaxKind::AsExpression
            | SyntaxKind::SatisfiesExpression
            | SyntaxKind::TypeAssertionExpression
            | SyntaxKind::NonNullExpression
            | SyntaxKind::ExpressionWithTypeArguments
            | SyntaxKind::ParenthesizedExpression
    ) {
        let Some(inner) = node.expression() else {
            break;
        };
        node = inner;
    }
    if let Some(chain) = external_chain(node, ctx) {
        if !ctx.externals.contains(&chain) {
            ctx.externals.push(chain.clone());
        }
        return format!("\"{chain}\"");
    }
    if node.kind() == SyntaxKind::PropertyAccessExpression {
        let pa = node.as_property_access_expression();
        let property = pa.name;
        if property.kind() == SyntaxKind::Identifier
            && property.text() == "$"
            && let Some(chain) = external_chain(unwrap_parens(pa.expression), ctx)
        {
            if !ctx.externals.contains(&chain) {
                ctx.externals.push(chain.clone());
            }
            return format!("[7, \"{chain}\", \"$\"]");
        }
    }
    match node.kind() {
        SyntaxKind::Identifier => format!("\"{}\"", node.text()),
        SyntaxKind::NumericLiteral => format!("[5, \"{}\"]", numeric_text(node, code)),
        SyntaxKind::StringLiteral => format!("[103, \"{}\"]", node.text()),
        SyntaxKind::TrueKeyword => "true".to_owned(),
        SyntaxKind::FalseKeyword => "false".to_owned(),
        SyntaxKind::NullKeyword => "[106]".to_owned(),
        SyntaxKind::BinaryExpression => {
            let bin = node.as_binary_expression();
            let op_kind = bin.operator_token.kind();
            let right_node = bin.right();
            if op_kind == SyntaxKind::EqualsToken && is_restored_assign(right_node) {
                return transpile_expr(right_node, code, ctx);
            }
            let op = operator_text(op_kind);
            let left = transpile_expr(bin.left, code, ctx);
            let right = transpile_expr(right_node, code, ctx);
            let kind = if is_assignment(op_kind) {
                2
            } else if is_logical(op_kind) {
                3
            } else {
                1
            };
            format!("[{kind}, {left}, \"{op}\", {right}]")
        }
        SyntaxKind::PrefixUnaryExpression => {
            let pre = node.as_prefix_unary_expression();
            if matches!(pre.operator, SyntaxKind::PlusPlusToken | SyntaxKind::MinusMinusToken) {
                refuse(
                    ctx,
                    code,
                    node,
                    "Prefix update expressions are not supported in WGSL.".to_owned(),
                );
                return format!("\"{}\"", slice_node(code, node));
            }
            format!(
                "[4, \"{}\", {}]",
                operator_text(pre.operator),
                transpile_expr(pre.operand, code, ctx)
            )
        }
        SyntaxKind::PostfixUnaryExpression => {
            let post = node.as_postfix_unary_expression();
            format!(
                "[102, \"{}\", {}]",
                operator_text(post.operator),
                transpile_expr(post.operand, code, ctx)
            )
        }
        SyntaxKind::PropertyAccessExpression => {
            let pa = node.as_property_access_expression();
            let property = pa.name.text();
            format!("[7, {}, \"{property}\"]", transpile_expr(pa.expression, code, ctx))
        }
        SyntaxKind::ElementAccessExpression => {
            let elem = node.as_element_access_expression();
            format!(
                "[8, {}, {}]",
                transpile_expr(elem.expression, code, ctx),
                transpile_expr(elem.argument_expression, code, ctx)
            )
        }
        SyntaxKind::CallExpression => {
            if let Some(restored) = restore_overload(node, code, ctx) {
                return restored;
            }
            let callee = node
                .expression()
                .map_or_else(|| "\"_\"".to_owned(), |expr| transpile_expr(expr, code, ctx));
            let args = node
                .arguments()
                .iter()
                .map(|&arg| transpile_expr(arg, code, ctx))
                .collect::<Vec<_>>();
            format!("[6, {callee}, [{}]]", args.join(", "))
        }
        SyntaxKind::ConditionalExpression => {
            let cond = node.as_conditional_expression();
            format!(
                "[105, {}, {}, {}]",
                transpile_expr(cond.condition, code, ctx),
                transpile_expr(cond.when_true, code, ctx),
                transpile_expr(cond.when_false, code, ctx)
            )
        }
        SyntaxKind::ArrayLiteralExpression => {
            let items = node
                .elements()
                .iter()
                .map(|&item| transpile_expr(item, code, ctx))
                .collect::<Vec<_>>();
            format!("[100, [{}]]", items.join(", "))
        }
        SyntaxKind::ObjectLiteralExpression => {
            let props = node
                .properties()
                .iter()
                .filter_map(|&prop_node| {
                    if matches!(
                        prop_node.kind(),
                        SyntaxKind::SpreadAssignment
                            | SyntaxKind::MethodDeclaration
                            | SyntaxKind::GetAccessor
                            | SyntaxKind::SetAccessor
                    ) {
                        refuse(ctx, code, prop_node, unsupported_message(prop_node.kind()));
                        return None;
                    }
                    if prop_node.kind() == SyntaxKind::ShorthandPropertyAssignment {
                        let key = prop_node.name()?.text().to_owned();
                        if !ctx.declared.iter().any(|declared| declared == &key)
                            && !ctx.externals.contains(&key)
                        {
                            ctx.externals.push(key.clone());
                        }
                        return Some(format!("{key}: \"{key}\""));
                    }
                    if prop_node.kind() != SyntaxKind::PropertyAssignment {
                        return None;
                    }
                    let name = prop_node.name()?;
                    let key = if name.kind() == SyntaxKind::Identifier {
                        name.text().to_owned()
                    } else {
                        slice_node(code, name)
                    };
                    let init = prop_node.initializer()?;
                    Some(format!("{key}: {}", transpile_expr(init, code, ctx)))
                })
                .collect::<Vec<_>>();
            format!("[104, {{ {} }}]", props.join(", "))
        }
        SyntaxKind::ParenthesizedExpression => node
            .expression()
            .map_or_else(|| "\"_\"".to_owned(), |expr| transpile_expr(expr, code, ctx)),
        _ => {
            refuse(ctx, code, node, unsupported_message(node.kind()));
            format!("\"{}\"", slice_node(code, node))
        }
    }
}

fn is_restored_assign(node: P<Node>) -> bool {
    if node.kind() != SyntaxKind::CallExpression {
        return false;
    }
    let Some(callee_raw) = node.expression() else {
        return false;
    };
    let callee = unwrap_parens(callee_raw);
    callee.kind() == SyntaxKind::Identifier
        && matches!(
            callee.text(),
            "__tsover_add" | "__tsover_sub" | "__tsover_mul" | "__tsover_div" | "__tsover_mod"
        )
}

fn restore_overload(node: P<Node>, code: &str, ctx: &mut TinyCtx) -> Option<String> {
    let callee = unwrap_parens(node.expression()?);
    if callee.kind() != SyntaxKind::Identifier {
        return None;
    }
    let op = match callee.text() {
        "__tsover_add" => "+",
        "__tsover_sub" => "-",
        "__tsover_mul" => "*",
        "__tsover_div" => "/",
        "__tsover_mod" => "%",
        _ => return None,
    };
    let args = node.arguments();
    if args.len() != 2 {
        return None;
    }
    let left_node = args[0];
    let right_node = args[1];
    let left = transpile_expr(left_node, code, ctx);
    let right = if right_node.kind() == SyntaxKind::CallExpression
        && let Some(inner_raw) = right_node.expression()
    {
        let inner = unwrap_parens(inner_raw);
        let inner_args = right_node.arguments();
        if inner.kind() == SyntaxKind::Identifier
            && inner.text() == callee.text()
            && inner_args.len() == 2
            && slice_node(code, inner_args[0]) == slice_node(code, left_node)
        {
            transpile_expr(inner_args[1], code, ctx)
        } else {
            transpile_expr(right_node, code, ctx)
        }
    } else {
        transpile_expr(right_node, code, ctx)
    };
    let left_text = slice_node(code, left_node);
    let assign = node.parent().is_some_and(|parent| {
        if parent.kind() != SyntaxKind::BinaryExpression {
            return false;
        }
        let bin = parent.as_binary_expression();
        bin.operator_token.kind() == SyntaxKind::EqualsToken
            && bin.right() == node
            && slice_node(code, bin.left) == left_text
    });
    if assign {
        let symbol = match op {
            "+" => "+=",
            "-" => "-=",
            "*" => "*=",
            "/" => "/=",
            "%" => "%=",
            _ => op,
        };
        return Some(format!("[2, {left}, \"{symbol}\", {right}]"));
    }
    Some(format!("[1, {left}, \"{op}\", {right}]"))
}

fn external_chain(node: P<Node>, ctx: &TinyCtx) -> Option<String> {
    match node.kind() {
        SyntaxKind::Identifier => {
            let name = node.text();
            if ctx.declared.iter().any(|declared| declared == name) || is_builtin(name) {
                None
            } else {
                Some(name.to_owned())
            }
        }
        SyntaxKind::PropertyAccessExpression => {
            let pa = node.as_property_access_expression();
            let property = pa.name;
            if property.kind() == SyntaxKind::PrivateIdentifier {
                let object = external_chain(unwrap_parens(pa.expression), ctx)?;
                let name = property.text().trim_start_matches('#');
                return Some(format!("{object}.#{name}"));
            }
            if property.kind() != SyntaxKind::Identifier || property.text() == "$" {
                return None;
            }
            let object = external_chain(unwrap_parens(pa.expression), ctx)?;
            Some(format!("{object}.{}", property.text()))
        }
        SyntaxKind::ElementAccessExpression => {
            external_chain(unwrap_parens(node.as_element_access_expression().expression), ctx)
        }
        SyntaxKind::ThisKeyword => Some("this".to_owned()),
        _ => None,
    }
}

fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "undefined"
            | "NaN"
            | "Infinity"
            | "Math"
            | "Number"
            | "console"
            | "globalThis"
            | "__tsover_add"
            | "__tsover_sub"
            | "__tsover_mul"
            | "__tsover_div"
            | "__tsover_mod"
    )
}

fn is_assignment(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::EqualsToken
            | SyntaxKind::PlusEqualsToken
            | SyntaxKind::MinusEqualsToken
            | SyntaxKind::AsteriskEqualsToken
            | SyntaxKind::SlashEqualsToken
            | SyntaxKind::PercentEqualsToken
            | SyntaxKind::AsteriskAsteriskEqualsToken
            | SyntaxKind::AmpersandAmpersandEqualsToken
            | SyntaxKind::BarBarEqualsToken
            | SyntaxKind::QuestionQuestionEqualsToken
            | SyntaxKind::AmpersandEqualsToken
            | SyntaxKind::BarEqualsToken
            | SyntaxKind::CaretEqualsToken
            | SyntaxKind::LessThanLessThanEqualsToken
            | SyntaxKind::GreaterThanGreaterThanEqualsToken
            | SyntaxKind::GreaterThanGreaterThanGreaterThanEqualsToken
    )
}

fn is_logical(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::AmpersandAmpersandToken
            | SyntaxKind::BarBarToken
            | SyntaxKind::QuestionQuestionToken
    )
}

fn operator_text(kind: SyntaxKind) -> &'static str {
    match kind {
        SyntaxKind::PlusToken => "+",
        SyntaxKind::MinusToken => "-",
        SyntaxKind::AsteriskToken => "*",
        SyntaxKind::SlashToken => "/",
        SyntaxKind::PercentToken => "%",
        SyntaxKind::AsteriskAsteriskToken => "**",
        SyntaxKind::AmpersandToken => "&",
        SyntaxKind::BarToken => "|",
        SyntaxKind::CaretToken => "^",
        SyntaxKind::LessThanLessThanToken => "<<",
        SyntaxKind::GreaterThanGreaterThanToken => ">>",
        SyntaxKind::GreaterThanGreaterThanGreaterThanToken => ">>>",
        SyntaxKind::EqualsToken => "=",
        SyntaxKind::PlusEqualsToken => "+=",
        SyntaxKind::MinusEqualsToken => "-=",
        SyntaxKind::AsteriskEqualsToken => "*=",
        SyntaxKind::SlashEqualsToken => "/=",
        SyntaxKind::PercentEqualsToken => "%=",
        SyntaxKind::AsteriskAsteriskEqualsToken => "**=",
        SyntaxKind::AmpersandEqualsToken => "&=",
        SyntaxKind::BarEqualsToken => "|=",
        SyntaxKind::CaretEqualsToken => "^=",
        SyntaxKind::LessThanLessThanEqualsToken => "<<=",
        SyntaxKind::GreaterThanGreaterThanEqualsToken => ">>=",
        SyntaxKind::GreaterThanGreaterThanGreaterThanEqualsToken => ">>>=",
        SyntaxKind::AmpersandAmpersandEqualsToken => "&&=",
        SyntaxKind::BarBarEqualsToken => "||=",
        SyntaxKind::QuestionQuestionEqualsToken => "??=",
        SyntaxKind::InKeyword => "in",
        SyntaxKind::InstanceOfKeyword => "instanceof",
        SyntaxKind::EqualsEqualsToken => "==",
        SyntaxKind::EqualsEqualsEqualsToken => "===",
        SyntaxKind::ExclamationEqualsToken => "!=",
        SyntaxKind::ExclamationEqualsEqualsToken => "!==",
        SyntaxKind::LessThanToken => "<",
        SyntaxKind::LessThanEqualsToken => "<=",
        SyntaxKind::GreaterThanToken => ">",
        SyntaxKind::GreaterThanEqualsToken => ">=",
        SyntaxKind::AmpersandAmpersandToken => "&&",
        SyntaxKind::BarBarToken => "||",
        SyntaxKind::QuestionQuestionToken => "??",
        SyntaxKind::TildeToken => "~",
        SyntaxKind::PlusPlusToken => "++",
        SyntaxKind::MinusMinusToken => "--",
        SyntaxKind::ExclamationToken => "!",
        _ => "?",
    }
}

fn numeric_text(node: P<Node>, code: &str) -> String {
    let raw = slice_node(code, node);
    js_number_text(jsnum::from_string(&raw).0)
}

#[expect(
    clippy::float_cmp,
    clippy::cast_possible_truncation,
    reason = "exact IEEE-754 integer check for finite f64 below MAX_SAFE_INTEGER"
)]
fn js_number_text(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-Infinity".to_owned()
        } else {
            "Infinity".to_owned()
        };
    }
    if value == value.trunc() && value.abs() < 9_007_199_254_740_992.0 {
        return format!("{}", value as i64);
    }
    format!("{value}")
}

fn slice_node(code: &str, node: P<Node>) -> String {
    let start = skip_trivia(code, pos_u32(node)) as usize;
    let end = (end_u32(node) as usize).min(code.len());
    let start = start.min(end);
    code[start..end].to_owned()
}
