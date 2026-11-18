//! API methods added for content-mapped files (`.sts`/`.stsx` and other content mappers).
//!
//! The upstream API serves the *virtual* file: the AST `getSourceFile` returns is the mapper's
//! output. These methods tell a client what the mapper did, following the conventions of the
//! other methods: a request names `snapshot`, `project` and `file`, a batch takes `files` or
//! `positions` and answers one entry per input, in order.
//!
//! - `getMappedFileInfo` `{snapshot, project, file}` -> `MappedFileInfo | null`
//! - `getMappedFilesInfo` `{snapshot, project, files?}` -> `(MappedFileInfo | null)[]`;
//!   without `files`, every content-mapped file of the program.
//! - `getOriginalPositions` `{snapshot, project, file, positions}` ->
//!   `{position, fidelity}[]`: virtual -> original.
//! - `getVirtualPositions` `{snapshot, project, file, positions}` ->
//!   `{position, fidelity}[][]`: original -> virtual. One original position
//!   can appear several times in macro output, so each entry is a list.
//!
//! Positions are UTF-16 code units, as everywhere in the API; the Rust functions below
//! ([`mapped_file_info`], [`original_positions`], [`virtual_positions`], [`regions`]) work in
//! UTF-8 bytes like the compiler. A file no mapper produced maps every position to itself
//! (`exact`).

use tsrs_api_transport::strictjson::{self, DecodeError};
use tsrs_ast::{SourceFile, compute_position_map};
use tsrs_core::P;
use tsrs_core::json::Value;
use tsrs_project::ID as ProjectID;
use tsrs_spanmap::{
    Expansion, Feature, Fidelity, Kind, MapperInfo, SpanMap, SyntaxImport,
    virtual_to_original_position,
};

use crate::handler::{ApiError, ApiResult, Response};
use crate::session::{Session, SnapshotID, json_response};
use crate::wire::{DocumentIdentifier, Obj, Params, b, s, strings};

pub const GET_MAPPED_FILE_INFO: &str = "getMappedFileInfo";
pub const GET_MAPPED_FILES_INFO: &str = "getMappedFilesInfo";
pub const GET_ORIGINAL_POSITIONS: &str = "getOriginalPositions";
pub const GET_VIRTUAL_POSITIONS: &str = "getVirtualPositions";

const fn u32_json(n: u32) -> Value {
    Value::Integer(n as i64)
}

/// Dispatches content-mapper extension methods, or returns `None` for upstream methods.
pub fn handle(session: &Session, method: &str, params: &[u8]) -> Option<ApiResult<Response>> {
    if !matches!(
        method,
        GET_MAPPED_FILE_INFO
            | GET_MAPPED_FILES_INFO
            | GET_ORIGINAL_POSITIONS
            | GET_VIRTUAL_POSITIONS
    ) {
        return None;
    }
    let parsed = match MappedFileParams::decode(params) {
        Ok(parsed) => parsed,
        Err(err) => return Some(Err(err)),
    };
    Some(dispatch(session, method, &parsed).and_then(|value| json_response(&value)))
}

fn dispatch(session: &Session, method: &str, params: &MappedFileParams) -> ApiResult<Value> {
    let sd = session.snapshot_data(params.snapshot)?;
    let program = sd.get_program(&params.project)?;
    let find = |file: &DocumentIdentifier| program.get_source_file(&file.to_file_name());
    match method {
        GET_MAPPED_FILE_INFO => {
            let value = params
                .file
                .as_ref()
                .and_then(find)
                .map_or(Value::Null, |source_file| api_file_info(source_file).to_json());
            Ok(value)
        }
        GET_MAPPED_FILES_INFO => {
            let infos: Vec<Value> = match &params.files {
                Some(files) => files
                    .iter()
                    .map(|file| {
                        find(file)
                            .map_or(Value::Null, |source_file| api_file_info(source_file).to_json())
                    })
                    .collect(),
                None => program
                    .get_source_files()
                    .iter()
                    .copied()
                    .filter(|source_file| source_file.is_content_mapped())
                    .map(|source_file| api_file_info(source_file).to_json())
                    .collect(),
            };
            Ok(Value::Array(infos))
        }
        _ => {
            let Some(source_file) = params.file.as_ref().and_then(find) else {
                return Ok(Value::Null);
            };
            let span_map = source_file.span_map();
            let map = span_map.as_deref();
            let virtual_map = source_file.get_position_map();
            let original_map = source_file
                .is_content_mapped()
                .then(|| compute_position_map(source_file.original_text()));
            let original_map = original_map.as_ref().unwrap_or(&virtual_map);
            if method == GET_ORIGINAL_POSITIONS {
                let input: Vec<u32> =
                    params.positions.iter().map(|&p| virtual_map.utf16_to_utf8(p)).collect();
                let mut out = original_positions(map, &input);
                for item in &mut out {
                    item.position = original_map.utf8_to_utf16(item.position);
                }
                Ok(Value::Array(out.iter().map(MappedPosition::to_json).collect()))
            } else {
                let input: Vec<u32> =
                    params.positions.iter().map(|&p| original_map.utf16_to_utf8(p)).collect();
                let mut out = virtual_positions(map, &input);
                for item in out.iter_mut().flatten() {
                    item.position = virtual_map.utf8_to_utf16(item.position);
                }
                Ok(Value::Array(
                    out.into_iter()
                        .map(|group| {
                            Value::Array(group.iter().map(MappedPosition::to_json).collect())
                        })
                        .collect(),
                ))
            }
        }
    }
}

/// One file's answer with every position in UTF-16 code units, as the API reports positions.
fn api_file_info(source_file: P<SourceFile>) -> MappedFileInfo {
    let mut answer = mapped_file_info(source_file);
    let virtual_map = source_file.get_position_map();
    let original_map =
        source_file.is_content_mapped().then(|| compute_position_map(source_file.original_text()));
    let original_map = original_map.as_ref().unwrap_or(&virtual_map);
    answer.convert_positions(&|p| virtual_map.utf8_to_utf16(p), &|p| original_map.utf8_to_utf16(p));
    answer
}

/// Params of every method here; each reads the members it needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MappedFileParams {
    pub snapshot: SnapshotID,
    pub project: ProjectID,
    pub file: Option<DocumentIdentifier>,
    pub files: Option<Vec<DocumentIdentifier>>,
    pub positions: Vec<u32>,
}

impl MappedFileParams {
    /// Decodes raw JSON request bytes into [`MappedFileParams`].
    pub fn decode(params: &[u8]) -> ApiResult<Self> {
        if params.is_empty() {
            return Ok(Self::default());
        }
        let decoded = strictjson::decode(params).map_err(|err| match err {
            DecodeError::Invalid(msg) => ApiError::invalid_request(format!(
                "failed to unmarshal *api.MappedFileParams: {msg}"
            )),
            DecodeError::OutOfRange(msg) => ApiError::invalid_request(msg),
        })?;
        match &decoded.value {
            Value::Null => return Ok(Self::default()),
            Value::Object(_) => {}
            _ => {
                return Err(ApiError::invalid_request(
                    "failed to unmarshal *api.MappedFileParams: params must be an object",
                ));
            }
        }
        let p = Params(&decoded.value);
        let obj = p.object()?;
        let snapshot = parse_u64(
            p.get("snapshot"),
            decoded.lexemes.get("/snapshot").map(String::as_str),
            "snapshot",
        )?;
        let project = ProjectID(p.str("project")?.to_owned());
        let file = if obj.contains_key("file") {
            Some(DocumentIdentifier::parse(p.get("file"), "file")?)
        } else {
            None
        };
        let files = if obj.contains_key("files") && !matches!(p.get("files"), Value::Null) {
            Some(DocumentIdentifier::parse_list(p.array("files")?, "files")?)
        } else {
            None
        };
        let positions = p
            .array("positions")?
            .iter()
            .enumerate()
            .map(|(idx, v)| {
                let ptr = format!("/positions/{idx}");
                parse_u32(v, decoded.lexemes.get(&ptr).map(String::as_str), "positions")
            })
            .collect::<ApiResult<Vec<u32>>>()?;
        Ok(Self { snapshot, project, file, files, positions })
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "f64 bounds and integrality are checked immediately before casting"
)]
fn parse_u64(v: &Value, lexeme: Option<&str>, key: &str) -> ApiResult<u64> {
    if let Some(raw) = lexeme {
        return raw.parse::<u64>().map_err(|_| {
            ApiError::invalid_request(format!("{key} must be a non-negative integer"))
        });
    }
    match v {
        Value::Integer(n) => u64::try_from(*n).map_err(|_| {
            ApiError::invalid_request(format!("{key} must be a non-negative integer"))
        }),
        Value::Number(x) if *x >= 0.0 && x.fract() == 0.0 && *x < 18_446_744_073_709_551_616.0 => {
            Ok(*x as u64)
        }
        Value::Null => Ok(0),
        _ => Err(ApiError::invalid_request(format!("{key} must be a non-negative integer"))),
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "f64 bounds and integrality are checked immediately before casting"
)]
fn parse_u32(v: &Value, lexeme: Option<&str>, key: &str) -> ApiResult<u32> {
    if let Some(raw) = lexeme {
        return raw
            .parse::<u32>()
            .map_err(|_| ApiError::invalid_request(format!("{key} must be an array of uint32")));
    }
    match v {
        Value::Integer(n) => u32::try_from(*n)
            .map_err(|_| ApiError::invalid_request(format!("{key} must be an array of uint32"))),
        Value::Number(x) if *x >= 0.0 && x.fract() == 0.0 && *x <= f64::from(u32::MAX) => {
            Ok(*x as u32)
        }
        _ => Err(ApiError::invalid_request(format!("{key} must be an array of uint32"))),
    }
}

/// How a mapped position relates to its counterpart.
const fn fidelity_name(fidelity: Fidelity) -> &'static str {
    match fidelity {
        Fidelity::Exact => "exact",
        Fidelity::Atom => "atom",
        Fidelity::Approximate => "approximate",
        Fidelity::None => "none",
    }
}

/// One mapped position and its mapping fidelity.
///
/// `fidelity` is `exact` (1:1, safe for edits), `atom` (inside generated code: the position
/// of its whole original range), `approximate`, or `none` (no counterpart; `position` is the
/// nearest one the map gives).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MappedPosition {
    pub position: u32,
    pub fidelity: &'static str,
}

impl MappedPosition {
    #[must_use]
    pub fn to_json(&self) -> Value {
        Obj::new()
            .set("position", u32_json(self.position))
            .set("fidelity", s(self.fidelity))
            .build()
    }
}

/// Virtual -> original, one answer per position.
#[must_use]
pub fn original_positions(map: Option<&SpanMap>, positions: &[u32]) -> Vec<MappedPosition> {
    positions
        .iter()
        .map(|&pos| {
            let (position, fidelity) = virtual_to_original_position(map, pos);
            MappedPosition { position, fidelity: fidelity_name(fidelity) }
        })
        .collect()
}

/// Original -> virtual: every virtual projection of each position, in virtual order; empty for a
/// position no segment covers.
#[must_use]
pub fn virtual_positions(map: Option<&SpanMap>, positions: &[u32]) -> Vec<Vec<MappedPosition>> {
    positions
        .iter()
        .map(|&pos| match map {
            None => vec![MappedPosition { position: pos, fidelity: "exact" }],
            Some(map) => map
                .original_to_virtual_positions(pos, Feature::All)
                .into_iter()
                .map(|mapped| MappedPosition {
                    position: mapped.position,
                    fidelity: fidelity_name(mapped.fidelity),
                })
                .collect(),
        })
        .collect()
}

/// One run of the virtual text and where it came from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Region {
    pub virtual_start: u32,
    pub virtual_end: u32,
    /// `verbatim`, `generated` or `synthesized`.
    pub kind: &'static str,
    /// The original range; `None` for a synthesized run.
    pub original: Option<(u32, u32)>,
}

impl Region {
    #[must_use]
    pub fn to_json(&self) -> Value {
        let obj = Obj::new()
            .set("virtualStart", u32_json(self.virtual_start))
            .set("virtualEnd", u32_json(self.virtual_end))
            .set("kind", s(self.kind));
        match self.original {
            Some((start, end)) => {
                obj.set("originalStart", u32_json(start)).set("originalEnd", u32_json(end)).build()
            }
            None => obj.build(),
        }
    }
}

/// Partitions the virtual text `[0, len)` into consecutive provenance regions.
///
/// A verbatim segment is user-written text, an atom is generated text attributed to its
/// original range, and anything between segments is synthesized. Adjacent runs of one kind
/// that continue each other are merged.
#[must_use]
pub fn regions(map: Option<&SpanMap>, len: u32) -> Vec<Region> {
    let Some(map) = map else {
        return vec![Region {
            virtual_start: 0,
            virtual_end: len,
            kind: "verbatim",
            original: Some((0, len)),
        }];
    };
    let mut out: Vec<Region> = Vec::new();
    let mut push = |region: Region| {
        if region.virtual_start >= region.virtual_end {
            return;
        }
        if let Some(last) = out.last_mut()
            && last.kind == region.kind
            && last.virtual_end == region.virtual_start
        {
            match (last.original, region.original) {
                (None, None) => {
                    last.virtual_end = region.virtual_end;
                    return;
                }
                (Some((ls, le)), Some((rs, re))) if region.kind == "verbatim" && le == rs => {
                    last.virtual_end = region.virtual_end;
                    last.original = Some((ls, re));
                    return;
                }
                _ => {}
            }
        }
        out.push(region);
    };
    let mut cursor = 0;
    for segment in map.segments() {
        push(Region {
            virtual_start: cursor,
            virtual_end: segment.virtual_start.min(len),
            kind: "synthesized",
            original: None,
        });
        push(Region {
            virtual_start: segment.virtual_start,
            virtual_end: segment.virtual_end.min(len),
            kind: if segment.kind == Kind::VERBATIM { "verbatim" } else { "generated" },
            original: Some((segment.original_start, segment.original_end)),
        });
        cursor = cursor.max(segment.virtual_end);
    }
    push(Region { virtual_start: cursor, virtual_end: len, kind: "synthesized", original: None });
    out
}

fn expansion_to_json(item: &Expansion) -> Value {
    Obj::new()
        .set("virtualStart", u32_json(item.virtual_start))
        .set("virtualEnd", u32_json(item.virtual_end))
        .set("originalStart", u32_json(item.original_start))
        .set("originalEnd", u32_json(item.original_end))
        .set("label", s(item.label.clone()))
        .build()
}

fn syntax_import_to_json(item: &SyntaxImport) -> Value {
    let bindings: Vec<Value> = item
        .bindings
        .iter()
        .map(|binding| {
            Obj::new()
                .set("imported", s(binding.imported.clone()))
                .set("local", s(binding.local.clone()))
                .build()
        })
        .collect();
    Obj::new()
        .set("specifier", s(item.specifier.clone()))
        .set_opt("resolved", item.resolved.as_ref().map(|r| s(r.clone())))
        .set("bindings", Value::Array(bindings))
        .set("shadowsCore", b(item.shadows_core))
        .set("originalStart", u32_json(item.original_start))
        .set("originalEnd", u32_json(item.original_end))
        .build()
}

/// What a content mapper did to one file. `mapped` is false (and the mapper fields empty) for a
/// file no mapper produced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MappedFileInfo {
    pub file_name: String,
    pub mapped: bool,
    pub content_mapper: String,
    pub dependencies: Vec<String>,
    pub syntax_imports: Vec<SyntaxImport>,
    pub expansions: Vec<Expansion>,
    pub regions: Vec<Region>,
}

impl MappedFileInfo {
    #[must_use]
    pub fn to_json(&self) -> Value {
        Obj::new()
            .set("fileName", s(self.file_name.clone()))
            .set("mapped", b(self.mapped))
            .set("contentMapper", s(self.content_mapper.clone()))
            .set("dependencies", strings(self.dependencies.iter().cloned()))
            .set(
                "syntaxImports",
                Value::Array(self.syntax_imports.iter().map(syntax_import_to_json).collect()),
            )
            .set(
                "expansions",
                Value::Array(self.expansions.iter().map(expansion_to_json).collect()),
            )
            .set("regions", Value::Array(self.regions.iter().map(Region::to_json).collect()))
            .build()
    }

    /// Rewrites every position: `v` for virtual ones, `o` for original ones.
    pub fn convert_positions(&mut self, v: &dyn Fn(u32) -> u32, o: &dyn Fn(u32) -> u32) {
        for item in &mut self.expansions {
            item.virtual_start = v(item.virtual_start);
            item.virtual_end = v(item.virtual_end);
            item.original_start = o(item.original_start);
            item.original_end = o(item.original_end);
        }
        for item in &mut self.syntax_imports {
            item.original_start = o(item.original_start);
            item.original_end = o(item.original_end);
        }
        for item in &mut self.regions {
            item.virtual_start = v(item.virtual_start);
            item.virtual_end = v(item.virtual_end);
            item.original = item.original.map(|(start, end)| (o(start), o(end)));
        }
    }
}

/// The answer for one [`SourceFile`], in UTF-8 byte offsets.
#[must_use]
pub fn mapped_file_info(file: P<SourceFile>) -> MappedFileInfo {
    let virtual_len = u32::try_from(file.text().len()).unwrap_or(u32::MAX);
    let span_map = file.span_map();
    file_info_of(
        file.file_name(),
        file.is_content_mapped().then(|| (file.content_mapper(), span_map.as_deref())),
        virtual_len,
    )
}

/// [`mapped_file_info`] from the mapper's name and span map.
#[must_use]
pub fn file_info_of(
    file_name: &str,
    mapper: Option<(&str, Option<&SpanMap>)>,
    virtual_len: u32,
) -> MappedFileInfo {
    let Some((content_mapper, map)) = mapper else {
        return MappedFileInfo {
            file_name: file_name.to_owned(),
            regions: regions(None, virtual_len),
            ..MappedFileInfo::default()
        };
    };
    let MapperInfo { expansions, dependencies, syntax_imports } = SpanMap::mapper_info(map).clone();
    MappedFileInfo {
        file_name: file_name.to_owned(),
        mapped: true,
        content_mapper: content_mapper.to_owned(),
        dependencies,
        syntax_imports,
        expansions,
        regions: regions(map, virtual_len),
    }
}
