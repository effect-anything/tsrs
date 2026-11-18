//! Invalidation by a content-mapped file's dependencies.
//!
//! A transform can read files other than its input (Sweetener reads the
//! `for syntax` macro modules), and the mapper reports them as
//! `dependencies`. Go keys a mapped parse by the original text and the
//! transform identity only, so after an edit of a macro module the
//! language server kept serving the old expansion of every file that
//! imports it. Here the parse cache key also folds in the state of those
//! files: [`salt`] is mixed into the transform identity, and [`record`]
//! notes the dependencies after each acquire.
//!
//! The state of a dependency is the hash of its text on disk -- the mapper
//! reads the disk, not the editor's buffer. The first state seen for a
//! file is its baseline and salts nothing, so the key stays Go's until a
//! dependency changes; any other state salts with the hash of every
//! dependency the file has had, which no other state shares.
//!
//! `tsrs_project/src/compilerhost.rs` keeps one call to each, and
//! `tsrs_project/src/projectcollectionbuilder.rs` one to [`with_dependents`] so a
//! changed macro module marks its importers changed; the watcher and the
//! incremental build use [`program_dependencies`] and [`version_text`].

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};

use rustc_hash::{FxHashMap, FxHashSet};
use tsrs_ast::SourceFile;
use tsrs_core::P;
use tsrs_core::tspath::Path;
use tsrs_spanmap::SpanMap;
use xxhash_rust::xxh3::xxh3_128;

use crate::program::Program;

#[derive(Default)]
struct Record {
    /// Each dependency's hash when the file was first transformed.
    baseline: BTreeMap<String, u128>,
    /// Every dependency the file has had since.
    seen: BTreeSet<String>,
}

fn records() -> &'static Mutex<FxHashMap<String, Record>> {
    static RECORDS: OnceLock<Mutex<FxHashMap<String, Record>>> = OnceLock::new();
    RECORDS.get_or_init(Default::default)
}

/// Whether `path` currently has a dependency record.
#[must_use]
pub fn has_record(path: &str) -> bool {
    records().lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains_key(path)
}

/// The hash of a file's text on disk; `0` when it cannot be read.
#[must_use]
pub fn disk_state(path: &str) -> u128 {
    match tsrs_vfs::osvfs::fs().read_file(path) {
        Some(text) => xxh3_128(text.as_bytes()) | 1,
        None => 0,
    }
}

/// What to mix into the cache key of the mapped file at `path`. `state`
/// reads a dependency's current state (normally [`disk_state`]).
#[must_use]
pub fn salt_with(path: &str, state: &dyn Fn(&str) -> u128) -> u128 {
    let records = records().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(record) = records.get(path) else {
        return 0;
    };
    if record.baseline.iter().all(|(dep, hash)| state(dep) == *hash) {
        return 0;
    }
    let mut buf = Vec::with_capacity(record.seen.len() * 48);
    for dep in &record.seen {
        buf.extend_from_slice(dep.as_bytes());
        buf.push(0);
        buf.extend_from_slice(&state(dep).to_le_bytes());
    }
    xxh3_128(&buf) | 1
}

/// [`salt_with`] the disk.
#[must_use]
pub fn salt(path: &str) -> u128 {
    salt_with(path, &disk_state)
}

/// Notes the dependencies the mapper reported for the file at `path`.
pub fn record_with(path: &str, span_map: Option<&SpanMap>, state: &dyn Fn(&str) -> u128) {
    let deps = SpanMap::dependencies(span_map);
    let mut records = records().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if deps.is_empty() && !records.contains_key(path) {
        return;
    }
    let record = records.entry(path.to_owned()).or_default();
    // A dependency that appears after the first transform (a macro
    // module that started to import another) is not baseline: the
    // baseline state did not read it.
    record.seen.extend(deps.iter().cloned());
    if record.baseline.is_empty() {
        for dep in deps {
            record.baseline.insert(dep.clone(), state(dep));
        }
    }
}

/// [`record_with`] the disk, from the canonical file of a mapped parse.
pub fn record(path: &str, files: &tsrs_contentmapper::SourceFiles) {
    let span_map = files.canonical.and_then(|file| file.span_map());
    record_with(path, span_map.as_deref(), &disk_state);
}

/// Every dependency of the mapped files of `program`, for the watcher's seen files.
///
/// An edit of a macro module outside the program must start a build too.
#[must_use]
pub fn program_dependencies(program: Option<&Program>) -> Vec<String> {
    let Some(program) = program else {
        return Vec::new();
    };
    let mut out = BTreeSet::new();
    #[expect(
        clippy::iter_over_hash_type,
        reason = "collected into a BTreeSet, so output order is deterministic"
    )]
    for file in program.files_by_path().values() {
        let span_map = file.span_map();
        out.extend(SpanMap::dependencies(span_map.as_deref()).iter().cloned());
    }
    out.into_iter().collect()
}

/// Returns `paths` plus every content-mapped file of `program` that read one of them.
///
/// Used by the language server's file change handling so a changed macro module marks
/// the files that import it changed even though their own text did not change.
#[must_use]
pub fn with_dependents<'a>(
    paths: &'a [Path],
    program: Option<&Program>,
    to_path: &dyn Fn(&str) -> Path,
) -> Cow<'a, [Path]> {
    let Some(program) = program else {
        return Cow::Borrowed(paths);
    };
    if paths.is_empty() {
        return Cow::Borrowed(paths);
    }
    let changed: FxHashSet<&Path> = paths.iter().collect();
    let mut extra = Vec::new();
    #[expect(clippy::iter_over_hash_type, reason = "extra paths are sorted before returning")]
    for (path, file) in program.files_by_path() {
        let span_map = file.span_map();
        let deps = SpanMap::dependencies(span_map.as_deref());
        if !deps.is_empty()
            && !changed.contains(path)
            && deps.iter().any(|dep| changed.contains(&to_path(dep)))
        {
            extra.push(path.clone());
        }
    }
    if extra.is_empty() {
        return Cow::Borrowed(paths);
    }
    extra.sort();
    let mut all = paths.to_vec();
    all.extend(extra);
    Cow::Owned(all)
}

/// The text an incremental build hashes for a content-mapped file's version.
///
/// Go hashes the original text and the transform identity, so a file whose expansion
/// changed with a macro module kept its version and reused old diagnostics and output.
/// When the file has macro module dependencies, the virtual text is included as well.
#[must_use]
pub fn version_text(file: P<SourceFile>) -> String {
    let original = file.original_text();
    let identity = file.content_mapper_transform_identity();
    let span_map = file.span_map();
    // Upstream (Go incremental/programtosnapshot.go:94) is original text plus
    // transform identity. Only a file whose expansion read other modules can
    // change without either changing, so only it adds the virtual text; every
    // other mapped file keeps the upstream version and buildinfo.
    if SpanMap::dependencies(span_map.as_deref()).is_empty() {
        format!("{original}\x00{identity}")
    } else {
        format!("{original}\x00{identity}\x00{}", file.text())
    }
}
