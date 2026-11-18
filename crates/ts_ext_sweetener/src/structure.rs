//! Structural comparison of Sweetener's expanded TypeScript.
//!
//! This is not `TypeGPU`'s comparator. A Sweetener case is `input.sts`
//! against `theirs.ts`, and both sides are ordinary TypeScript after
//! expansion. Formatting is not a difference: line breaks, indentation, a
//! trailing comma, and a redundant pair of parentheses. Stripping every
//! `(` and `)` is not this comparison. `() => ({ tag: "None" })` is an
//! object literal and `() => { tag: "None" }` is a block. Those are
//! different programs.
//!
//! The parser is `tsrs_parser`. The expander's printer is not used here,
//! because the printer is what the two sides are being compared about.

use std::fmt::Write as _;
use tsrs_ast::{Kind, Node, NodeFlags, SourceFileParseOptions};
use tsrs_core::{P, ScriptKind, arena::Region};
use tsrs_parser::parse_source_file_owned;

/// A stable dump of the syntax tree. Two texts with the same dump are the
/// same program up to formatting. `None` when the text does not parse.
#[must_use]
pub fn dump(text: &str, tsx: bool) -> Option<String> {
    let region = Region::new(64 << 10);
    let _guard = region.enter();
    let opts = SourceFileParseOptions {
        file_name: if tsx { "/shape.tsx" } else { "/shape.ts" }.to_owned(),
        ..SourceFileParseOptions::default()
    };
    let kind = if tsx { ScriptKind::TSX } else { ScriptKind::TS };
    let file = parse_source_file_owned(opts, text.to_owned(), kind);
    let mut out = String::new();
    write_node(file.as_node(), 0, &mut out);
    Some(out)
}

/// True when both texts are the same program up to formatting. Falls back
/// to `false` when either side does not parse, so a broken expansion is not
/// accepted as shape.
#[must_use]
pub fn same(left: &str, right: &str, tsx: bool) -> bool {
    match (dump(left, tsx), dump(right, tsx)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Unified diff of two texts. The labels are always `theirs.ts` and
/// `ours.ts`: no temporary directory and no timestamp, so the file is the
/// same on every machine.
#[must_use]
pub fn unified(theirs: &str, ours: &str) -> String {
    let left = lines_of(theirs);
    let right = lines_of(ours);
    let mut out = String::from("--- theirs.ts\n+++ ours.ts\n");
    for hunk in hunks(&left, &right) {
        let _ = writeln!(
            out,
            "@@ -{},{} +{},{} @@",
            hunk.left_start, hunk.left_len, hunk.right_start, hunk.right_len
        );
        for line in &hunk.lines {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn lines_of(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

struct Hunk {
    left_start: usize,
    left_len: usize,
    right_start: usize,
    right_len: usize,
    lines: Vec<String>,
}

fn hunks(left: &[&str], right: &[&str]) -> Vec<Hunk> {
    let script = edits(left, right);
    let mut placed = Vec::new();
    let mut left_at = 0usize;
    let mut right_at = 0usize;
    for edit in &script {
        placed.push((*edit, left_at, right_at));
        match edit {
            Edit::Equal => {
                left_at += 1;
                right_at += 1;
            }
            Edit::Delete => left_at += 1,
            Edit::Insert => right_at += 1,
        }
    }
    let mut hunks = Vec::new();
    let mut index = 0usize;
    while index < placed.len() {
        if placed[index].0 == Edit::Equal {
            index += 1;
            continue;
        }
        let change = index;
        while index < placed.len() && placed[index].0 != Edit::Equal {
            index += 1;
        }
        let from = change.saturating_sub(3);
        let to = (index + 3).min(placed.len());
        let mut end = index;
        while end < to && placed[end].0 == Edit::Equal {
            end += 1;
        }
        let mut lines = Vec::new();
        for (edit, left_index, right_index) in &placed[from..end] {
            lines.push(match edit {
                Edit::Equal => format!(" {}", left[*left_index]),
                Edit::Delete => format!("-{}", left[*left_index]),
                Edit::Insert => format!("+{}", right[*right_index]),
            });
        }
        let left_len = lines.iter().filter(|line| !line.starts_with('+')).count();
        let right_len = lines.iter().filter(|line| !line.starts_with('-')).count();
        hunks.push(Hunk {
            left_start: placed[from].1 + 1,
            left_len,
            right_start: placed[from].2 + 1,
            right_len,
            lines,
        });
        index = end;
    }
    hunks
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Edit {
    Equal,
    Delete,
    Insert,
}

fn edits(left: &[&str], right: &[&str]) -> Vec<Edit> {
    let n = left.len();
    let m = right.len();
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if left[i] == right[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if left[i] == right[j] {
            out.push(Edit::Equal);
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            out.push(Edit::Delete);
            i += 1;
        } else {
            out.push(Edit::Insert);
            j += 1;
        }
    }
    while i < n {
        out.push(Edit::Delete);
        i += 1;
    }
    while j < m {
        out.push(Edit::Insert);
        j += 1;
    }
    out
}

fn write_node(node: P<Node>, depth: usize, out: &mut String) {
    let node = unwrap_paren(node);
    let kind = node.kind();
    // `&&` and `||` are associative. `(a && b) && c` and `a && (b && c)`
    // are the same expansion. An object literal and a block are not.
    if let Some(op) = logic_op(node) {
        out.push_str(&"  ".repeat(depth));
        let _ = writeln!(out, "Chain {op}");
        let mut operands = Vec::new();
        flatten_logic(node, op, &mut operands);
        for operand in operands {
            write_node(operand, depth + 1, out);
        }
        return;
    }
    out.push_str(&"  ".repeat(depth));
    let _ = write!(out, "{kind:?}");
    write_node_extra(node, out);
    let mut children = Vec::new();
    node.for_each_child_static(&mut |child| {
        // A trailing comma is formatting. The module note says so; the dump
        // used to keep `CommaToken`, so import lists differed by that comma.
        if child.kind() != Kind::CommaToken {
            children.push(child);
        }
        false
    });
    if children.is_empty() {
        let text = leaf_text(node);
        if !text.is_empty() {
            out.push(' ');
            out.push_str(text);
        }
    }
    out.push('\n');
    for child in children {
        write_node(child, depth + 1, out);
    }
}

fn write_node_extra(node: P<Node>, out: &mut String) {
    match node.kind() {
        Kind::PrefixUnaryExpression => {
            let _ = write!(out, " {:?}", node.as_prefix_unary_expression().operator());
        }
        Kind::PostfixUnaryExpression => {
            let _ = write!(out, " {:?}", node.as_postfix_unary_expression().operator());
        }
        Kind::TypeOperator => {
            let _ = write!(out, " {:?}", node.as_type_operator_node().operator());
        }
        Kind::VariableDeclarationList => {
            let flags = node.flags() & NodeFlags::BlockScoped;
            let _ = write!(out, " {flags:?}");
        }
        Kind::HeritageClause => {
            let _ = write!(out, " {:?}", node.as_heritage_clause().token());
        }
        Kind::MetaProperty => {
            let _ = write!(out, " {:?}", node.as_meta_property().keyword_token());
        }
        Kind::ModuleDeclaration => {
            let _ = write!(out, " {:?}", node.as_module_declaration().keyword());
        }
        Kind::ImportAttributes => {
            let _ = write!(out, " {:?}", node.as_import_attributes().token());
        }
        Kind::ImportClause => {
            let phase = node.as_import_clause().phase_modifier();
            if phase != Kind::Unknown {
                let _ = write!(out, " {phase:?}");
            }
        }
        Kind::ImportSpecifier if node.as_import_specifier().is_type_only() => {
            out.push_str(" type");
        }
        Kind::ExportDeclaration if node.as_export_declaration().is_type_only() => {
            out.push_str(" type");
        }
        Kind::ExportSpecifier if node.as_export_specifier().is_type_only() => {
            out.push_str(" type");
        }
        Kind::ExportAssignment if node.as_export_assignment().is_export_equals() => {
            out.push_str(" =");
        }
        Kind::ImportEqualsDeclaration if node.as_import_equals_declaration().is_type_only() => {
            out.push_str(" type");
        }
        Kind::ImportType if node.as_import_type_node().is_type_of() => {
            out.push_str(" typeof");
        }
        _ => {}
    }
}

fn leaf_text(node: P<Node>) -> &'static str {
    match node.kind() {
        Kind::Identifier => node.as_identifier().text(),
        Kind::PrivateIdentifier => node.as_private_identifier().text(),
        Kind::StringLiteral => node.as_string_literal().text(),
        Kind::NumericLiteral => node.as_numeric_literal().text(),
        Kind::BigIntLiteral => node.as_big_int_literal().text(),
        Kind::RegularExpressionLiteral => node.as_regular_expression_literal().text(),
        Kind::NoSubstitutionTemplateLiteral => node.as_no_substitution_template_literal().text(),
        Kind::TemplateHead => node.as_template_head().text(),
        Kind::TemplateMiddle => node.as_template_middle().text(),
        Kind::TemplateTail => node.as_template_tail().text(),
        Kind::JsxText => node.as_jsx_text().text(),
        _ => "",
    }
}

fn unwrap_paren(mut node: P<Node>) -> P<Node> {
    loop {
        match node.kind() {
            Kind::ParenthesizedExpression => {
                node = node.as_parenthesized_expression().expression();
            }
            Kind::ParenthesizedType => {
                node = node.as_parenthesized_type_node().type_();
            }
            _ => return node,
        }
    }
}

fn logic_op(node: P<Node>) -> Option<&'static str> {
    let node = unwrap_paren(node);
    if node.kind() != Kind::BinaryExpression {
        return None;
    }
    match node.as_binary_expression().operator_token().kind() {
        Kind::AmpersandAmpersandToken => Some("&&"),
        Kind::BarBarToken => Some("||"),
        _ => None,
    }
}

fn flatten_logic(node: P<Node>, op: &str, operands: &mut Vec<P<Node>>) {
    let node = unwrap_paren(node);
    if logic_op(node) == Some(op) {
        let bin = node.as_binary_expression();
        flatten_logic(bin.left(), op, operands);
        flatten_logic(bin.right(), op, operands);
        return;
    }
    operands.push(node);
}
