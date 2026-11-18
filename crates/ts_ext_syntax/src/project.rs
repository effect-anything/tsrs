//! A project: the tsconfig, its tsc-ext options, its root files, and the
//! rewritten text of each file. The bundler integration (`load` hook) and the
//! Node API `Project` sit on this.
//!
//! The rewrite of one file needs no other file: Sweetener expansion reads
//! its macro modules (they are the file's `dependencies`), tsover and the
//! `TypeGPU` wrap are per-file syntax. The project holds what is shared:
//! the options, the include list (`files`/`include`/`exclude` as `tsc`
//! reads them, through the port's own config parser), a cache of results
//! keyed by text, and the reverse dependency index that turns a changed
//! macro module into the files to load again. Expansion itself also goes
//! through the process-wide Sweetener memo and incremental replay, which
//! the open warms in the background (`prewarm`).
//!
//! One project per tsconfig and options: [`open`] returns the registered
//! one while any handle to it is alive.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use rustc_hash::FxHashMap;
use tsrs_core::tspath;
use tsrs_vfs::{Entries, FS, FileInfo};

use crate::TransformSession;
use crate::config::{self, Options};

/// One file's answer, in the shape the Node API returns (JSON text).
#[derive(Clone, Debug)]
struct Cached {
    text_hash: u64,
    json: Arc<str>,
    dependencies: Vec<String>,
}

#[derive(Debug, Default)]
struct State {
    options: Options,
    session: Option<TransformSession>,
    files: BTreeSet<String>,
    extensions: Vec<String>,
    /// tsconfig and the configs it extends (a change reopens the project).
    configs: Vec<String>,
    cache: FxHashMap<String, Cached>,
    /// dependency path → files whose rewrite read it.
    dependents: FxHashMap<String, BTreeSet<String>>,
    config_errors: Vec<String>,
}

#[derive(Debug)]
pub struct Project {
    pub tsconfig: String,
    explicit: Options,
    state: Mutex<State>,
}

/// What [`Project::get_file`] answers when the file is not in the project.
pub const NOT_OWNED: &str = "not owned";

fn normalize(path: &str) -> String {
    let path = path.split('?').next().unwrap_or(path);
    path.replace('\\', "/")
}

fn hash_text(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = rustc_hash::FxHasher::default();
    text.hash(&mut hasher);
    hasher.finish()
}

const SELF_MANIFEST: &str = r#"{"name":"@effect-x/tsc","version":"0.0.0","typescript":{"contentMapper":{"exec":["tsc-ext"]}}}"#;

/// `tsc` drops a `contentMappers` entry whose package it cannot resolve in
/// `node_modules`. Because this library *is* `@effect-x/tsc`, a project whose
/// `node_modules` does not yet link `@effect-x/tsc` still resolves the mapper
/// through a fallback manifest at the filesystem root, after every real
/// `node_modules` directory above the tsconfig has been checked.
struct SelfMapperFs<F> {
    inner: F,
    fallback_nm: String,
    fallback_scope: String,
    fallback_pkg: String,
    fallback_manifest: String,
}

impl<F: FS> SelfMapperFs<F> {
    fn new(inner: F, cwd: &str) -> Self {
        let root_len = tspath::get_root_length(cwd);
        let root = if root_len > 0 { &cwd[..root_len] } else { "/" };
        let fallback_nm = tspath::combine_paths(root, &["node_modules"]);
        let fallback_scope = tspath::combine_paths(&fallback_nm, &["@effect-x"]);
        let fallback_pkg = tspath::combine_paths(&fallback_nm, &[crate::config::PACKAGE]);
        let fallback_manifest = tspath::combine_paths(&fallback_pkg, &["package.json"]);
        Self { inner, fallback_nm, fallback_scope, fallback_pkg, fallback_manifest }
    }
}

impl<F: FS> FS for SelfMapperFs<F> {
    fn use_case_sensitive_file_names(&self) -> bool {
        self.inner.use_case_sensitive_file_names()
    }

    fn file_exists(&self, path: &str) -> bool {
        self.inner.file_exists(path) || path == self.fallback_manifest
    }

    fn read_file(&self, path: &str) -> Option<String> {
        self.inner
            .read_file(path)
            .or_else(|| (path == self.fallback_manifest).then(|| SELF_MANIFEST.to_owned()))
    }

    fn write_file(&self, path: &str, data: &str) -> Result<(), String> {
        self.inner.write_file(path, data)
    }

    fn append_file(&self, path: &str, data: &str) -> Result<(), String> {
        self.inner.append_file(path, data)
    }

    fn remove(&self, path: &str) -> Result<(), String> {
        self.inner.remove(path)
    }

    fn chtimes(&self, path: &str, a_time: SystemTime, m_time: SystemTime) -> Result<(), String> {
        self.inner.chtimes(path, a_time, m_time)
    }

    fn directory_exists(&self, path: &str) -> bool {
        self.inner.directory_exists(path)
            || path == self.fallback_nm
            || path == self.fallback_scope
            || path == self.fallback_pkg
    }

    fn get_accessible_entries(&self, path: &str) -> Entries {
        self.inner.get_accessible_entries(path)
    }

    fn stat(&self, path: &str) -> Option<FileInfo> {
        self.inner.stat(path)
    }

    fn realpath(&self, path: &str) -> String {
        if (path == self.fallback_pkg || path == self.fallback_manifest)
            && !self.inner.directory_exists(&self.fallback_pkg)
        {
            return path.to_owned();
        }
        self.inner.realpath(path)
    }
}

struct ConfigHost<F> {
    fs: SelfMapperFs<F>,
    cwd: String,
    own: Vec<String>,
}

impl<F: FS> tsrs_tsoptions::ParseConfigHost for ConfigHost<F> {
    fn fs(&self) -> &dyn FS {
        &self.fs
    }

    fn get_current_directory(&self) -> &str {
        &self.cwd
    }
}

/// Root files of `tsconfig` as `tsc` computes them, the content-mapper
/// extensions it declares, the configs it extends, and its errors.
fn read_project(tsconfig: &str) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    let region = tsrs_core::arena::Region::new(64 << 10);
    let _guard = region.enter();
    let cwd = Path::new(tsconfig).parent().filter(|dir| !dir.as_os_str().is_empty()).map_or_else(
        || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        |dir| {
            if dir.is_absolute() {
                dir.to_path_buf()
            } else {
                std::env::current_dir().map_or_else(|_| dir.to_path_buf(), |base| base.join(dir))
            }
        },
    );
    let cwd = tspath::normalize_path(&cwd.display().to_string());
    let fs = SelfMapperFs::new(tsrs_vfs::bundled::wrap_fs(tsrs_vfs::osvfs::fs()), &cwd);
    let host = ConfigHost { fs, cwd, own: crate::config::own_extensions(Path::new(tsconfig)) };
    // The project is the content mapper: its code is already running, so the
    // `--runExternalCode` gate (and its diagnostic) does not apply.
    let options = tsrs_core::CompilerOptions {
        run_external_code: tsrs_core::Tristate::True,
        ..Default::default()
    };
    let (parsed, errors) = tsrs_tsoptions::get_parsed_command_line_of_config_file(
        tsconfig,
        Some(&options),
        None,
        &host,
        None,
    );
    let mut messages: Vec<String> = errors.iter().map(|item| item.localize()).collect();
    let Some(parsed) = parsed else {
        return (Vec::new(), Vec::new(), vec![tsconfig.to_owned()], messages);
    };
    messages
        .extend(parsed.get_config_file_parsing_diagnostics().iter().map(|item| item.localize()));
    let files = parsed
        .file_names()
        .iter()
        .map(|name| normalize(name))
        .filter(|name| !is_declaration(name) && !name.contains("/node_modules/"))
        .collect();
    let mut configs = vec![normalize(tsconfig)];
    configs.extend(parsed.extended_source_files().iter().map(|name| normalize(name)));
    let mut extensions = parsed.content_mapper_extensions();
    for ext in &host.own {
        if !extensions.iter().any(|known| known.eq_ignore_ascii_case(ext)) {
            extensions.push(ext.clone());
        }
    }
    (files, extensions, configs, messages)
}

#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "tsc matches extensions case-sensitively"
)]
fn is_declaration(path: &str) -> bool {
    [".d.ts", ".d.mts", ".d.cts"].iter().any(|suffix| path.ends_with(suffix))
        || path.contains(".d.") && path.ends_with(".ts")
}

/// `ts`, `tsx`, `js` or `jsx`: how a bundler parses the rewritten text.
#[must_use]
#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "tsc matches extensions case-sensitively"
)]
pub fn module_type(path: &str) -> &'static str {
    let path = normalize(path);
    if path.ends_with(".stsx") || path.ends_with(".tsx") {
        "tsx"
    } else if path.ends_with(".jsx") {
        "jsx"
    } else if path.ends_with(".js") || path.ends_with(".mjs") || path.ends_with(".cjs") {
        "js"
    } else {
        "ts"
    }
}

impl Project {
    fn new(tsconfig: String, explicit: Options) -> Result<Self, String> {
        let project = Self { tsconfig, explicit, state: Mutex::new(State::default()) };
        project.reload()?;
        Ok(project)
    }

    fn reload(&self) -> Result<(), String> {
        let base = config::read_tsconfig_options(Path::new(&self.tsconfig))?;
        let options = base.merged(&self.explicit);
        let (files, extensions, configs, errors) = read_project(&self.tsconfig);
        let mut state = self.state.lock().map_err(|err| err.to_string())?;
        state.session = Some(TransformSession::from_options(&options));
        state.options = options;
        state.files = files.into_iter().collect();
        state.extensions = extensions;
        state.configs = configs;
        state.config_errors = errors;
        state.cache.clear();
        state.dependents.clear();
        Ok(())
    }

    /// Whether `path` is a root file of the project (so the integration
    /// serves it).
    #[must_use]
    pub fn owns(&self, path: &str) -> bool {
        let path = normalize(path);
        self.state.lock().is_ok_and(|state| state.files.contains(&path))
    }

    /// The owned files, sorted.
    #[must_use]
    pub fn files(&self) -> Vec<String> {
        self.state.lock().map_or_default(|state| state.files.iter().cloned().collect())
    }

    /// Errors reading the tsconfig (an empty list when it parsed).
    #[must_use]
    pub fn config_errors(&self) -> Vec<String> {
        self.state.lock().map_or_default(|state| state.config_errors.clone())
    }

    /// The content-mapper extensions the tsconfig declares (`.sts`, …).
    #[must_use]
    pub fn extensions(&self) -> Vec<String> {
        self.state.lock().map_or_default(|state| state.extensions.clone())
    }

    /// Return the rewrite of `path` as JSON (`transformNative` shape plus `moduleType`).
    ///
    /// `text` is the file's content, read from disk when absent. A repeated
    /// request with the same text returns the cached answer.
    ///
    /// # Errors
    /// `path` is not owned ([`NOT_OWNED`]), is unsupported, or cannot be read.
    pub fn get_file(&self, path: &str, text: Option<String>) -> Result<Arc<str>, String> {
        let path = normalize(path);
        if let Some(message) = crate::unsupported_source(&path) {
            return Err(message);
        }
        let session = {
            let state = self.state.lock().map_err(|err| err.to_string())?;
            if !state.files.contains(&path) {
                return Err(format!("{path}: {NOT_OWNED} by {}", self.tsconfig));
            }
            state.session.clone().unwrap_or_default()
        };
        let text = match text {
            Some(text) => text,
            None => std::fs::read_to_string(&path).map_err(|err| format!("{path}: {err}"))?,
        };
        let text_hash = hash_text(&text);
        if let Ok(state) = self.state.lock()
            && let Some(cached) = state.cache.get(&path)
            && cached.text_hash == text_hash
        {
            return Ok(Arc::clone(&cached.json));
        }
        let result = session.transform(&path, &text);
        let mut json = crate::answer::result_json(&path, &text, &result, Some(&self.tsconfig))?;
        if let Some(object) = json.as_object_mut() {
            object.insert("moduleType".to_owned(), module_type(&path).into());
        }
        let json: Arc<str> = Arc::from(json.to_string());
        let dependencies: Vec<String> =
            result.dependencies.iter().map(|dep| normalize(dep)).collect();
        let mut state = self.state.lock().map_err(|err| err.to_string())?;
        if let Some(old) = state.cache.get(&path) {
            for dependency in old.dependencies.clone() {
                if let Some(set) = state.dependents.get_mut(&dependency) {
                    set.remove(&path);
                }
            }
        }
        for dependency in &dependencies {
            state.dependents.entry(dependency.clone()).or_default().insert(path.clone());
        }
        state.cache.insert(path, Cached { text_hash, json: Arc::clone(&json), dependencies });
        Ok(json)
    }

    /// Invalidate `path` after a change, addition or removal.
    ///
    /// Answers the owned files whose rewrite must be loaded again: `path`
    /// itself when owned, every file that read it (a Sweetener macro module),
    /// and every file when it is the tsconfig or a config it extends (options
    /// and the include list are read again). A new file under the include list
    /// becomes owned.
    ///
    /// # Errors
    /// The tsconfig cannot be read again.
    pub fn invalidate(&self, path: &str) -> Result<Vec<String>, String> {
        let path = normalize(path);
        let is_config = self.state.lock().is_ok_and(|state| state.configs.contains(&path));
        if is_config {
            self.reload()?;
            return Ok(self.files());
        }
        let mut affected = BTreeSet::new();
        let mut state = self.state.lock().map_err(|err| err.to_string())?;
        let known = state.files.contains(&path);
        state.cache.remove(&path);
        if known {
            affected.insert(path.clone());
        }
        if let Some(dependents) = state.dependents.get(&path).cloned() {
            for file in dependents {
                state.cache.remove(&file);
                affected.insert(file);
            }
        }
        let candidate = !known
            && !path.contains("/node_modules/")
            && !is_declaration(&path)
            && (Path::new(&path).extension().is_some_and(|ext| {
                matches!(
                    ext.to_str(),
                    Some("ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs")
                )
            }) || state.extensions.iter().any(|ext| path.ends_with(ext.as_str())));
        drop(state);
        if candidate || (known && !Path::new(&path).exists()) {
            // A created or deleted file: the include list is read again.
            let (files, _, _, _) = read_project(&self.tsconfig);
            let mut state = self.state.lock().map_err(|err| err.to_string())?;
            state.files = files.into_iter().collect();
            if state.files.contains(&path) {
                affected.insert(path);
            }
        }
        Ok(affected.into_iter().collect())
    }
}

type Registry = Mutex<FxHashMap<String, (Arc<Project>, usize)>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(FxHashMap::default()))
}

fn key(tsconfig: &str, explicit: &str) -> String {
    format!("{tsconfig}\u{0}{explicit}")
}

/// The project of `tsconfig` under `explicit` options (JSON text, the
/// same object the `Transformer` takes). A second open of the same pair is
/// the same project; [`close`] once per open.
///
/// # Errors
/// The tsconfig cannot be read or holds invalid tsc-ext options.
pub fn open(
    tsconfig: &Path,
    explicit: Options,
    explicit_key: &str,
) -> Result<Arc<Project>, String> {
    let tsconfig = std::fs::canonicalize(tsconfig)
        .unwrap_or_else(|_| PathBuf::from(tsconfig))
        .display()
        .to_string();
    let tsconfig = normalize(&tsconfig);
    let id = key(&tsconfig, explicit_key);
    let mut registry = registry().lock().map_err(|err| err.to_string())?;
    if let Some((project, count)) = registry.get_mut(&id) {
        *count += 1;
        return Ok(Arc::clone(project));
    }
    let project = Arc::new(Project::new(tsconfig.clone(), explicit)?);
    registry.insert(id, (Arc::clone(&project), 1));
    drop(registry);
    let _ = crate::prewarm::prewarm_in_background(&tsconfig);
    Ok(project)
}

/// Drop one open of `project`. The last one unregisters it.
pub fn close(project: &Arc<Project>, explicit_key: &str) {
    let id = key(&project.tsconfig, explicit_key);
    if let Ok(mut registry) = registry().lock()
        && let Some((_, count)) = registry.get_mut(&id)
    {
        *count -= 1;
        if *count == 0 {
            registry.remove(&id);
        }
    }
}
