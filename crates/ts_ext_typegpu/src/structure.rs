//! Structural comparison of two expanded files.
//!
//! A byte comparison fails on formatting that does not change the program:
//! line breaks, indentation, a trailing comma, or a redundant pair of
//! parentheses. A text comparison that strips every `(` and `)` goes too
//! far: it reads `() => ({ tag: "None" })` and `() => { tag: "None" }` as
//! the same text, but the first is an object literal and the second is a
//! block with a label. That is a real expansion bug, not shape.
//!
//! This module parses both sides and compares the syntax tree. Whitespace
//! and comments are not in the tree, so they are shape. A trailing comma is
//! not a node, so it is shape. A parenthesized expression is skipped, so
//! `a + (b)` and `a + b` are shape. The node kinds still differ for an
//! object literal and a block, an operator, a name, or a type.
//!
//! The parser comes from `tsrs_parser`. This crate must not depend on the
//! expander's own printer for the comparison: the printer is what the two
//! sides are being compared about.

use std::fmt::Write as _;

use tsrs_ast::{ExternalModuleIndicatorOptions, Kind as SyntaxKind, Node, SourceFileParseOptions};
use tsrs_core::arena::Region;
use tsrs_core::tspath::Path;
use tsrs_core::{P, ScriptKind};
use tsrs_parser::parse_source_file_owned;

/// A stable dump of the syntax tree. Two texts with the same dump are the
/// same program up to formatting. `None` when the text does not parse.
#[expect(
    clippy::unnecessary_wraps,
    reason = "public API signature re-exported by ts_ext_syntax::structure"
)]
#[must_use]
pub fn dump(text: &str, tsx: bool) -> Option<String> {
    let region = Region::new(64 << 10);
    let _guard = region.enter();
    let file_name = if tsx { "/shape.tsx" } else { "/shape.ts" };
    let opts = SourceFileParseOptions {
        path: Path::from(file_name.to_owned()),
        file_name: file_name.to_owned(),
        external_module_indicator_options: ExternalModuleIndicatorOptions::default(),
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

fn has_node_text(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Identifier
            | SyntaxKind::PrivateIdentifier
            | SyntaxKind::StringLiteral
            | SyntaxKind::NumericLiteral
            | SyntaxKind::BigIntLiteral
            | SyntaxKind::MetaProperty
            | SyntaxKind::NoSubstitutionTemplateLiteral
            | SyntaxKind::TemplateHead
            | SyntaxKind::TemplateMiddle
            | SyntaxKind::TemplateTail
            | SyntaxKind::JsxNamespacedName
            | SyntaxKind::RegularExpressionLiteral
            | SyntaxKind::JSDocText
            | SyntaxKind::JSDocLink
            | SyntaxKind::JSDocLinkCode
            | SyntaxKind::JSDocLinkPlain
    )
}

fn write_node(node: P<Node>, depth: usize, out: &mut String) {
    let kind = node.kind();
    // A pair of parentheses is not part of the program. Skip it and keep
    // the expression it wraps.
    if kind == SyntaxKind::ParenthesizedExpression || kind == SyntaxKind::ParenthesizedType {
        node.for_each_child_static(&mut |child| {
            write_node(child, depth, out);
            false
        });
        return;
    }
    out.push_str(&"  ".repeat(depth));
    let _ = write!(out, "{kind:?}");
    let is_leaf = !node.for_each_child_static(&mut |_| true);
    if is_leaf && has_node_text(kind) {
        let text = node.text();
        if !text.is_empty() {
            out.push(' ');
            out.push_str(text);
        }
    }
    out.push('\n');
    node.for_each_child_static(&mut |child| {
        write_node(child, depth + 1, out);
        false
    });
}
