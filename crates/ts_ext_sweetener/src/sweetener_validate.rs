//! Definition checks upstream's macro-language parser, pattern compiler and
//! template compiler make before any macro runs: malformed captures and
//! repetitions (SWR2002), unknown categories (SWR2003), duplicate captures
//! (SWR2004), alternatives that bind different captures (SWR2005),
//! repetitions that can match nothing (SWR2006), unresolved and
//! left-recursive syntax classes (SWR2008, SWR2009), refinements of unknown
//! captures (SWR2010), template references (SWR2011 to SWR2015), operator
//! configuration (SWR4006), duplicate definitions (SWR4010) and definitions
//! written inside a block (SWR4016).
//!
//! The walk is over the file's token tree, so every diagnostic has the span
//! of the token upstream names.

use super::sweetener_reader::{Delimiter, Span, Syntax};
use crate::{Diagnostic, Severity};
use tsrs_core::collections::{OrderedMap, OrderedSet};

/// Builtin syntax classes (upstream's builtin registry).
const BUILTIN_CLASSES: &[&str] = &[
    "item",
    "stmt",
    "expr",
    "type",
    "binding",
    "classElement",
    "typeMember",
    "jsxChild",
    "token",
    "tt",
    "ident",
];

/// Categories a definition may declare.
const CATEGORIES: &[&str] = &[
    "item",
    "stmt",
    "expr",
    "type",
    "binding",
    "classElement",
    "typeMember",
    "jsxChild",
    "token",
    "tt",
];

/// A class visible to this file: its name and declared fields.
pub struct KnownClass {
    pub name: String,
    pub fields: Vec<String>,
}

fn span(node: &Syntax) -> Span {
    match node {
        Syntax::Token { span, .. }
        | Syntax::Comment { span, .. }
        | Syntax::String { span, .. }
        | Syntax::Operator { span, .. }
        | Syntax::Group { span, .. } => *span,
    }
}

fn raw(node: Option<&Syntax>) -> Option<&str> {
    match node? {
        Syntax::Token { raw, .. } | Syntax::Operator { raw, .. } => Some(raw.as_str()),
        _ => None,
    }
}

fn is(node: Option<&Syntax>, word: &str) -> bool {
    raw(node) == Some(word)
}

fn group(node: Option<&Syntax>, delimiter: Delimiter) -> Option<&[Syntax]> {
    match node? {
        Syntax::Group { delimiter: found, children, .. } if *found == delimiter => Some(children),
        _ => None,
    }
}

fn error(message: String, at: Span) -> Diagnostic {
    Diagnostic {
        message,
        start: at.start,
        end: at.end,
        severity: Severity::Error,
        related: Vec::new(),
    }
}

fn is_word(raw: &str) -> bool {
    !raw.is_empty() && raw.chars().all(|ch| ch.is_alphanumeric() || ch == '_' || ch == '$')
}

/// One capture of a rule's pattern.
#[derive(Clone)]
struct Capture {
    depth: usize,
    /// The repetitions it is nested in, outermost first.
    groups: Vec<usize>,
    class: String,
}

struct Validator<'a> {
    classes: &'a [KnownClass],
    /// Classes this file defines that failed to compile (SWR2009).
    invalid_classes: OrderedSet<String>,
    out: Vec<Diagnostic>,
    next_group: usize,
    /// Definitions that failed to compile; upstream does not bind them.
    invalid: OrderedSet<String>,
    /// Names a binding literal's path may start from: globals and what the
    /// file declares or imports.
    resolvable: OrderedSet<String>,
}

/// Check every definition in the file's top level, and report definitions
/// written below it.
pub fn validate(nodes: &[Syntax], classes: &[KnownClass]) -> (Vec<Diagnostic>, OrderedSet<String>) {
    let mut validator = Validator {
        classes,
        invalid_classes: OrderedSet::default(),
        out: Vec::new(),
        next_group: 0,
        invalid: OrderedSet::default(),
        resolvable: declared_names(nodes),
    };
    // Classes first: a class failing SWR2009 is unresolved where it is used.
    for (index, _) in nodes.iter().enumerate() {
        if is(nodes.get(index), "syntax") && is(nodes.get(index + 1), "class") {
            validator.class_definition(nodes, index);
        }
    }
    let mut seen: OrderedMap<String, (String, bool)> = OrderedMap::default();
    let mut definition_bodies = Vec::new();
    let mut index = 0;
    while index < nodes.len() {
        let exported = (index > 0 && is(nodes.get(index - 1), "export"))
            || (index > 1 && is(nodes.get(index - 1), "rec") && is(nodes.get(index - 2), "export"));
        let after_for = index > 0 && is(nodes.get(index - 1), "for");
        let at_definition = (!after_for
            && is(nodes.get(index), "syntax")
            && raw(nodes.get(index + 1)).is_some_and(is_word)
            && !is(nodes.get(index + 1), "class")
            && !is(nodes.get(index + 1), "parameter"))
            || (is(nodes.get(index), "operator")
                && group(nodes.get(index + 1), Delimiter::Paren).is_some());
        let rec = is(nodes.get(index), "rec") && is(nodes.get(index + 1), "syntax");
        if rec {
            index += 1;
            continue;
        }
        if at_definition {
            validator.definition(nodes, index, exported, &mut seen);
            let mut body = index + 1;
            while body < nodes.len()
                && group(nodes.get(body), Delimiter::Brace).is_none()
                && !is(nodes.get(body), ";")
            {
                body += 1;
            }
            definition_bodies.push(body);
        }
        // A definition's own body is read by `definition`; any other
        // block is where a definition would be skipped.
        if let Some(children) = group(nodes.get(index), Delimiter::Brace)
            && !definition_bodies.contains(&index)
        {
            validator.nested_definitions(children);
        }
        index += 1;
    }
    (validator.out, validator.invalid)
}

impl Validator<'_> {
    fn class_known(&self, name: &str) -> bool {
        BUILTIN_CLASSES.contains(&name)
            || (self.classes.iter().any(|class| class.name == name)
                && !self.invalid_classes.contains(name))
    }

    /// SWR4016: a definition inside a block is not read.
    fn nested_definitions(&mut self, nodes: &[Syntax]) {
        for (index, node) in nodes.iter().enumerate() {
            let starts = index == 0
                || matches!(raw(nodes.get(index - 1)), Some(";" | "export"))
                || matches!(
                    nodes.get(index - 1),
                    Some(Syntax::Group { delimiter: Delimiter::Brace, .. })
                );
            if starts
                && is(Some(node), "syntax")
                && let Some(name) = raw(nodes.get(index + 1))
                && is_word(name)
                && name != "class"
                && is(nodes.get(index + 2), ":")
            {
                self.out.push(error(
                    format!(
                        "SWR4016: Macro definitions are read at module level, so {name} written inside a block was not processed. Move it to a module and import it for syntax."
                    ),
                    span(node),
                ));
            }
            if let Syntax::Group { children, .. } = node {
                self.nested_definitions(children);
            }
        }
    }

    fn class_definition(&mut self, nodes: &[Syntax], at: usize) {
        let Some(name) = raw(nodes.get(at + 2)).map(str::to_owned) else {
            return;
        };
        let Some(body) = group(nodes.get(at + 3), Delimiter::Brace) else {
            return;
        };
        let mut index = 0;
        let mut left_recursive = false;
        while index < body.len() {
            if is(body.get(index), "rule")
                && let Some(pattern) = group(body.get(index + 1), Delimiter::Brace)
            {
                // `rule { $l:Self ... }`: the class calls itself before
                // reading anything.
                let first = pattern.iter().position(|node| !matches!(node, Syntax::Comment { .. }));
                if let Some(first) = first
                    && is(pattern.get(first), "$")
                    && is(pattern.get(first + 2), ":")
                    && is(pattern.get(first + 3), &name)
                {
                    left_recursive = true;
                }
            }
            index += 1;
        }
        if left_recursive {
            self.invalid_classes.insert(name.clone());
            self.out.push(error(
                format!("SWR2009: Syntax class {name} has an unguarded recursive dependency."),
                span(&nodes[at]),
            ));
        }
    }

    fn definition(
        &mut self,
        nodes: &[Syntax],
        at: usize,
        exported: bool,
        seen: &mut OrderedMap<String, (String, bool)>,
    ) {
        let operator = is(nodes.get(at), "operator");
        let name = if operator {
            let Some(spelling) = group(nodes.get(at + 1), Delimiter::Paren) else {
                return;
            };
            spelling.iter().filter_map(|node| raw(Some(node))).collect::<String>()
        } else {
            match raw(nodes.get(at + 1)) {
                Some(name) => name.to_owned(),
                None => return,
            }
        };
        // `:category`
        let category_at = at + 3;
        if !is(nodes.get(at + 2), ":") {
            self.out.push(error(
                "SWR2001: Expected macro name and category.".to_owned(),
                span(nodes.get(at + 1).unwrap_or(&nodes[at])),
            ));
            return;
        }
        let Some(category) = raw(nodes.get(category_at)).map(str::to_owned) else {
            return;
        };
        if !CATEGORIES.contains(&category.as_str()) {
            self.out.push(error(
                format!("SWR2003: Unknown syntax category {category}."),
                span(&nodes[category_at]),
            ));
        }
        // `shadows core` may sit between the category and the body.
        let mut body_at = category_at + 1;
        while body_at < nodes.len() && group(nodes.get(body_at), Delimiter::Brace).is_none() {
            if is(nodes.get(body_at), ";") {
                break;
            }
            body_at += 1;
        }
        let Some(body) = group(nodes.get(body_at), Delimiter::Brace) else {
            self.out.push(error(
                "SWR2001: Expected macro definition body.".to_owned(),
                span(&nodes[category_at]),
            ));
            return;
        };
        let keyword = span(&nodes[at]);
        if let Some((previous, previous_exported)) = seen.get(&name) {
            let detail = if *previous == category {
                Some(format!("another {category} definition"))
            } else if exported && *previous_exported {
                Some(format!(
                    "an exported {previous} definition, and a module's export list records one category per name"
                ))
            } else {
                None
            };
            if let Some(detail) = detail {
                self.out.push(error(
                    format!("SWR4010: Macro {name} is already defined in this module: {detail}."),
                    keyword,
                ));
            }
        } else {
            seen.insert(name.clone(), (category.clone(), exported));
        }
        let before = self.out.len();
        if operator {
            self.operator_configuration(body, &name, keyword);
        }
        let mut index = 0;
        while index < body.len() {
            // `literal globalThis.NaN as NaN`: the path must resolve.
            if is(body.get(index), "literal")
                && let Some(root) = raw(body.get(index + 1))
                && !self.resolvable.contains(root)
            {
                let mut path = String::new();
                let mut at = index + 1;
                while at < body.len() && !is(body.get(at), "as") && !is(body.get(at), ";") {
                    path.push_str(raw(body.get(at)).unwrap_or(""));
                    at += 1;
                }
                let alias = raw(body.get(at + 1)).unwrap_or("unknown");
                self.out.push(error(
                    format!("SWR4009: Binding literal {alias} cannot resolve {path}."),
                    span(&body[index]),
                ));
            }
            if is(body.get(index), "rule") {
                match group(body.get(index + 1), Delimiter::Brace) {
                    Some(pattern) => self.rule(body, index, pattern),
                    None => self.out.push(error(
                        "SWR2001: Expected rule pattern group.".to_owned(),
                        span(&body[index]),
                    )),
                }
            }
            index += 1;
        }
        // A malformed capture still reads as literal syntax upstream; every
        // other problem leaves the definition unbound.
        if self.out[before..].iter().any(|diagnostic| !diagnostic.message.starts_with("SWR2002")) {
            self.invalid.insert(name);
        }
    }

    /// SWR4006, in upstream's order of checks.
    fn operator_configuration(&mut self, body: &[Syntax], name: &str, keyword: Span) {
        let property = |word: &str| {
            body.iter()
                .enumerate()
                .find(|(index, node)| {
                    let previous = body[..*index]
                        .iter()
                        .rev()
                        .find(|node| !matches!(node, Syntax::Comment { .. }));
                    is(Some(node), word)
                        && (previous.is_none()
                            || is(previous, ";")
                            || matches!(previous, Some(Syntax::Group { .. })))
                })
                .and_then(|(index, _)| raw(body.get(index + 1)))
        };
        let fixity = property("fixity");
        let associativity = property("associativity").or(match fixity {
            Some("prefix" | "postfix") => Some("none"),
            _ => None,
        });
        let precedence = property("precedence").and_then(|raw| raw.parse::<u64>().ok());
        let operand = property("operand");
        let problem = if !matches!(fixity, Some("prefix" | "infix" | "postfix")) {
            Some("fixity must be prefix, infix, or postfix")
        } else if !matches!(associativity, Some("left" | "right" | "none")) {
            Some("associativity must be left, right, or none")
        } else if fixity != Some("infix") && associativity != Some("none") {
            Some("prefix and postfix operators must be nonassociative")
        } else if !precedence.is_some_and(|value| value >= 1 && value <= (1 << 53) - 1) {
            Some("precedence must be a positive safe integer")
        } else if operand.is_some_and(|operand| operand != "arrow") {
            Some("operand must be arrow")
        } else if operand.is_some() && fixity != Some("infix") {
            Some("only an infix operator takes operand arrow")
        } else {
            None
        };
        if let Some(problem) = problem {
            self.out.push(error(format!("SWR4006: Invalid operator {name}: {problem}."), keyword));
        }
    }

    fn rule(&mut self, body: &[Syntax], rule_at: usize, pattern: &[Syntax]) {
        let mut captures: OrderedMap<String, Capture> = OrderedMap::default();
        let mut first_seen: OrderedMap<String, Span> = OrderedMap::default();
        self.pattern(pattern, 0, &mut Vec::new(), &mut captures, &mut first_seen);
        // Clauses up to `=>`, then the template.
        let mut index = rule_at + 2;
        while index < body.len() && !is_arrow(body, index) {
            if is(body.get(index), "rule") {
                return;
            }
            if is(body.get(index), "refine")
                && is(body.get(index + 1), "$")
                && let Some(target) = raw(body.get(index + 2))
                && !captures.contains_key(target)
            {
                self.out.push(error(
                    format!("SWR2010: Invalid refinement target or predicate for ${target}."),
                    span(&body[index]),
                ));
            }
            index += 1;
        }
        if index >= body.len() {
            return;
        }
        let template_at = if is(body.get(index), "=") { index + 2 } else { index + 1 };
        if let Some(template) = group(body.get(template_at), Delimiter::Brace) {
            let mut locals = OrderedSet::default();
            template_locals(template, &mut locals);
            self.template(template, 0, &captures, &locals);
        }
    }

    /// Returns whether the sequence can match without consuming syntax.
    fn pattern(
        &mut self,
        nodes: &[Syntax],
        depth: usize,
        stack: &mut Vec<usize>,
        captures: &mut OrderedMap<String, Capture>,
        first_seen: &mut OrderedMap<String, Span>,
    ) -> bool {
        // Alternatives: a `|` with space on both sides.
        let mut cuts = Vec::new();
        for index in 1..nodes.len().saturating_sub(1) {
            if is(nodes.get(index), "|")
                && span(&nodes[index]).start > span(&nodes[index - 1]).end
                && span(&nodes[index + 1]).start > span(&nodes[index]).end
            {
                cuts.push(index);
            }
        }
        if !cuts.is_empty() {
            let mut bounds = vec![0];
            for cut in &cuts {
                bounds.push(cut + 1);
            }
            let mut sets: Vec<Vec<String>> = Vec::new();
            let mut nullable = false;
            let outer = captures.clone();
            let outer_seen = first_seen.clone();
            for (position, start) in bounds.iter().enumerate() {
                let end = cuts.get(position).copied().unwrap_or(nodes.len());
                let mut local = outer.clone();
                let mut seen = outer_seen.clone();
                let before: OrderedSet<String> = local.keys().cloned().collect();
                nullable |= self.pattern(&nodes[*start..end], depth, stack, &mut local, &mut seen);
                for (name, at) in seen {
                    first_seen.entry(name).or_insert(at);
                }
                let mut bound: Vec<String> =
                    local.keys().filter(|name| !before.contains(*name)).cloned().collect();
                bound.sort();
                for name in &bound {
                    if let Some(capture) = local.get(name) {
                        captures.insert(name.clone(), capture.clone());
                    }
                }
                sets.push(bound);
            }
            if sets.windows(2).any(|pair| pair[0] != pair[1]) {
                let differing = sets
                    .iter()
                    .flatten()
                    .find(|name| !sets.iter().all(|set| set.contains(name)))
                    .cloned()
                    .unwrap_or_default();
                self.out.push(error(
                    format!("SWR2005: Alternative has an incompatible shape for ${differing}."),
                    span(&nodes[0]),
                ));
            }
            return nullable;
        }
        let mut nullable = true;
        let mut index = 0;
        while index < nodes.len() {
            let node = &nodes[index];
            if matches!(node, Syntax::Comment { .. }) {
                index += 1;
                continue;
            }
            if is(Some(node), "$") {
                // `$$name` is the literal `$name`.
                if is(nodes.get(index + 1), "$") {
                    nullable = false;
                    index += 3;
                    continue;
                }
                if let Some(children) = group(nodes.get(index + 1), Delimiter::Paren) {
                    let mut quantifier = index + 2;
                    if !matches!(raw(nodes.get(quantifier)), Some("*" | "+" | "?"))
                        && nodes.get(quantifier).is_some()
                    {
                        quantifier += 1;
                    }
                    let Some(kind @ ("*" | "+" | "?")) = raw(nodes.get(quantifier)) else {
                        self.out.push(error(
                            "SWR2002: Malformed pattern: repetition requires *, +, or ?."
                                .to_owned(),
                            span(node),
                        ));
                        nullable = false;
                        index += 1;
                        continue;
                    };
                    let id = self.next_group;
                    self.next_group += 1;
                    stack.push(id);
                    let body_nullable =
                        self.pattern(children, depth + 1, stack, captures, first_seen);
                    stack.pop();
                    if body_nullable && kind != "?" {
                        self.out.push(error(
                            "SWR2006: Repeated pattern can match without consuming syntax."
                                .to_owned(),
                            span(node),
                        ));
                    }
                    if kind == "+" && !body_nullable {
                        nullable = false;
                    }
                    index = quantifier + 1;
                    continue;
                }
                let named = raw(nodes.get(index + 1)).filter(|name| is_word(name));
                let class = raw(nodes.get(index + 3)).filter(|class| is_word(class));
                let (Some(name), true, Some(class)) = (named, is(nodes.get(index + 2), ":"), class)
                else {
                    self.out.push(error(
                        "SWR2002: Malformed pattern: capture requires $name:class.".to_owned(),
                        span(node),
                    ));
                    nullable = false;
                    index += 1;
                    continue;
                };
                if let Some(first) = first_seen.get(name) {
                    if captures.contains_key(name) {
                        self.out.push(error(
                            format!("SWR2004: Capture ${name} is bound more than once."),
                            *first,
                        ));
                    }
                } else {
                    first_seen.insert(name.to_owned(), span(node));
                }
                if !self.class_known(class) {
                    self.out.push(error(
                        format!("SWR2008: Unresolved syntax class {class}."),
                        span(node),
                    ));
                }
                captures.insert(
                    name.to_owned(),
                    Capture { depth, groups: stack.clone(), class: class.to_owned() },
                );
                nullable = false;
                index += 4;
                continue;
            }
            if let Syntax::Group { children, .. } = node {
                self.pattern(children, depth, stack, captures, first_seen);
            }
            nullable = false;
            index += 1;
        }
        nullable
    }

    fn template(
        &mut self,
        nodes: &[Syntax],
        depth: usize,
        captures: &OrderedMap<String, Capture>,
        locals: &OrderedSet<String>,
    ) {
        let mut index = 0;
        while index < nodes.len() {
            let node = &nodes[index];
            if is(Some(node), "$") {
                if is(nodes.get(index + 1), "$") {
                    index += 3;
                    continue;
                }
                if let Some(children) = group(nodes.get(index + 1), Delimiter::Paren) {
                    let mut quantifier = index + 2;
                    if !matches!(raw(nodes.get(quantifier)), Some("*" | "+" | "?")) {
                        quantifier += 1;
                    }
                    if matches!(raw(nodes.get(quantifier)), Some("*" | "+" | "?")) {
                        self.repetition(node, children, depth + 1, captures, locals);
                        self.template(children, depth + 1, captures, locals);
                        index = quantifier + 1;
                        continue;
                    }
                    index += 1;
                    continue;
                }
                if let Some(name) = raw(nodes.get(index + 1)).filter(|name| is_word(name)) {
                    if locals.contains(name) {
                        index += 2;
                        continue;
                    }
                    match captures.get(name) {
                        None => self.out.push(error(
                            format!("SWR2011: Template refers to unknown capture ${name}."),
                            span(node),
                        )),
                        Some(capture) => {
                            if is(nodes.get(index + 2), ".")
                                && let Some(field) = raw(nodes.get(index + 3))
                                && let Some(class) =
                                    self.classes.iter().find(|class| class.name == capture.class)
                                && !class.fields.iter().any(|known| known == field)
                            {
                                self.out.push(error(
                                    format!("SWR2012: Template capture has no field {field}."),
                                    span(&nodes[index + 3]),
                                ));
                            }
                            if capture.depth > depth {
                                self.out.push(error(
                                    format!(
                                        "SWR2013: Capture ${name} requires template depth {}.",
                                        capture.depth
                                    ),
                                    span(node),
                                ));
                            }
                        }
                    }
                    index += 2;
                    continue;
                }
            }
            // `#fold($items, ...)` walks a sequence: its argument is read one
            // repetition deeper.
            let sequence_op = |name: Option<&str>| {
                matches!(name, Some("fold" | "count" | "join" | "index" | "if"))
            };
            // `#count($xs)`, `#join`, `#index` and an `#if` predicate read a
            // capture at any depth.
            let sequence_args = if is(Some(node), "#") && sequence_op(raw(nodes.get(index + 1))) {
                Some(index + 2)
            } else if raw(Some(node)).is_some_and(|op| sequence_op(op.strip_prefix('#'))) {
                Some(index + 1)
            } else {
                None
            };
            // `#text($item)` inside `#fold`: a fold binder is not a capture
            // `#text` can quote (upstream SWR2011, then SWR2017).
            let text_args = if is(Some(node), "#") && is(nodes.get(index + 1), "text") {
                Some(index + 2)
            } else if is(Some(node), "#text") {
                Some(index + 1)
            } else {
                None
            };
            if let Some(args) = text_args
                && let Some([dollar, name]) = group(nodes.get(args), Delimiter::Paren)
                && is(Some(dollar), "$")
                && raw(Some(name)).is_some_and(|name| locals.contains(name))
            {
                let name = raw(Some(name)).unwrap_or_default();
                self.out.push(error(
                    format!("SWR2011: Template refers to unknown capture ${name}."),
                    span(dollar),
                ));
                self.out.push(error(
                    "SWR2017: Invalid argument for template operation #text.".to_owned(),
                    span(node),
                ));
                index = args + 1;
                continue;
            }
            if let Some(args) = sequence_args
                && let Some(children) = group(nodes.get(args), Delimiter::Paren)
            {
                self.template(children, usize::MAX / 2, captures, locals);
                index = args + 1;
                continue;
            }
            // `#syntax { ... }` writes definitions of its own; their
            // captures are theirs.
            if (is(Some(node), "#") && is(nodes.get(index + 1), "syntax"))
                || is(Some(node), "#syntax")
            {
                index += if is(Some(node), "#") { 3 } else { 2 };
                continue;
            }
            if let Syntax::Group { children, .. } = node {
                self.template(children, depth, captures, locals);
            }
            index += 1;
        }
    }

    /// SWR2014 / SWR2015: what drives a template repetition at `depth`.
    fn repetition(
        &mut self,
        dollar: &Syntax,
        body: &[Syntax],
        depth: usize,
        captures: &OrderedMap<String, Capture>,
        locals: &OrderedSet<String>,
    ) {
        let mut references = Vec::new();
        let mut field_access = false;
        references_in(body, &mut references, &mut field_access);
        if field_access {
            return;
        }
        let drivers: Vec<&Capture> = references
            .iter()
            .filter(|name| !locals.contains(*name))
            .filter_map(|name| captures.get(name))
            .filter(|capture| capture.depth >= depth)
            .collect();
        if drivers.is_empty() {
            // An unknown reference is reported on its own.
            if references.iter().any(|name| !locals.contains(name) && !captures.contains_key(name))
            {
                return;
            }
            self.out.push(error(
                format!("SWR2014: Template repetition at depth {depth} has no driving capture."),
                span(dollar),
            ));
            return;
        }
        let groups: OrderedSet<usize> =
            drivers.iter().filter_map(|capture| capture.groups.get(depth - 1).copied()).collect();
        if groups.len() > 1 {
            self.out.push(error(
                format!(
                    "SWR2015: Template repetition at depth {depth} has incompatible driving captures."
                ),
                span(dollar),
            ));
        }
    }
}

/// The global object's names and every name the file's top level declares
/// or imports.
fn declared_names(nodes: &[Syntax]) -> OrderedSet<String> {
    let mut out: OrderedSet<String> =
        ["globalThis", "window", "self", "global"].into_iter().map(str::to_owned).collect();
    for (index, node) in nodes.iter().enumerate() {
        if matches!(
            raw(Some(node)),
            Some("const" | "let" | "var" | "function" | "class" | "enum" | "namespace")
        ) && let Some(name) = raw(nodes.get(index + 1)).filter(|name| is_word(name))
        {
            out.insert(name.to_owned());
        }
        if is(Some(node), "import")
            && let Some(names) = group(nodes.get(index + 1), Delimiter::Brace)
        {
            for name in names {
                if let Some(word) = raw(Some(name)).filter(|word| is_word(word)) {
                    out.insert(word.to_owned());
                }
            }
        }
        if is(Some(node), "import")
            && let Some(name) = raw(nodes.get(index + 1)).filter(|name| is_word(name))
        {
            out.insert(name.to_owned());
        }
        if is(Some(node), "as")
            && let Some(name) = raw(nodes.get(index + 1)).filter(|name| is_word(name))
        {
            out.insert(name.to_owned());
        }
    }
    out
}

/// Whether the import at `index` that names `for syntax` has upstream's
/// shape: named bindings, a module string, `for syntax`, and an optional
/// `shadows core`, ending the statement.
pub(crate) fn syntax_import_well_formed(source: &str, nodes: &[Syntax], index: usize) -> bool {
    let shaped = group(nodes.get(index + 1), Delimiter::Brace).is_some()
        && is(nodes.get(index + 2), "from")
        && matches!(nodes.get(index + 3), Some(Syntax::String { .. }))
        && is(nodes.get(index + 4), "for")
        && is(nodes.get(index + 5), "syntax");
    if !shaped {
        return false;
    }
    let mut end = index + 6;
    if is(nodes.get(end), "shadows") && is(nodes.get(end + 1), "core") {
        end += 2;
    }
    match nodes.get(end) {
        None => true,
        Some(next) if is(Some(next), ";") => true,
        Some(next) => {
            let gap = &source[span(&nodes[end - 1]).end as usize..span(next).start as usize];
            gap.contains('\n')
        }
    }
}

/// SWR2019 for every `for syntax` import that is not well formed.
pub(crate) fn malformed_syntax_imports(source: &str, nodes: &[Syntax]) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for index in 0..nodes.len() {
        if !is(nodes.get(index), "import") {
            continue;
        }
        let mut cursor = index + 1;
        let mut names_syntax = false;
        while cursor < nodes.len() && !is(nodes.get(cursor), ";") && cursor < index + 10 {
            if is(nodes.get(cursor), "for") && is(nodes.get(cursor + 1), "syntax") {
                names_syntax = true;
                break;
            }
            if is(nodes.get(cursor), "import") {
                break;
            }
            cursor += 1;
        }
        if names_syntax && !syntax_import_well_formed(source, nodes, index) {
            out.push(error(
                "SWR2019: Malformed compile-time syntax import: expected named bindings, module string, `for syntax`, and optional `shadows core`.".to_owned(),
                span(&nodes[index]),
            ));
        }
    }
    out
}

/// Each definition of this file whose template parameterizes a name that is
/// not a syntax parameter in scope here (SWR4019, reported where the macro
/// is used): the macro and the name.
pub(crate) fn unknown_parameterizations(
    nodes: &[Syntax],
    in_scope: &OrderedSet<String>,
) -> OrderedMap<String, String> {
    let mut out = OrderedMap::default();
    for index in 0..nodes.len() {
        let name = if is(nodes.get(index), "syntax")
            && !is(nodes.get(index + 1), "class")
            && !is(nodes.get(index + 1), "parameter")
            && !(index > 0 && is(nodes.get(index - 1), "for"))
        {
            raw(nodes.get(index + 1)).map(str::to_owned)
        } else if is(nodes.get(index), "operator") {
            group(nodes.get(index + 1), Delimiter::Paren)
                .map(|spelling| spelling.iter().filter_map(|node| raw(Some(node))).collect())
        } else {
            None
        };
        let Some(name) = name else {
            continue;
        };
        let Some(body) =
            nodes[index..].iter().take(8).find_map(|node| group(Some(node), Delimiter::Brace))
        else {
            continue;
        };
        if let Some(param) = first_unknown_parameter(body, in_scope) {
            out.entry(name).or_insert(param);
        }
    }
    out
}

fn first_unknown_parameter(nodes: &[Syntax], in_scope: &OrderedSet<String>) -> Option<String> {
    for (index, node) in nodes.iter().enumerate() {
        let args_at = if is(Some(node), "#") && is(nodes.get(index + 1), "parameterize") {
            Some(index + 2)
        } else if is(Some(node), "#parameterize") {
            Some(index + 1)
        } else {
            None
        };
        // Without its `{ body }` the operation is malformed (SWR2016), and
        // names nothing.
        let args = args_at
            .filter(|at| group(nodes.get(at + 1), Delimiter::Brace).is_some())
            .and_then(|at| group(nodes.get(at), Delimiter::Paren));
        if let Some(args) = args {
            let mut param = String::new();
            for arg in args {
                match raw(Some(arg)) {
                    Some("=") => break,
                    Some("required") if param.is_empty() => {}
                    Some(word) => param.push_str(word),
                    None => {
                        if let Syntax::Group { children, .. } = arg {
                            for child in children {
                                param.push_str(raw(Some(child)).unwrap_or(""));
                            }
                        }
                    }
                }
            }
            if !param.is_empty() && !in_scope.contains(&param) {
                return Some(param);
            }
        }
        if let Syntax::Group { children, .. } = node
            && let Some(param) = first_unknown_parameter(children, in_scope)
        {
            return Some(param);
        }
    }
    None
}

fn is_arrow(nodes: &[Syntax], index: usize) -> bool {
    is(nodes.get(index), "=>") || (is(nodes.get(index), "=") && is(nodes.get(index + 1), ">"))
}

/// `$name` references anywhere in `nodes`.
fn references_in(nodes: &[Syntax], out: &mut Vec<String>, field_access: &mut bool) {
    for (index, node) in nodes.iter().enumerate() {
        if is(Some(node), "$")
            && !is(nodes.get(index + 1), "$")
            && (index == 0 || !is(nodes.get(index - 1), "$"))
            && let Some(name) = raw(nodes.get(index + 1)).filter(|name| is_word(name))
        {
            out.push(name.to_owned());
            if is(nodes.get(index + 2), ".") {
                *field_access = true;
            }
        }
        if let Syntax::Group { children, .. } = node {
            references_in(children, out, field_access);
        }
    }
}

/// Names a template binds for itself: `#fold`'s `($acc, $item) =>`.
fn template_locals(nodes: &[Syntax], out: &mut OrderedSet<String>) {
    for (index, node) in nodes.iter().enumerate() {
        if let Some(children) = group(Some(node), Delimiter::Paren)
            && is_arrow(nodes, index + 1)
        {
            for (at, child) in children.iter().enumerate() {
                if is(Some(child), "$")
                    && let Some(name) = raw(children.get(at + 1))
                {
                    out.insert(name.to_owned());
                }
            }
        }
        if let Syntax::Group { children, .. } = node {
            template_locals(children, out);
        }
    }
}
