//! `tsrs --watch` with the real `tsc-ext` mapper: an edit of a `for syntax`
//! macro module expands and checks again the files that import it.
//!
//! Two layouts: the macro module outside the program (`files` names only
//! the importer), which the watcher must still watch, and inside it. In
//! both, the importer's text does not change, so its incremental version
//! must cover its expansion, not only its text.
//!
//! The watch backend is a stub that routes the events the test sends, and
//! each cycle runs on `do_cycle`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tsrs_core::tspath;
use tsrs_execute::execute::command_line_with_testing;
use tsrs_execute::tsc::{CommandLineTesting, ReadWriteCloser, SyncWriter, System};
use tsrs_execute::watchmanager::{WatchBackend, WatchDirectoryRequest};
use tsrs_fswatch::{self as fswatch, Event, EventKind};
use tsrs_vfs::{FS, bundled, osvfs};

#[derive(Default)]
struct Backend {
    watches: Mutex<Vec<(String, bool, fswatch::WatchCallback)>>,
}

struct Closer;

impl fswatch::Watch for Closer {
    fn close(&self) -> Result<(), fswatch::Error> {
        Ok(())
    }
}

impl WatchBackend for Backend {
    fn watch_directories(
        &self,
        requests: Vec<WatchDirectoryRequest>,
    ) -> Result<Vec<Box<dyn fswatch::Watch>>, fswatch::Error> {
        let mut watches = self.watches.lock().unwrap();
        Ok(requests
            .into_iter()
            .map(|request| {
                watches.push((request.dir, request.recursive, request.callback));
                Box::new(Closer) as Box<dyn fswatch::Watch>
            })
            .collect())
    }
}

impl Backend {
    /// An update of `file`, through every watch of a directory above it.
    /// Returns whether any watch covered it.
    fn update(&self, file: &Path) -> bool {
        let path = file.to_string_lossy().into_owned();
        let parent = file.parent().unwrap().to_string_lossy().into_owned();
        let targets: Vec<fswatch::WatchCallback> = self
            .watches
            .lock()
            .unwrap()
            .iter()
            .filter(|(dir, recursive, _)| {
                parent == *dir || (*recursive && parent.starts_with(&format!("{dir}/")))
            })
            .map(|(_, _, callback)| Arc::clone(callback))
            .collect();
        for callback in &targets {
            callback(vec![Event::new(EventKind::UPDATE, path.clone())], None);
        }
        !targets.is_empty()
    }
}

struct TestSys {
    cwd: String,
    fs: Arc<dyn FS>,
    default_library_path: String,
    output: Arc<Mutex<String>>,
    start: Instant,
}

impl System for TestSys {
    fn fs(&self) -> Arc<dyn FS> {
        Arc::clone(&self.fs)
    }

    fn default_library_path(&self) -> &str {
        &self.default_library_path
    }

    fn get_current_directory(&self) -> &str {
        &self.cwd
    }

    fn write(&self, text: &str) {
        self.output.lock().unwrap().push_str(text);
    }

    fn flush(&self) {}

    fn write_output_is_tty(&self) -> bool {
        false
    }

    fn get_environment_variable(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn spawn(
        &self,
        command: &[String],
        dir: &str,
        stderr: Option<Box<dyn Write + Send>>,
    ) -> Result<ReadWriteCloser, String> {
        tsrs_contentmapper::spawn_process(command, dir, stderr)
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn since_start(&self) -> Duration {
        self.start.elapsed()
    }
}

struct Quiet {
    backend: Arc<Backend>,
}

impl CommandLineTesting for Quiet {
    fn watch_backend(&self) -> Option<Arc<dyn WatchBackend>> {
        Some(Arc::clone(&self.backend) as Arc<dyn WatchBackend>)
    }

    fn get_trace(
        &'static self,
        _w: SyncWriter,
        _is_sys_writer: bool,
    ) -> Box<tsrs_compiler::TraceFn> {
        Box::new(|_, _| {})
    }
}

const PAIR: &str = "export syntax twice:expr {\n  rule { twice($x:expr) } => { [$x, $x] }\n}\n";
const TRIPLE: &str =
    "export syntax twice:expr {\n  rule { twice($x:expr) } => { [$x, $x, $x] }\n}\n";
const APP: &str = "import { twice } from \"./macros/twice.sts\" for syntax;\nexport const pair: [number, number] = twice(1);\n";

fn write_file(root: &Path, name: &str, text: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Sets up a project in `root`, runs `tsrs --watch`, edits the macro module
/// and returns the output of the first build and of the cycle after the edit.
fn run(root: &Path, files: Option<&str>) -> (String, String, String) {
    let files = files.map_or_else(String::new, |files| format!("\"files\": {files},"));
    write_file(
        root,
        "tsconfig.json",
        &format!(
            r#"{{ {files} "compilerOptions": {{ "noEmit": true, "strict": true, "types": [] }},
  "contentMappers": [ {{ "package": "mapper", "extensions": [".sts"] }} ] }}"#
        ),
    );
    write_file(
        root,
        "node_modules/mapper/package.json",
        &format!(
            r#"{{ "name": "mapper", "version": "1.0.0", "typescript": {{ "contentMapper": {{ "exec": [{:?}] }} }} }}"#,
            env!("CARGO_BIN_EXE_tsc-ext")
        ),
    );
    write_file(root, "macros/twice.sts", PAIR);
    write_file(root, "app.sts", APP);

    let output = Arc::new(Mutex::new(String::new()));
    let take = || std::mem::take(&mut *output.lock().unwrap());
    let sys: &'static dyn System = Box::leak(Box::new(TestSys {
        cwd: tspath::normalize_path(&root.to_string_lossy()),
        fs: Arc::new(bundled::wrap_fs(osvfs::fs())),
        default_library_path: bundled::default_lib_path(),
        output: Arc::clone(&output),
        start: Instant::now(),
    }));
    let backend = Arc::new(Backend::default());
    let testing: &'static dyn CommandLineTesting =
        Box::leak(Box::new(Quiet { backend: Arc::clone(&backend) }));
    let args: Vec<String> = ["--watch", "--runExternalCode", "--preserveWatchOutput"]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect();
    let result = command_line_with_testing(sys, args, Some(testing));
    let mut watcher = result.watcher.expect("a watcher");
    let first = take();

    write_file(root, "macros/twice.sts", TRIPLE);
    assert!(backend.update(&root.join("macros/twice.sts")), "no watch covers the macro module");
    watcher.do_cycle();
    let edited = take();

    write_file(root, "macros/twice.sts", PAIR);
    backend.update(&root.join("macros/twice.sts"));
    watcher.do_cycle();
    let restored = take();
    drop(watcher);
    (first, edited, restored)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ts_ext_watch_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

#[test]
fn an_edited_macro_module_rechecks_its_importers() {
    let _guard = crate::E2E_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for (name, files) in [("outside", Some(r#"["app.sts"]"#)), ("inside", None)] {
        let root = scratch(name);
        let (first, edited, restored) = run(&root, files);
        assert!(!first.contains("error TS"), "{name}: first build:\n{first}");
        assert!(
            edited.contains("app.sts") && edited.contains("error TS2322"),
            "{name}: after the edit, app.sts must fail:\n{edited}"
        );
        assert!(!restored.contains("error TS"), "{name}: after the revert:\n{restored}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
