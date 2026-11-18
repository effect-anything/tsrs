//! Operator types for check and LSP.
//!
//! The author's `+` is not rewritten before check. When either operand's
//! type has `Symbol.operatorPlus` (and the same for `- * / % **`), the
//! expression type is that call signature's return type. The left operand
//! is tried first. `Symbol.deferOperation` tries the right operand. A pair
//! with no signature falls through to TypeScript's own operator.
//!
//! This is tsover's `checkBinaryLikeExpressionWorker` hook. Emit still
//! prints `__tsover_*`.

use crate::*;

/// The binary expression to show when hover is on an overloaded operator.
///
/// Stock quickinfo ignores a `+` token. In a `"use tsover"` or `"use gpu"`
/// scope the expression type is the overload result, so hover should name it.
#[must_use]
pub fn hover_expression(node: P<Node>) -> Option<P<Node>> {
    let expr = if node.kind() == Kind::BinaryExpression {
        node
    } else {
        let parent = node.parent()?;
        if parent.kind() == Kind::BinaryExpression
            && parent.as_binary_expression().operator_token == node
        {
            parent
        } else {
            return None;
        }
    };
    in_overload_scope(expr).then_some(expr)
}

/// Checks whether a prologue directive expression names `use <name>`.
#[must_use]
pub fn is_named_directive(node: P<Node>, name: &str) -> bool {
    let text = node.text();
    let unquoted = text
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| text.strip_prefix('\'')?.strip_suffix('\''))
        .unwrap_or(text);
    unquoted.strip_prefix("use ") == Some(name)
}

/// Checks overloaded binary operators in `"use tsover"` and `"use gpu"` scopes.
///
/// `+` stays in the tree. A `Symbol.operatorPlus` (and the other five) on
/// either operand supplies the type, and a compound assignment is checked
/// against it. Emit turns it into `__tsover_*` later.
pub fn check_binary(
    checker: &mut Checker,
    left: P<Node>,
    operator: Kind,
    right: P<Node>,
    left_type: P<Type>,
    right_type: P<Type>,
) -> Option<P<Type>> {
    let overloaded = binary_type(checker, left, operator, right, left_type, right_type)?;
    if ast::is_compound_assignment(operator) {
        checker.check_assignment_operator(left, operator, right, left_type, overloaded);
    }
    Some(overloaded)
}

/// The type of `left operator right`, when an overload symbol applies.
pub fn binary_type(
    checker: &mut Checker,
    left: P<Node>,
    operator: Kind,
    _right: P<Node>,
    left_type: P<Type>,
    right_type: P<Type>,
) -> Option<P<Type>> {
    let symbol = symbol_name(operator)?;
    if !in_overload_scope(left) {
        return None;
    }
    let property = checker.get_property_name_for_known_symbol_name(symbol);
    let defer = defer_operation_type(checker);
    let pairs = pairs(checker, left_type, right_type);
    let mut results = Vec::with_capacity(pairs.len());
    for (lhs, rhs) in pairs {
        let mut result = signature_return(checker, lhs, &property, lhs, rhs);
        let left_missing = result.is_none();
        if left_missing || is_defer(checker, result, defer) {
            result = signature_return(checker, rhs, &property, lhs, rhs);
        }
        if is_defer(checker, result, defer) {
            result = None;
        }
        let result = result?;
        results.push(result);
    }
    if results.is_empty() {
        return None;
    }
    if results.len() == 1 {
        return Some(results[0]);
    }
    Some(checker.get_union_type(&results))
}

fn symbol_name(operator: Kind) -> Option<&'static str> {
    let base = match operator {
        Kind::PlusEqualsToken => Kind::PlusToken,
        Kind::MinusEqualsToken => Kind::MinusToken,
        Kind::AsteriskEqualsToken => Kind::AsteriskToken,
        Kind::SlashEqualsToken => Kind::SlashToken,
        Kind::AsteriskAsteriskEqualsToken => Kind::AsteriskAsteriskToken,
        Kind::PercentEqualsToken => Kind::PercentToken,
        other => other,
    };
    match base {
        Kind::PlusToken => Some("operatorPlus"),
        Kind::MinusToken => Some("operatorMinus"),
        Kind::AsteriskToken => Some("operatorStar"),
        Kind::SlashToken => Some("operatorSlash"),
        Kind::AsteriskAsteriskToken => Some("operatorStarStar"),
        Kind::PercentToken => Some("operatorPercent"),
        _ => None,
    }
}

fn in_overload_scope(node: P<Node>) -> bool {
    if let Some(file) = ast::get_source_file_of_node(node) {
        let statements = file.statements.nodes();
        if prologue_has(statements, "use tsover") || prologue_has(statements, "use gpu") {
            return true;
        }
    }
    let mut current = Some(node);
    while let Some(curr) = current {
        if ast::is_function_like_declaration(curr)
            && let Some(body) = curr.body()
            && body.kind() == Kind::Block
        {
            let statements = body.as_block().statements.nodes();
            if prologue_has(statements, "use gpu") || prologue_has(statements, "use tsover") {
                return true;
            }
        }
        current = curr.parent();
    }
    false
}

fn prologue_has(statements: &[P<Node>], text: &str) -> bool {
    let name = text.strip_prefix("use ").unwrap_or(text);
    for &statement in statements {
        if !ast::is_prologue_directive(statement) {
            return false;
        }
        if let Some(expr) = statement.expression()
            && is_named_directive(expr, name)
        {
            return true;
        }
    }
    false
}

fn pairs(checker: &mut Checker, left: P<Type>, right: P<Type>) -> Vec<(P<Type>, P<Type>)> {
    let left_members = members(checker, left);
    let right_members = members(checker, right);
    if left_members.len() == 1 && right_members.len() == 1 {
        return vec![(left_members[0], right_members[0])];
    }
    let mut out = Vec::new();
    for &lhs in &left_members {
        for &rhs in &right_members {
            out.push((lhs, rhs));
        }
    }
    out
}

fn members(checker: &mut Checker, t: P<Type>) -> Vec<P<Type>> {
    let flags = t.flags();
    if flags.intersects(TypeFlags::Union) {
        let types = t.types();
        return types.iter().flat_map(|&part| members(checker, part)).collect();
    }
    if flags.intersects(TypeFlags::TypeVariable)
        && let Some(constraint) = checker.get_base_constraint_of_type(t)
        && constraint != t
    {
        return members(checker, constraint);
    }
    vec![t]
}

fn signature_return(
    checker: &mut Checker,
    operand: P<Type>,
    property: &str,
    left: P<Type>,
    right: P<Type>,
) -> Option<P<Type>> {
    let property_type = checker.get_type_of_property_of_type(operand, property)?;
    let signatures = checker.get_signatures_of_type(property_type, SignatureKind::Call);
    for &signature in signatures {
        let first = checker.get_type_at_position(signature, 0);
        let second = checker.get_type_at_position(signature, 1);
        if checker.is_type_assignable_to(left, first)
            && checker.is_type_assignable_to(right, second)
        {
            return Some(checker.get_return_type_of_signature(signature));
        }
    }
    None
}

fn defer_operation_type(checker: &mut Checker) -> Option<P<Type>> {
    let constructor = checker.get_global_es_symbol_constructor_symbol_or_nil()?;
    let constructor_type = checker.get_type_of_symbol(constructor);
    checker.get_type_of_property_of_type(constructor_type, "deferOperation")
}

fn is_defer(checker: &mut Checker, result: Option<P<Type>>, defer: Option<P<Type>>) -> bool {
    match (result, defer) {
        (Some(result), Some(defer)) => checker.is_type_identical_to(result, defer),
        _ => false,
    }
}
