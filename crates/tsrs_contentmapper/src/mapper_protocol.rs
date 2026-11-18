//! Content mapper protocol additions on mapped outputs and diagnostics.
//!
//! PORT: not in Go. Adds `expansions`, `dependencies` (the files a transform
//! read, e.g. `for syntax` macro modules) and `syntaxImports` on a mapped
//! output ([`MappedOutputExt`]), and `category` / `relatedInformation` on a
//! mapper diagnostic ([`DiagnosticExt`]). `contentmapper/hostimpl.rs` keeps a
//! field and a call each.

use tsrs_ast::{self as ast, DiagnosticExt as _};
use tsrs_core::json::{JsonMap, Value};
use tsrs_core::{P, TextRange};
use tsrs_diagnostics::Category;
use tsrs_spanmap::SpanMap;
use tsrs_spanmap::spanmap_ext::{Expansion, MapperInfo, SyntaxBinding, SyntaxImport};

use crate::hostimpl::{
    ProtocolJson, int_json, member_pointer, unmarshal_bool, unmarshal_int, unmarshal_int32,
    unmarshal_slice, unmarshal_string, unmarshal_struct,
};

/// One macro invocation and the virtual text it produced, in the selected
/// position encoding.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct ExpansionInformation {
    pub virtual_start: i64,
    pub virtual_length: i64,
    pub original_start: i64,
    pub original_length: i64,
    pub label: String,
}

impl ProtocolJson for ExpansionInformation {
    fn marshal_json(&self) -> Value {
        let mut o = JsonMap::new();
        o.insert("virtualStart".to_string(), int_json(self.virtual_start));
        o.insert("virtualLength".to_string(), int_json(self.virtual_length));
        o.insert("originalStart".to_string(), int_json(self.original_start));
        o.insert("originalLength".to_string(), int_json(self.original_length));
        o.insert("label".to_string(), Value::String(self.label.clone()));
        Value::Object(o)
    }

    fn unmarshal_json(value: Value, pointer: &str) -> Result<Self, String> {
        let mut v = ExpansionInformation::default();
        for (name, member) in
            unmarshal_struct(value, "contentmapper.ExpansionInformation", pointer)?
                .into_iter()
                .flatten()
        {
            let pointer = &member_pointer(pointer, &name);
            match name.as_str() {
                "virtualStart" => {
                    v.virtual_start = unmarshal_int(&member, "int", i64::MIN, i64::MAX, pointer)?;
                }
                "virtualLength" => {
                    v.virtual_length = unmarshal_int(&member, "int", i64::MIN, i64::MAX, pointer)?;
                }
                "originalStart" => {
                    v.original_start = unmarshal_int(&member, "int", i64::MIN, i64::MAX, pointer)?;
                }
                "originalLength" => {
                    v.original_length = unmarshal_int(&member, "int", i64::MIN, i64::MAX, pointer)?;
                }
                "label" => v.label = unmarshal_string(member, pointer)?,
                _ => {}
            }
        }
        Ok(v)
    }
}

/// One `for syntax` import of the original file, in the selected position encoding.
///
/// Wire shape: `{specifier, resolved?, bindings: [{imported, local}], shadowsCore?, originalStart, originalLength}`.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct SyntaxImportInformation {
    pub specifier: String,
    pub resolved: Option<String>,
    pub bindings: Vec<SyntaxBindingInformation>,
    pub shadows_core: bool,
    pub original_start: i64,
    pub original_length: i64,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct SyntaxBindingInformation {
    pub imported: String,
    pub local: String,
}

impl ProtocolJson for SyntaxBindingInformation {
    fn marshal_json(&self) -> Value {
        let mut o = JsonMap::new();
        o.insert("imported".to_string(), Value::String(self.imported.clone()));
        o.insert("local".to_string(), Value::String(self.local.clone()));
        Value::Object(o)
    }

    fn unmarshal_json(value: Value, pointer: &str) -> Result<Self, String> {
        let mut v = SyntaxBindingInformation::default();
        for (name, member) in
            unmarshal_struct(value, "contentmapper.SyntaxBindingInformation", pointer)?
                .into_iter()
                .flatten()
        {
            let pointer = &member_pointer(pointer, &name);
            match name.as_str() {
                "imported" => v.imported = unmarshal_string(member, pointer)?,
                "local" => v.local = unmarshal_string(member, pointer)?,
                _ => {}
            }
        }
        Ok(v)
    }
}

impl ProtocolJson for SyntaxImportInformation {
    fn marshal_json(&self) -> Value {
        let mut o = JsonMap::new();
        o.insert("specifier".to_string(), Value::String(self.specifier.clone()));
        if let Some(resolved) = &self.resolved {
            o.insert("resolved".to_string(), Value::String(resolved.clone()));
        }
        o.insert(
            "bindings".to_string(),
            Value::Array(self.bindings.iter().map(ProtocolJson::marshal_json).collect()),
        );
        if self.shadows_core {
            o.insert("shadowsCore".to_string(), Value::Bool(true));
        }
        o.insert("originalStart".to_string(), int_json(self.original_start));
        o.insert("originalLength".to_string(), int_json(self.original_length));
        Value::Object(o)
    }

    fn unmarshal_json(value: Value, pointer: &str) -> Result<Self, String> {
        let mut v = SyntaxImportInformation::default();
        for (name, member) in
            unmarshal_struct(value, "contentmapper.SyntaxImportInformation", pointer)?
                .into_iter()
                .flatten()
        {
            let pointer = &member_pointer(pointer, &name);
            match name.as_str() {
                "specifier" => v.specifier = unmarshal_string(member, pointer)?,
                "resolved" => {
                    v.resolved = match member {
                        Value::Null => None,
                        member => Some(unmarshal_string(member, pointer)?),
                    };
                }
                "bindings" => {
                    v.bindings = unmarshal_slice(
                        member,
                        "[]contentmapper.SyntaxBindingInformation",
                        pointer,
                        SyntaxBindingInformation::unmarshal_json,
                    )?;
                }
                "shadowsCore" => v.shadows_core = unmarshal_bool(&member, pointer)?,
                "originalStart" => {
                    v.original_start = unmarshal_int(&member, "int", i64::MIN, i64::MAX, pointer)?;
                }
                "originalLength" => {
                    v.original_length = unmarshal_int(&member, "int", i64::MIN, i64::MAX, pointer)?;
                }
                _ => {}
            }
        }
        Ok(v)
    }
}

/// Our members of a mapped output. All are optional on the wire.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct MappedOutputExt {
    pub expansions: Vec<ExpansionInformation>,
    pub dependencies: Vec<String>,
    pub syntax_imports: Vec<SyntaxImportInformation>,
}

impl MappedOutputExt {
    /// Writes the members that are present.
    pub fn marshal_members(&self, o: &mut JsonMap) {
        if !self.expansions.is_empty() {
            o.insert(
                "expansions".to_string(),
                Value::Array(self.expansions.iter().map(ProtocolJson::marshal_json).collect()),
            );
        }
        if !self.dependencies.is_empty() {
            o.insert(
                "dependencies".to_string(),
                Value::Array(self.dependencies.iter().cloned().map(Value::String).collect()),
            );
        }
        if !self.syntax_imports.is_empty() {
            o.insert(
                "syntaxImports".to_string(),
                Value::Array(self.syntax_imports.iter().map(ProtocolJson::marshal_json).collect()),
            );
        }
    }

    /// Decodes one of our members; gives `member` back otherwise.
    pub fn unmarshal_member(
        &mut self,
        name: &str,
        member: Value,
        pointer: &str,
    ) -> Result<Option<Value>, String> {
        match name {
            "expansions" => {
                self.expansions = unmarshal_slice(
                    member,
                    "[]contentmapper.ExpansionInformation",
                    pointer,
                    ExpansionInformation::unmarshal_json,
                )?;
            }
            "dependencies" => {
                self.dependencies = unmarshal_slice(member, "[]string", pointer, unmarshal_string)?;
            }
            "syntaxImports" => {
                self.syntax_imports = unmarshal_slice(
                    member,
                    "[]contentmapper.SyntaxImportInformation",
                    pointer,
                    SyntaxImportInformation::unmarshal_json,
                )?;
            }
            _ => return Ok(Some(member)),
        }
        Ok(None)
    }
}

/// One `relatedInformation` entry of a mapper diagnostic, in original-source
/// coordinates.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct RelatedInformation {
    pub message_text: String,
    pub start: i64,
    pub length: i64,
}

impl ProtocolJson for RelatedInformation {
    fn marshal_json(&self) -> Value {
        let mut o = JsonMap::new();
        o.insert("messageText".to_string(), Value::String(self.message_text.clone()));
        o.insert("start".to_string(), int_json(self.start));
        o.insert("length".to_string(), int_json(self.length));
        Value::Object(o)
    }

    fn unmarshal_json(value: Value, pointer: &str) -> Result<Self, String> {
        let mut v = RelatedInformation::default();
        for (name, member) in unmarshal_struct(value, "contentmapper.RelatedInformation", pointer)?
            .into_iter()
            .flatten()
        {
            let pointer = &member_pointer(pointer, &name);
            match name.as_str() {
                "messageText" => v.message_text = unmarshal_string(member, pointer)?,
                "start" => v.start = unmarshal_int(&member, "int", i64::MIN, i64::MAX, pointer)?,
                "length" => v.length = unmarshal_int(&member, "int", i64::MIN, i64::MAX, pointer)?,
                _ => {}
            }
        }
        Ok(v)
    }
}

/// Extra members of a mapper diagnostic (`category` and `relatedInformation`).
///
/// Carries an optional TypeScript `DiagnosticCategory` (0 warning, 1 error,
/// 2 suggestion, 3 message; absent means error, as in Go) and secondary
/// locations in the original content.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct DiagnosticExt {
    pub category: Option<i32>,
    pub related_information: Vec<RelatedInformation>,
}

impl DiagnosticExt {
    /// Writes the members that are present.
    pub fn marshal_members(&self, o: &mut JsonMap) {
        if let Some(category) = self.category {
            o.insert("category".to_string(), Value::Number(f64::from(category)));
        }
        if !self.related_information.is_empty() {
            o.insert(
                "relatedInformation".to_string(),
                Value::Array(
                    self.related_information.iter().map(ProtocolJson::marshal_json).collect(),
                ),
            );
        }
    }

    /// Decodes `category` or `relatedInformation`.
    pub fn unmarshal_member(
        &mut self,
        name: &str,
        member: Value,
        pointer: &str,
    ) -> Result<(), String> {
        if name == "category" {
            self.category = match member {
                Value::Null => None,
                member => Some(unmarshal_int32(&member, "int32", pointer)?),
            };
            return Ok(());
        }
        self.related_information = unmarshal_slice(
            member,
            "[]contentmapper.RelatedInformation",
            pointer,
            RelatedInformation::unmarshal_json,
        )?;
        Ok(())
    }

    /// The category to report; anything absent or unknown stays an error.
    #[must_use]
    pub fn category(&self) -> Category {
        match self.category {
            Some(0) => Category::Warning,
            Some(2) => Category::Suggestion,
            Some(3) => Category::Message,
            _ => Category::Error,
        }
    }

    /// Applies the category and related locations to the decoded diagnostic.
    ///
    /// `normalize` maps an original position to UTF-8; a related location that
    /// does not map is dropped, not an error.
    pub fn apply<E>(
        &self,
        diagnostic: Option<P<ast::Diagnostic>>,
        normalize: impl Fn(i64) -> Result<u32, E>,
    ) {
        let Some(diagnostic) = diagnostic else {
            return;
        };
        diagnostic.set_category(self.category());
        let (source, code) = (diagnostic.source(), diagnostic.code());
        let mut related = Vec::with_capacity(self.related_information.len());
        for item in &self.related_information {
            if item.start < 0 || item.length < 0 || item.start > i64::MAX - item.length {
                continue;
            }
            let (Some(start), Some(end)) =
                (normalize(item.start).ok(), normalize(item.start + item.length).ok())
            else {
                continue;
            };
            related.push(ast::new_external_diagnostic(
                None,
                TextRange::new(start, end),
                source,
                Category::Message,
                code,
                &item.message_text,
            ));
        }
        if !related.is_empty() {
            diagnostic.set_related_info(&related);
        }
    }
}

/// Attaches the normalized mapper report to the freshly decoded span map.
///
/// An entry with a position outside its text is dropped: it is information for
/// language features and tools, not part of the mapping contract. Even if the
/// decoded map is already shared, `with_mapper_info` allocates a fresh
/// `P<SpanMap>` copy carrying the report rather than mutating in place or
/// dropping it silently.
pub fn attach_mapper_info<E>(
    mappings: &mut Option<P<SpanMap>>,
    output: &MappedOutputExt,
    virtual_pos: impl Fn(i64) -> Result<u32, E>,
    original_pos: impl Fn(i64) -> Result<u32, E>,
) {
    let info = mapper_info(output, virtual_pos, original_pos);
    if info.is_empty() {
        return;
    }
    let Some(map) = mappings.take() else {
        return;
    };
    *mappings = Some(map.with_mapper_info(info));
}

/// The normalized report of one mapped output.
pub fn mapper_info<E>(
    output: &MappedOutputExt,
    virtual_pos: impl Fn(i64) -> Result<u32, E>,
    original_pos: impl Fn(i64) -> Result<u32, E>,
) -> MapperInfo {
    let range = |start: i64, length: i64, f: &dyn Fn(i64) -> Result<u32, E>| {
        if start < 0 || length < 0 || start > i64::MAX - length {
            return None;
        }
        Some((f(start).ok()?, f(start + length).ok()?))
    };
    let expansions = output
        .expansions
        .iter()
        .filter_map(|item| {
            let (virtual_start, virtual_end) =
                range(item.virtual_start, item.virtual_length, &virtual_pos)?;
            let (original_start, original_end) =
                range(item.original_start, item.original_length, &original_pos)?;
            Some(Expansion {
                virtual_start,
                virtual_end,
                original_start,
                original_end,
                label: item.label.clone(),
            })
        })
        .collect();
    let syntax_imports = output
        .syntax_imports
        .iter()
        .filter_map(|item| {
            let (original_start, original_end) =
                range(item.original_start, item.original_length, &original_pos)?;
            Some(SyntaxImport {
                specifier: item.specifier.clone(),
                resolved: item.resolved.clone(),
                bindings: item
                    .bindings
                    .iter()
                    .map(|b| SyntaxBinding { imported: b.imported.clone(), local: b.local.clone() })
                    .collect(),
                shadows_core: item.shadows_core,
                original_start,
                original_end,
            })
        })
        .collect();
    let mut dependencies = output.dependencies.clone();
    dependencies.sort();
    dependencies.dedup();
    MapperInfo { expansions, dependencies, syntax_imports }
}
