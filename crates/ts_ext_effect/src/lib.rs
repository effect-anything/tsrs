//! Native Effect v3/v4 semantic diagnostics integrated into `tsrs`.
//!
//! Adapted from `yielded-dev/effect-tsrs` (MIT License) with content-mapper
//! (`.sts` / `.stsx`) span and directive support and pure VFS resolution.

#![allow(clippy::all, clippy::pedantic)]

mod config;
mod registry;
mod rules;
pub(crate) mod semantic;

use std::sync::OnceLock;

use serde::Serialize;
use tsrs_ast::{Diagnostic, SourceFile};
use tsrs_checker::Checker;
use tsrs_core::{P, TextRange};
use tsrs_diagnostics::Category;
use tsrs_tsoptions::ParsedCommandLine;
use tsrs_vfs::FS;

pub use registry::Rule;
use semantic::PackageCache;

/// Host interface implemented by `tsrs_compiler::Program` so `ts_ext_effect`
/// stays downstream of `tsrs_checker` and upstream of `tsrs_compiler`.
pub trait EffectProgramHost: tsrs_checker::Program {
    fn fs(&self) -> &dyn FS;
    fn command_line(&self) -> P<ParsedCommandLine>;
    fn is_source_file_from_external_library(&self, file: P<SourceFile>) -> bool;
    fn get_nearest_ancestor_directory_with_package_json(&self, directory: &str) -> String;
}

pub(crate) struct EffectVersion {
    pub major: u8,
    pub exact: Option<String>,
}

impl EffectVersion {
    pub fn supported(&self) -> u8 {
        if self.major == 4 { 4 } else { 3 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Message,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Message => "message",
        }
    }

    pub fn category(self) -> Category {
        match self {
            Self::Error => Category::Error,
            Self::Warning => Category::Warning,
            Self::Message => Category::Suggestion,
        }
    }
}

pub(crate) fn original_source_file_name(file: P<SourceFile>) -> &'static str {
    let name = file.get().original_file_name();
    if let Some(base) = name.strip_suffix(".sts.ts") {
        &name[..base.len() + ".sts".len()]
    } else if let Some(base) = name.strip_suffix(".stsx.tsx") {
        &name[..base.len() + ".stsx".len()]
    } else {
        name
    }
}

struct ActiveEffectState {
    configuration: config::Configuration,
    packages: OnceLock<PackageCache>,
    effect_version: OnceLock<EffectVersion>,
}

impl ActiveEffectState {
    fn packages(&self, program: &dyn EffectProgramHost) -> &PackageCache {
        self.packages.get_or_init(|| PackageCache::new(program))
    }

    fn effect_version(
        &self,
        program: &dyn EffectProgramHost,
        packages: &PackageCache,
    ) -> &EffectVersion {
        self.effect_version.get_or_init(|| detect_effect_version(program, packages))
    }
}

pub struct ProgramEffectState {
    inner: Option<ActiveEffectState>,
}

impl ProgramEffectState {
    pub fn new(program: &dyn EffectProgramHost) -> Self {
        let cwd = program.get_current_directory();
        let command_line = program.command_line();
        let Ok(configuration) = config::resolve(&command_line, program.fs(), &cwd, None, None)
        else {
            return Self { inner: None };
        };
        if !configuration.has_plugin() && configuration.diagnostics().is_empty() {
            return Self { inner: None };
        }
        Self {
            inner: Some(ActiveEffectState {
                configuration,
                packages: OnceLock::new(),
                effect_version: OnceLock::new(),
            }),
        }
    }

    pub fn has_plugin(&self) -> bool {
        self.inner.as_ref().is_some_and(|active| active.configuration.has_plugin())
    }

    pub fn config_diagnostics(&self) -> Vec<P<Diagnostic>> {
        let Some(active) = &self.inner else {
            return Vec::new();
        };
        let config_diags = active.configuration.diagnostics();
        if config_diags.is_empty() {
            return Vec::new();
        }
        let _outer = tsrs_core::arena::escape_for_diagnostic();
        config_diags
            .iter()
            .map(|diagnostic| {
                let message = format!("{} effect({})", diagnostic.message, diagnostic.name);
                tsrs_ast::new_diagnostic_from_text(
                    diagnostic.file,
                    diagnostic.range.unwrap_or_else(TextRange::synthetic),
                    diagnostic.code,
                    diagnostic.severity.category(),
                    &message,
                    &[],
                    &[],
                    false,
                    false,
                )
            })
            .collect()
    }

    pub fn check_file(
        &self,
        program: &dyn EffectProgramHost,
        checker: &mut Checker,
        file: P<SourceFile>,
    ) -> Vec<P<Diagnostic>> {
        let Some(active) = &self.inner else {
            return Vec::new();
        };
        if !active.configuration.has_plugin()
            || file.is_declaration_file()
            || program.is_source_file_from_external_library(file)
        {
            return Vec::new();
        }
        let mut file_config = active.configuration.for_file(file);
        let mut staged: Vec<StagedDiagnostic> = Vec::new();
        if !file_config.enabled.is_empty() {
            let packages = active.packages(program);
            let effect_version = active.effect_version(program, packages);
            let found = rules::analyze_file(
                checker,
                program,
                file,
                &file_config.enabled,
                &file_config.options,
                effect_version,
                packages,
            );
            for item in found {
                let Some(severity) = file_config.severity(item.rule, item.range) else {
                    continue;
                };
                staged.push(StagedDiagnostic {
                    file: item.file,
                    range: item.range,
                    is_original_range: false,
                    severity,
                    code: item.code,
                    name: item.rule.name(),
                    message: item.message,
                    related: item
                        .related
                        .into_iter()
                        .map(|rel| StagedRelated {
                            file: rel.file,
                            range: rel.range,
                            code: rel.code,
                            message: rel.message,
                        })
                        .collect(),
                });
            }
        }
        for diagnostic in file_config.finish() {
            let (Some(diag_file), Some(range)) = (diagnostic.file, diagnostic.range) else {
                continue;
            };
            staged.push(StagedDiagnostic {
                file: diag_file,
                range,
                is_original_range: diagnostic.is_original_range,
                severity: diagnostic.severity,
                code: diagnostic.code,
                name: diagnostic.name,
                message: diagnostic.message,
                related: Vec::new(),
            });
        }
        if staged.is_empty() {
            return Vec::new();
        }
        staged.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
        staged.dedup_by(|later, earlier| {
            if later.sort_key() != earlier.sort_key() {
                return false;
            }
            earlier.related.append(&mut later.related);
            earlier.related.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
            earlier.related.dedup_by(|a, b| a.sort_key() == b.sort_key());
            true
        });

        let _outer = tsrs_core::arena::escape_for_diagnostic();
        staged
            .into_iter()
            .map(|diag| {
                let full_message = format!("{} effect({})", diag.message, diag.name);
                if diag.is_original_range {
                    tsrs_ast::new_external_diagnostic(
                        Some(diag.file),
                        diag.range,
                        "TS",
                        diag.severity.category(),
                        diag.code,
                        &full_message,
                    )
                } else {
                    let related: Vec<P<Diagnostic>> = diag
                        .related
                        .into_iter()
                        .map(|rel| {
                            tsrs_ast::new_diagnostic_from_text(
                                Some(rel.file),
                                rel.range,
                                rel.code,
                                Category::Message,
                                &rel.message,
                                &[],
                                &[],
                                false,
                                false,
                            )
                        })
                        .collect();
                    tsrs_ast::new_diagnostic_from_text(
                        Some(diag.file),
                        diag.range,
                        diag.code,
                        diag.severity.category(),
                        &full_message,
                        &[],
                        &related,
                        false,
                        false,
                    )
                }
            })
            .collect()
    }
}

struct StagedRelated {
    file: P<SourceFile>,
    range: TextRange,
    code: i32,
    message: String,
}

impl StagedRelated {
    fn sort_key(&self) -> (&str, Option<u32>, Option<u32>, i32, &str) {
        (
            self.file.file_name(),
            self.range.source_pos(),
            self.range.source_end(),
            self.code,
            &self.message,
        )
    }
}

struct StagedDiagnostic {
    file: P<SourceFile>,
    range: TextRange,
    is_original_range: bool,
    severity: Severity,
    code: i32,
    name: &'static str,
    message: String,
    related: Vec<StagedRelated>,
}

impl StagedDiagnostic {
    fn sort_key(
        &self,
    ) -> (&str, bool, Option<u32>, Option<u32>, i32, Severity, &'static str, &str) {
        (
            self.file.file_name(),
            self.is_original_range,
            self.range.source_pos(),
            self.range.source_end(),
            self.code,
            self.severity,
            self.name,
            &self.message,
        )
    }
}

fn detect_effect_version(
    program: &dyn EffectProgramHost,
    packages: &PackageCache,
) -> EffectVersion {
    let mut detected = None;
    let mut exact = None;
    let mut conflicting_versions = false;
    for &file in program.source_files() {
        let Some(package) = packages.get(file) else {
            continue;
        };
        if package["name"] != "effect" {
            continue;
        }
        let major = match package["version"].as_str().and_then(|version| version.as_bytes().first())
        {
            Some(b'3') => 3,
            Some(b'4') => 4,
            _ => 0,
        };
        detected = Some(if detected.is_some_and(|version| version != major) { 0 } else { major });
        if let Some(version) = package["version"].as_str() {
            if exact.as_deref().is_some_and(|previous| previous != version) {
                conflicting_versions = true;
            } else {
                exact = Some(version.to_string());
            }
        }
    }
    EffectVersion {
        major: detected.unwrap_or(0),
        exact: if conflicting_versions { None } else { exact },
    }
}
