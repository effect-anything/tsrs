//! Native per-platform binary resolution for content mapper packages.
//!
//! Not in tsgo. A mapper package whose `exec` is one launcher (`./bin/<name>`)
//! may ship that program natively in a per-platform package,
//! `<package>-<platform>-<arch>/lib/<name>`, as npm packages with optional
//! platform dependencies do. When that file is installed it replaces `exec`,
//! so a mapper start does not pay for a Node process. Nothing in the manifest
//! names it: this is a host convention.
//!
//! `contentmappers.rs` calls [`exec`].

use tsrs_core::{RESOLUTION_MODE_NONE, tspath};
use tsrs_module::DefaultResolver;

use crate::tsconfigparsing::ParseConfigHost;

/// `exec`, or the installed native program that replaces it.
#[must_use]
pub fn exec(
    host: &dyn ParseConfigHost,
    resolver: &DefaultResolver<'_>,
    package_directory: &str,
    package_name: &str,
    exec: Vec<String>,
) -> Vec<String> {
    native_exec(host, resolver, package_directory, package_name, &exec)
        .map_or(exec, |path| vec![path])
}

/// Node's `process.platform` and `process.arch` for this build.
#[must_use]
pub fn node_platform() -> (&'static str, &'static str) {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        "x86" => "ia32",
        other => other,
    };
    (platform, arch)
}

/// Resolves the native program for a one-entry `exec` (`[".../<name>"]`).
///
/// Looks up `<package>-<platform>-<arch>/lib/<name>` (Node's spelling of
/// platform and arch), resolved from the mapper's package directory, when that
/// file exists; `.exe` is tried on Windows. `None` keeps `exec`.
#[must_use]
pub fn native_exec(
    host: &dyn ParseConfigHost,
    resolver: &DefaultResolver<'_>,
    package_directory: &str,
    package_name: &str,
    exec: &[String],
) -> Option<String> {
    let [launcher] = exec else {
        return None;
    };
    let program =
        launcher.rsplit(['/', '\\']).next().filter(|name| !name.is_empty() && *name != "..")?;
    let (platform, arch) = node_platform();
    let package = format!("{package_name}-{platform}-{arch}");
    let containing_file = tspath::combine_paths(package_directory, &["package.json"]);
    let resolved = resolver
        .resolve_package_directory(&package, &containing_file, RESOLUTION_MODE_NONE, None)
        .filter(|resolved| !resolved.resolved_file_name.is_empty())?;
    let path = tspath::combine_paths(resolved.resolved_file_name, &["lib", program]);
    let fs = host.fs();
    if fs.file_exists(&path) {
        return Some(path);
    }
    let exe = format!("{path}.exe");
    (platform == "win32" && fs.file_exists(&exe)).then_some(exe)
}
