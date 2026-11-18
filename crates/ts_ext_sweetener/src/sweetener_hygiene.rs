//! Output-based hygiene, upstream's `planHygienicRenames` on printed text.
//!
//! Expansion does not choose names. Every binder a template introduces is
//! printed as a placeholder, one class per (expansion, spelling), and every
//! use the template ties to that binder gets the same placeholder. Once the
//! whole file is printed, TypeScript parses it and one assignment runs:
//!
//! - candidates are the placeholder classes seen in a binder position, in
//!   first-occurrence order;
//! - a class bound by an `export` declaration or named by `export { .. }`
//!   is published and keeps its spelling;
//! - unavailable names are every other identifier the output spells in the
//!   same namespace (value or label), properties excluded;
//! - each class takes its spelling, or `name_N` with the first free `N`,
//!   never a name another class of that namespace already took;
//! - a shorthand `{ x }` prints `{ x: x_N }`, and `import { a }` prints
//!   `import { a as a_N }`; a property position keeps the spelling.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use rustc_hash::{FxHashMap, FxHashSet};
use tsrs_ast::{Kind, ModifierFlags, Node, SourceFileParseOptions};
use tsrs_core::{P, ScriptKind, arena::Region};
use tsrs_parser::parse_source_file_owned;

use crate::Segment;

pub const PREFIX: &str = "__swh";
static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A fresh placeholder for one introduced binder named `name`.
pub fn placeholder(name: &str) -> String {
    // Tests force an expander panic through a `#fresh` hint when enabled on
    // this expansion thread.
    if name == crate::sweetener_expand::PANIC_FRESH_HINT && PANIC_ON_HINT.get() {
        panic!("forced expander panic");
    }
    // Relaxed: monotonic counter only needs uniqueness across calls, not synchronization.
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("{PREFIX}{id}_{name}")
}

thread_local! {
    static PANIC_ON_HINT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static SCOPE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static REMEMBERED: std::cell::RefCell<BTreeMap<String, String>> =
        const { std::cell::RefCell::new(BTreeMap::new()) };
}

pub(crate) fn panic_on_hint() -> bool {
    PANIC_ON_HINT.get()
}

pub(crate) fn with_panic_hint<T>(enabled: bool, f: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            PANIC_ON_HINT.set(self.0);
        }
    }
    let _reset = Reset(PANIC_ON_HINT.replace(enabled));
    f()
}

/// Run `f` as one expansion: `scoped` gives each spelling one placeholder.
pub(crate) fn in_scope<T>(f: impl FnOnce() -> T) -> T {
    let saved = SCOPE.replace(true);
    let out = f();
    SCOPE.set(saved);
    out
}

/// A fresh placeholder id, for an item the expander replays (`items`).
pub(crate) fn fresh_id() -> usize {
    // Relaxed: monotonic counter only needs uniqueness across calls, not synchronization.
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Whether a `scoped` expansion is running.
pub(crate) fn scope_active() -> bool {
    SCOPE.get()
}

/// The spellings handed to later forms so far.
pub(crate) fn remembered() -> BTreeMap<String, String> {
    REMEMBERED.with(|map| map.borrow().clone())
}

pub(crate) fn remembered_is_empty() -> bool {
    REMEMBERED.with(|map| map.borrow().is_empty())
}

pub(crate) fn set_remembered(remembered: BTreeMap<String, String>) {
    REMEMBERED.with(|map| *map.borrow_mut() = remembered);
}

/// Hand `printed` to the form that will print `name`'s declaration.
pub(crate) fn remember(name: &str, printed: &str) {
    REMEMBERED.with(|map| {
        map.borrow_mut().insert(name.to_owned(), printed.to_owned());
    });
}

pub(crate) fn is_placeholder(text: &str) -> bool {
    spelling(text).is_some()
}

/// The spelling a placeholder stands for.
fn spelling(text: &str) -> Option<&str> {
    let rest = text.strip_prefix(PREFIX)?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    rest[digits..].strip_prefix('_').filter(|name| !name.is_empty())
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Space {
    Value,
    Label,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Binder,
    Property,
    Reference,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Occurrence {
    Identifier,
    Shorthand,
    Import,
}

struct Ident {
    text: String,
    start: usize,
    role: Role,
    space: Space,
    kind: Occurrence,
    published: bool,
    export_specifier: bool,
}

fn role_of(ident: P<Node>, parent: P<Node>) -> Role {
    let is_name = || parent.name() == Some(ident);
    match parent.kind() {
        Kind::Parameter => {
            if is_name() {
                Role::Binder
            } else {
                Role::Property
            }
        }
        Kind::VariableDeclaration
        | Kind::FunctionDeclaration
        | Kind::FunctionExpression
        | Kind::ClassDeclaration
        | Kind::ClassExpression
        | Kind::InterfaceDeclaration
        | Kind::TypeAliasDeclaration
        | Kind::EnumDeclaration
        | Kind::ModuleDeclaration
        | Kind::TypeParameter
        | Kind::ImportClause
        | Kind::NamespaceImport
        | Kind::ImportEqualsDeclaration => {
            if is_name() {
                Role::Binder
            } else {
                Role::Reference
            }
        }
        Kind::BindingElement => {
            if parent.as_binding_element().property_name() == Some(ident) {
                Role::Property
            } else if is_name() {
                Role::Binder
            } else {
                Role::Reference
            }
        }
        Kind::ImportSpecifier => {
            if parent.as_import_specifier().property_name() == Some(ident) {
                Role::Property
            } else if is_name() {
                Role::Binder
            } else {
                Role::Reference
            }
        }
        Kind::ExportSpecifier
        | Kind::PropertyAssignment
        | Kind::MethodDeclaration
        | Kind::MethodSignature
        | Kind::PropertyDeclaration
        | Kind::PropertySignature
        | Kind::GetAccessor
        | Kind::SetAccessor
        | Kind::EnumMember
        | Kind::JsxAttribute
        | Kind::NamedTupleMember
        | Kind::PropertyAccessExpression => {
            if is_name() {
                Role::Property
            } else {
                Role::Reference
            }
        }
        Kind::QualifiedName => {
            if parent.as_qualified_name().left() == ident {
                Role::Reference
            } else {
                Role::Property
            }
        }
        Kind::LabeledStatement => {
            if parent.as_labeled_statement().label() == ident {
                Role::Binder
            } else {
                Role::Reference
            }
        }
        Kind::MetaProperty => Role::Property,
        _ => Role::Reference,
    }
}

fn space_of(ident: P<Node>, parent: P<Node>) -> Space {
    match parent.kind() {
        Kind::LabeledStatement if parent.as_labeled_statement().label() == ident => Space::Label,
        Kind::BreakStatement | Kind::ContinueStatement => Space::Label,
        _ => Space::Value,
    }
}

/// Whether the declaration a binder belongs to carries `export`.
fn declares_public_name(parents: &[P<Node>]) -> bool {
    for node in parents.iter().rev() {
        match node.kind() {
            Kind::VariableDeclaration
            | Kind::VariableDeclarationList
            | Kind::BindingElement
            | Kind::ObjectBindingPattern
            | Kind::ArrayBindingPattern => {}
            _ => return node.modifier_flags().intersects(ModifierFlags::Export),
        }
    }
    false
}

fn visit(node: P<Node>, parents: &mut Vec<P<Node>>, out: &mut Vec<Ident>) {
    if node.kind() == Kind::Identifier {
        if let Some(&parent) = parents.last() {
            let text = node.text().to_owned();
            let end = node.end().source_offset().unwrap_or(0) as usize;
            let start = end.saturating_sub(text.len());
            let role = role_of(node, parent);
            let kind = match parent.kind() {
                Kind::ShorthandPropertyAssignment if parent.name() == Some(node) => {
                    Occurrence::Shorthand
                }
                Kind::BindingElement
                    if parent.name() == Some(node)
                        && parent.as_binding_element().property_name().is_none()
                        && parents
                            .get(parents.len().wrapping_sub(2))
                            .is_some_and(|grand| grand.kind() == Kind::ObjectBindingPattern) =>
                {
                    Occurrence::Shorthand
                }
                Kind::ImportSpecifier if parent.as_import_specifier().property_name().is_none() => {
                    Occurrence::Import
                }
                _ => Occurrence::Identifier,
            };
            out.push(Ident {
                text,
                start,
                role,
                space: space_of(node, parent),
                kind,
                published: role == Role::Binder && declares_public_name(parents),
                export_specifier: parent.kind() == Kind::ExportSpecifier,
            });
        }
        return;
    }
    parents.push(node);
    node.for_each_child_static(&mut |child| {
        visit(child, parents, out);
        false
    });
    parents.pop();
}

fn identifiers(code: &str, tsx: bool) -> Vec<Ident> {
    let region = Region::new(64 << 10);
    let _guard = region.enter();
    let opts = SourceFileParseOptions {
        file_name: if tsx { "/hygiene.tsx" } else { "/hygiene.ts" }.to_owned(),
        ..SourceFileParseOptions::default()
    };
    let kind = if tsx { ScriptKind::TSX } else { ScriptKind::TS };
    let file = parse_source_file_owned(opts, code.to_owned(), kind);
    let mut out = Vec::new();
    let mut parents = Vec::new();
    visit(file.as_node(), &mut parents, &mut out);
    out.sort_by_key(|ident| ident.start);
    out
}

/// Assign every placeholder class its printed name and rewrite `code`.
/// Segment offsets move with the text.
pub fn assign(code: &str, segments: &mut [Segment], tsx: bool) -> String {
    if !code.contains(PREFIX) {
        return code.to_owned();
    }
    let idents = identifiers(code, tsx);

    let mut order: Vec<String> = Vec::new();
    let mut space: FxHashMap<String, Space> = FxHashMap::default();
    let mut binders: FxHashSet<String> = FxHashSet::default();
    let mut pinned: FxHashSet<String> = FxHashSet::default();
    let mut unavailable: FxHashMap<Space, FxHashSet<String>> = FxHashMap::default();
    let mut kinds: FxHashMap<usize, (Occurrence, Role)> = FxHashMap::default();
    for ident in &idents {
        kinds.insert(ident.start, (ident.kind, ident.role));
        if is_placeholder(&ident.text) {
            if ident.export_specifier {
                pinned.insert(ident.text.clone());
            }
            if ident.role == Role::Property {
                continue;
            }
            if !space.contains_key(&ident.text) {
                order.push(ident.text.clone());
                space.insert(ident.text.clone(), ident.space);
            }
            if ident.role == Role::Binder {
                binders.insert(ident.text.clone());
                if ident.published {
                    pinned.insert(ident.text.clone());
                }
            }
            continue;
        }
        if ident.role == Role::Property {
            continue;
        }
        unavailable.entry(ident.space).or_default().insert(ident.text.clone());
    }
    // A class never seen binding, or published, holds its spelling and
    // blocks it for the others.
    for key in &order {
        if pinned.contains(key) || !binders.contains(key) {
            let name = spelling(key).unwrap_or(key).to_owned();
            unavailable.entry(space[key]).or_default().insert(name);
        }
    }
    let mut names: FxHashMap<String, String> = FxHashMap::default();
    let mut taken: FxHashMap<Space, FxHashSet<String>> = FxHashMap::default();
    for key in &order {
        let base = spelling(key).unwrap_or(key).to_owned();
        if pinned.contains(key) || !binders.contains(key) {
            names.insert(key.clone(), base);
            continue;
        }
        let here = space[key];
        let blocked = unavailable.get(&here);
        let claimed = taken.entry(here).or_default();
        let mut candidate = base.clone();
        let mut suffix = 1;
        while blocked.is_some_and(|set| set.contains(&candidate)) || claimed.contains(&candidate) {
            candidate = format!("{base}_{suffix}");
            suffix += 1;
        }
        claimed.insert(candidate.clone());
        names.insert(key.clone(), candidate);
    }

    // Rewrite, tracking how far each old offset moved.
    let mut out = String::with_capacity(code.len());
    let mut edits: Vec<(usize, usize, usize)> = Vec::new();
    let mut cursor = 0;
    let mut position = 0;
    while let Some(found) = code[position..].find(PREFIX) {
        let at = position + found;
        let end = ident_end(code, at);
        if code[..at].ends_with("/*") && code[at..].starts_with(&format!("{PREFIX},")) {
            if let Some(close) = code[at..].find("*/") {
                let close = at + close;
                let renamed = code[at + PREFIX.len() + 1..close]
                    .split(',')
                    .filter(|key| !key.is_empty())
                    .any(|key| {
                        names
                            .get(key)
                            .is_some_and(|printed| Some(printed.as_str()) != spelling(key))
                    });
                out.push_str(&code[cursor..at - 2]);
                if !renamed {
                    out.push(',');
                }
                edits.push((at - 2, close + 2, out.len()));
                cursor = close + 2;
                position = cursor;
                continue;
            }
        }
        let boundary = !code[..at].chars().next_back().is_some_and(ident_char);
        let text = &code[at..end];
        if let (true, Some(name)) = (boundary, spelling(text)) {
            let (kind, role) =
                kinds.get(&at).copied().unwrap_or((Occurrence::Identifier, Role::Reference));
            let printed = if role == Role::Property {
                name.to_owned()
            } else {
                names.get(text).cloned().unwrap_or_else(|| name.to_owned())
            };
            let replacement = match kind {
                Occurrence::Shorthand if printed != name => format!("{name}: {printed}"),
                Occurrence::Import if printed != name => format!("{name} as {printed}"),
                _ => printed,
            };
            out.push_str(&code[cursor..at]);
            out.push_str(&replacement);
            edits.push((at, end, out.len()));
            cursor = end;
        }
        position = end.max(at + 1);
    }
    out.push_str(&code[cursor..]);
    let map = |offset: usize| -> usize {
        let mut moved = offset as isize;
        for (start, end, new_end) in &edits {
            if *end <= offset {
                moved = offset as isize + (*new_end as isize - *end as isize);
            } else if *start < offset {
                moved = *new_end as isize;
            } else {
                break;
            }
        }
        moved.max(0) as usize
    };
    for segment in segments.iter_mut() {
        segment.virtual_start = map(segment.virtual_start as usize) as u32;
        segment.virtual_end = map(segment.virtual_end as usize) as u32;
        // A rewrite inside a verbatim range changed its length: the bytes
        // no longer match one for one, so the range maps as a whole.
        if segment.kind == crate::SegmentKind::Verbatim
            && segment.virtual_end - segment.virtual_start
                != segment.original_end - segment.original_start
        {
            segment.kind = crate::SegmentKind::Atom;
        }
    }
    out
}

fn ident_char(ch: char) -> bool {
    ch == '_' || ch == '$' || ch.is_alphanumeric()
}

fn ident_end(code: &str, at: usize) -> usize {
    code[at..]
        .char_indices()
        .find(|(_, ch)| !ident_char(*ch))
        .map_or(code.len(), |(offset, _)| at + offset)
}
