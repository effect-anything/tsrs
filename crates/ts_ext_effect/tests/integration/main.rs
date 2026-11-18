#![allow(clippy::disallowed_types)]

use std::collections::HashMap;
use std::sync::Arc;

use tsrs_compiler::diagnosticwriter::{FormattingOptions, write_format_diagnostic};
use tsrs_compiler::{
    ProgramOptions, ProgramRegions, free_unshared_program, get_diagnostics_of_any_program,
    new_compiler_host, new_program, sort_and_deduplicate_diagnostics,
};
use tsrs_contentmapper::{
    Error as ContentMapperError, Mapper, OptionDiagnostic, Project, Request, TransformResultFiles,
};
use tsrs_core::arena::Region;
use tsrs_core::context::Context;
use tsrs_core::tspath::ComparePathsOptions;
use tsrs_core::{CompilerOptions, P, Tristate};
use tsrs_spanmap::{Feature, Kind, Segment};
use tsrs_tsoptions::{ParseConfigHost, get_parsed_command_line_of_config_file};
use tsrs_vfs::{FS, bundled, vfstest};

const EFFECT_PACKAGE_JSON: &str = r#"{
  "name": "effect",
  "version": "3.19.0",
  "types": "./index.d.ts"
}
"#;

const EFFECT_INDEX_D_TS: &str = r#"export declare const EffectTypeId: unique symbol;

export interface Variance<out A, out E, out R> {
  readonly _A: () => A;
  readonly _E: () => E;
  readonly _R: () => R;
}

export interface Effect<out A, out E = never, out R = never> {
  readonly [EffectTypeId]: Variance<A, E, R>;
}

export declare namespace Effect {
  export function succeed<A>(value: A): Effect<A>;
  export function runSync<A, E>(effect: Effect<A, E, never>): A;
}

export declare function pipe<A>(a: A): A;
export declare function pipe<A, B>(a: A, ab: (a: A) => B): B;
"#;

const TSC_MAPPER_PACKAGE_JSON: &str = r#"{
  "name": "@effect-x/tsc",
  "version": "0.1.0",
  "typescript": {
    "contentMapper": {
      "exec": ["./bin/tsc-ext"]
    }
  }
}
"#;

struct VfsConfigHost {
    fs: Arc<dyn FS>,
    cwd: String,
}

impl ParseConfigHost for VfsConfigHost {
    fn fs(&self) -> &dyn FS {
        &*self.fs
    }

    fn get_current_directory(&self) -> &str {
        &self.cwd
    }
}

struct SweetenerTestProject;

impl Project for SweetenerTestProject {
    fn refresh(&self) -> Result<(), ContentMapperError> {
        Ok(())
    }

    fn identities(&self) -> Result<Vec<String>, ContentMapperError> {
        Ok(vec!["@effect-x/tsc@0.1.0".to_string()])
    }

    fn identity(&self, _mapper: &Mapper) -> Result<String, ContentMapperError> {
        Ok("@effect-x/tsc@0.1.0".to_string())
    }

    fn watched_files(&self) -> Result<Vec<String>, ContentMapperError> {
        Ok(Vec::new())
    }

    fn diagnostics(&self) -> Vec<OptionDiagnostic> {
        Vec::new()
    }

    fn transform(
        &self,
        _mapper: &Mapper,
        request: Request<'_>,
    ) -> Result<TransformResultFiles, ContentMapperError> {
        let expanded = ts_ext_syntax::expand(ts_ext_syntax::ExpandRequest {
            filename: request.file_name,
            code: request.content,
            phase: ts_ext_syntax::Phase::Check,
        });
        let segments: Vec<Segment> = expanded
            .span_map_segments()
            .into_iter()
            .map(|(v_start, v_end, o_start, o_end, kind)| Segment {
                virtual_start: v_start as u32,
                virtual_end: v_end as u32,
                original_start: o_start as u32,
                original_end: o_end as u32,
                kind: Kind(kind),
                features: Feature::All,
            })
            .collect();
        Ok(TransformResultFiles {
            text: expanded.code,
            virtual_extension: ts_ext_syntax::session::virtual_extension(request.file_name)
                .to_string(),
            mappings: Some(tsrs_spanmap::new(&segments)),
            diagnostic_directives: Vec::new(),
            diagnostics: Vec::new(),
            supplemental: Vec::new(),
        })
    }

    fn close(&self) -> Result<(), ContentMapperError> {
        Ok(())
    }
}

fn check_project(files: &[(&str, &str)], with_sweetener: bool) -> Vec<String> {
    let mut map: HashMap<String, String> = HashMap::new();
    map.insert(
        "/app/node_modules/effect/package.json".to_string(),
        EFFECT_PACKAGE_JSON.to_string(),
    );
    map.insert("/app/node_modules/effect/index.d.ts".to_string(), EFFECT_INDEX_D_TS.to_string());
    if with_sweetener {
        map.insert(
            "/app/node_modules/@effect-x/tsc/package.json".to_string(),
            TSC_MAPPER_PACKAGE_JSON.to_string(),
        );
        map.insert("/app/node_modules/@effect-x/tsc/bin/tsc-ext".to_string(), String::new());
    }
    for (path, content) in files {
        map.insert((*path).to_string(), (*content).to_string());
    }

    let raw_fs: Arc<dyn FS> = Arc::new(vfstest::from_map(&map, true));
    let fs: Arc<dyn FS> = Arc::new(bundled::wrap_fs(raw_fs));

    let region = Region::new_scratch(1 << 20);
    let _scope = region.enter();
    let loader_regions = Arc::new(ProgramRegions::default());

    let cli_options = if with_sweetener {
        Some(CompilerOptions { run_external_code: Tristate::True, ..Default::default() })
    } else {
        None
    };

    let (parsed, config_errors) = get_parsed_command_line_of_config_file(
        "/app/tsconfig.json",
        cli_options.as_ref(),
        None,
        &VfsConfigHost { fs: Arc::clone(&fs), cwd: "/app".to_string() },
        None,
    );
    assert!(config_errors.is_empty(), "unexpected tsconfig parse errors: {config_errors:?}");
    let config = P::new(parsed.expect("parsed tsconfig"));

    let cm_project: Option<Arc<dyn Project>> =
        if with_sweetener { Some(Arc::new(SweetenerTestProject)) } else { None };

    let host = new_compiler_host(
        "/app",
        Arc::clone(&fs),
        &bundled::default_lib_path(),
        None,
        None,
        cm_project,
    );
    let mut program_options = ProgramOptions::new(config, host);
    program_options.regions = Some(loader_regions);
    program_options.checker_regions = true;

    let program = new_program(program_options);
    let ctx = Context::background();
    let mut found = program.get_config_file_parsing_diagnostics().to_vec();
    found.extend(get_diagnostics_of_any_program(
        &ctx,
        program,
        None,
        false,
        &mut |ctx, file| program.get_bind_diagnostics(ctx, file),
        &mut |ctx, file| program.get_semantic_diagnostics(ctx, file),
    ));

    let format_opts = FormattingOptions {
        compare_paths_options: ComparePathsOptions {
            current_directory: "/app".to_string(),
            use_case_sensitive_file_names: true,
        },
        new_line: "\n".to_string(),
    };
    let formatted: Vec<String> = sort_and_deduplicate_diagnostics(&found)
        .iter()
        .map(|&diagnostic| {
            let mut out = Vec::new();
            write_format_diagnostic(&mut out, diagnostic, &format_opts);
            String::from_utf8_lossy(&out).trim_end().to_string()
        })
        .collect();

    unsafe {
        free_unshared_program(program);
    }

    formatted
}

#[test]
fn without_plugin_no_effect_diagnostics_are_reported() {
    let diagnostics = check_project(
        &[
            (
                "/app/tsconfig.json",
                r#"{
  "compilerOptions": {
    "target": "es2022",
    "module": "esnext",
    "moduleResolution": "bundler",
    "strict": true,
    "skipLibCheck": true,
    "noEmit": true
  },
  "include": ["src"]
}"#,
            ),
            ("/app/src/index.ts", "import { Effect } from \"effect\";\nEffect.succeed(1);\n"),
        ],
        false,
    );
    assert!(
        diagnostics.is_empty(),
        "expected no diagnostics without @effect/language-service plugin, got: {diagnostics:#?}"
    );
}

#[test]
fn with_plugin_ts_diagnostics_directives_overrides_and_config_diagnostics() {
    let diagnostics = check_project(
        &[
            (
                "/app/tsconfig.base.json",
                r#"{
  "compilerOptions": {
    "target": "es2022",
    "module": "esnext",
    "moduleResolution": "bundler",
    "strict": true,
    "skipLibCheck": true,
    "noEmit": true,
    "plugins": [
      {
        "name": "@effect/language-service",
        "diagnosticSeverity": {
          "notARealEffectRule": "error"
        },
        "overrides": [
          {
            "include": ["src/ignored/**/*"],
            "options": {
              "diagnosticSeverity": {
                "floatingEffect": "off"
              }
            }
          }
        ]
      }
    ]
  }
}"#,
            ),
            (
                "/app/tsconfig.json",
                r#"{
  "extends": "./tsconfig.base.json",
  "compilerOptions": {
    "plugins": [
      {
        "name": "@effect/language-service",
        "diagnosticSeverity": {
          "localUnknownRule": "warning"
        }
      }
    ]
  },
  "include": ["src"]
}"#,
            ),
            (
                "/app/src/index.ts",
                concat!(
                    "import { Effect } from \"effect\";\n",
                    "Effect.succeed(1);\n",
                    "// @effect-diagnostics-next-line floatingEffect:off\n",
                    "Effect.succeed(2);\n",
                    "// @effect-diagnostics-next-line floatingEffect:off\n",
                    "export const used = Effect.runSync(Effect.succeed(3));\n",
                ),
            ),
            ("/app/src/ignored/ok.ts", "import { Effect } from \"effect\";\nEffect.succeed(99);\n"),
        ],
        false,
    );

    assert!(
        diagnostics.iter().any(|d| { d.contains("TS377134") && d.contains("notARealEffectRule") }),
        "expected inherited TS377134 unknownRuleName for notARealEffectRule, got: {diagnostics:#?}"
    );
    assert!(
        diagnostics.iter().any(|d| {
            d.starts_with("tsconfig.json(")
                && d.contains("TS377134")
                && d.contains("localUnknownRule")
        }),
        "expected TS377134 unknownRuleName located on tsconfig.json, got: {diagnostics:#?}"
    );
    assert!(
        diagnostics.iter().any(|d| {
            d.starts_with("src/index.ts(2,1): error TS377001:")
                && d.contains("effect(floatingEffect)")
        }),
        "expected TS377001 floatingEffect on line 2 of src/index.ts, got: {diagnostics:#?}"
    );
    assert!(
        !diagnostics.iter().any(|d| d.starts_with("src/index.ts(4,")),
        "expected line 4 floatingEffect to be suppressed by next-line directive, got: {diagnostics:#?}"
    );
    assert!(
        diagnostics.iter().any(|d| {
            d.starts_with("src/index.ts(5,4):")
                && d.contains("TS377000")
                && d.contains("effect(unusedDirective)")
        }),
        "expected TS377000 unusedDirective on line 5 of src/index.ts, got: {diagnostics:#?}"
    );
    assert!(
        !diagnostics.iter().any(|d| d.contains("src/ignored/ok.ts")),
        "expected src/ignored/ok.ts to have floatingEffect disabled via overrides, got: {diagnostics:#?}"
    );
}

#[test]
fn sweetener_sts_files_report_mapped_effect_diagnostics_and_directives() {
    let diagnostics = check_project(
        &[
            (
                "/app/tsconfig.json",
                r#"{
  "contentMappers": [
    {
      "package": "@effect-x/tsc",
      "extensions": [".sts", ".stsx"]
    }
  ],
  "compilerOptions": {
    "target": "es2022",
    "module": "esnext",
    "moduleResolution": "bundler",
    "strict": true,
    "skipLibCheck": true,
    "noEmit": true,
    "plugins": [
      {
        "name": "@effect/language-service",
        "overrides": [
          {
            "include": ["src/ignored/**/*"],
            "options": {
              "diagnosticSeverity": {
                "floatingEffect": "off"
              }
            }
          }
        ]
      }
    ]
  },
  "include": ["src"]
}"#,
            ),
            (
                "/app/src/app.sts",
                concat!(
                    "import { Effect } from \"effect\";\n", // line 1
                    "syntax makeFloating:expr {\n",         // line 2
                    "  rule { makeFloating() } => { Effect.succeed(1) }\n", // line 3
                    "}\n",                                  // line 4
                    "makeFloating();\n",                    // line 5 -> TS377001 mapped here!
                    "// @effect-diagnostics-next-line floatingEffect:off\n", // line 6 -> suppresses line 7!
                    "makeFloating();\n", // line 7 -> suppressed!
                    "// @effect-diagnostics-next-line floatingEffect:off\n", // line 8 -> TS377000 unusedDirective!
                    "export const ok = Effect.runSync(Effect.succeed(42));\n", // line 9
                ),
            ),
            (
                "/app/src/ignored/skip.sts",
                concat!(
                    "import { Effect } from \"effect\";\n",
                    "syntax makeFloating:expr {\n",
                    "  rule { makeFloating() } => { Effect.succeed(9) }\n",
                    "}\n",
                    "makeFloating();\n",
                ),
            ),
        ],
        true,
    );

    assert!(
        diagnostics.iter().any(|d| {
            d.starts_with("src/app.sts(5,1): error TS377001:")
                && d.contains("effect(floatingEffect)")
        }),
        "expected TS377001 mapped back to src/app.sts(5,1), got: {diagnostics:#?}"
    );
    assert!(
        !diagnostics.iter().any(|d| d.starts_with("src/app.sts(7,")),
        "expected macro call on src/app.sts line 7 to be suppressed by .sts directive on line 6, got: {diagnostics:#?}"
    );
    assert!(
        diagnostics.iter().any(|d| {
            d.starts_with("src/app.sts(8,4):")
                && d.contains("TS377000")
                && d.contains("effect(unusedDirective)")
        }),
        "expected TS377000 unusedDirective at .sts comment line 8, got: {diagnostics:#?}"
    );
    assert!(
        !diagnostics.iter().any(|d| d.contains("src/ignored/skip.sts")),
        "expected .sts file under overrides glob to suppress floatingEffect, got: {diagnostics:#?}"
    );
}
