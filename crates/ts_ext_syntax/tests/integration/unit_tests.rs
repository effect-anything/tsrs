//! Extracted inline unit tests from `crates/ts_ext_syntax/src/**`.

mod answer {
    use ts_ext_syntax::answer::{Positions, host_path, split_code};

    #[test]
    fn codes_paths_and_positions() {
        assert_eq!(split_code("SWR4025: unread"), (4025, "unread"));
        assert_eq!(split_code("plain"), (0, "plain"));
        assert_eq!(host_path("/a/b.stsx", ".tsx"), "/a/b.tsx");
        assert_eq!(host_path("/a/b.ts", ".ts"), "/a/b.ts");
        let positions = Positions::new("é\nab😀c");
        assert_eq!(positions.line_column(3), (2, 1));
        assert_eq!(positions.line_column(9), (2, 5));
        assert_eq!(positions.utf16(9), 6);
    }
}

mod compose {
    use serde_json::json;
    use ts_ext_syntax::compose::{compose, decode, parse_spans};

    #[test]
    fn verbatim_positions_map_exactly_and_introduced_text_is_unmapped() {
        // virtual "let a = 1;\nX;\nlet b = 2;" from source "let a = 1;\nlet b = 2;"
        let source = "let a = 1;\nlet b = 2;";
        let code = "let a = 1;\nX;\nlet b = 2;";
        let spans = parse_spans("[[0,11,0,11,0],[14,10,11,10,0]]").unwrap();
        // identity map of `code`: one segment per line start and at col 4.
        let later = json!({"version":3,"sources":["a.ts"],"names":[],
            "mappings":"AAAA,IAAI;AACJ;AACA,IAAI"});
        let map = compose("a.sts", "a.ts", source, code, &spans, &later).unwrap();
        assert_eq!(map["sources"], json!(["a.sts"]));
        let lines = decode(map["mappings"].as_str().unwrap()).unwrap();
        // line 0: (0 -> 0:0), (4 -> 0:4); line 1: unmapped; line 2: -> 1:0, 1:4
        assert_eq!(lines[0], vec![vec![0, 0, 0, 0], vec![4, 0, 0, 4]]);
        assert_eq!(lines[1], vec![vec![0]]);
        assert_eq!(lines[2], vec![vec![0, 0, 1, -4], vec![4, 0, 0, 4]]);
    }
}

mod config {
    use serde_json::json;
    use ts_ext_syntax::config::{Options, find_tsconfig, read_tsconfig_options, strip_jsonc};

    #[test]
    fn reads_categories_and_reports_unknown_keys() {
        let (options, errors) = Options::from_json(&json!({
            "sweetener": { "rewriteImports": true },
            "typegpu": { "autoNaming": false, "forceTgpuAlias": "gpu", "exclude": "x" },
            "other": 1
        }));
        assert_eq!(options.typegpu.auto_naming, Some(false));
        assert_eq!(options.sweetener.rewrite_imports, Some(true));
        assert_eq!(options.typegpu.force_tgpu_alias.as_deref(), Some("gpu"));
        let messages: Vec<String> = errors.iter().map(ToString::to_string).collect();
        assert_eq!(
            messages,
            ["options.other: unknown option category", "options.typegpu.exclude: unknown option"]
        );
    }

    #[test]
    fn explicit_fields_override_single_fields() {
        let (base, _) =
            Options::from_json(&json!({"typegpu": {"autoNaming": false, "earlyPruning": true}}));
        let (over, _) = Options::from_json(&json!({"typegpu": {"autoNaming": true}}));
        let merged = base.merged(&over);
        assert_eq!(merged.typegpu.auto_naming, Some(true));
        assert_eq!(merged.typegpu.early_pruning, Some(true));
    }

    #[test]
    fn jsonc_comments_and_trailing_commas() {
        let text = "{\n // c\n \"a\": \"//x,}\", /* b */ \"b\": [1,],\n}";
        let value: serde_json::Value = serde_json::from_str(&strip_jsonc(text)).unwrap();
        assert_eq!(value, json!({"a": "//x,}", "b": [1]}));
    }

    #[test]
    fn tsconfig_entry_and_extends() {
        let dir = std::env::temp_dir().join(format!("tsc-ext-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("base.json"),
            r#"{ "contentMappers": [{ "package": "@effect-x/tsc", "extensions": [".sts"],
                 "options": { "typegpu": { "earlyPruning": true } } }] }"#,
        )
        .unwrap();
        std::fs::write(dir.join("tsconfig.json"), r#"{ "extends": "./base", }"#).unwrap();
        let options = read_tsconfig_options(&dir.join("tsconfig.json")).unwrap();
        assert_eq!(options.typegpu.early_pruning, Some(true));
        assert_eq!(find_tsconfig(&dir.join("src/a.sts")), Some(dir.join("tsconfig.json")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

mod prewarm {
    use ts_ext_syntax::prewarm::{prewarm, project_files};

    #[test]
    fn project_files_skip_dependencies_and_outputs() {
        let root = std::env::temp_dir().join(format!("prewarm-files-{}", std::process::id()));
        for dir in ["src/deep", "node_modules/pkg", "dist", ".cache"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for file in [
            "src/a.sts",
            "src/deep/b.stsx",
            "src/c.ts",
            "node_modules/pkg/d.sts",
            "dist/e.sts",
            ".cache/f.sts",
        ] {
            std::fs::write(root.join(file), "const x = 1;\n").unwrap();
        }
        let found: Vec<String> = project_files(&root)
            .iter()
            .map(|path| path.strip_prefix(&root).unwrap().display().to_string())
            .collect();
        assert_eq!(found, vec!["src/a.sts", "src/deep/b.stsx"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A warmed file answers from the memo with exactly what a fresh
    /// expansion gives; an edit after warming is expanded again.
    #[test]
    fn prewarmed_expansion_equals_a_fresh_one() {
        let root = std::env::temp_dir().join(format!("prewarm-hit-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("m.sts"),
            "export syntax one:expr {\n  rule { one() } => { 1 }\n}\n",
        )
        .unwrap();
        let main = root.join("main.sts");
        let code = "import { one } from \"./m.sts\" for syntax;\nconst a = one();\n";
        std::fs::write(&main, code).unwrap();
        assert_eq!(prewarm(project_files(&root), 2), 2);
        let filename = main.display().to_string();
        let request = |code: &str| {
            ts_ext_syntax::expand(ts_ext_syntax::ExpandRequest {
                filename: &filename,
                code,
                phase: ts_ext_syntax::Phase::Check,
            })
        };
        let warmed = request(code);
        assert!(warmed.code.contains("const a = 1;"), "{}", warmed.code);
        ts_ext_sweetener::clear_caches();
        assert_eq!(warmed, request(code));
        let edited = request("import { one } from \"./m.sts\" for syntax;\nconst b = one();\n");
        assert!(edited.code.contains("const b = 1;"), "{}", edited.code);
        std::fs::remove_dir_all(&root).unwrap();
    }
}

mod project {
    use ts_ext_syntax::config::Options;
    use ts_ext_syntax::project::{close, module_type, open};

    fn fixture(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("tsc-ext-project-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src/macros")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/dep")).unwrap();
        std::fs::write(
            root.join("tsconfig.json"),
            r#"{ "compilerOptions": { "strict": true, "allowImportingTsExtensions": true, "noEmit": true },
                 "include": ["src"],
                 "contentMappers": [{ "package": "@effect-x/tsc", "extensions": [".sts", ".stsx"] }] }"#,
        )
        .unwrap();
        std::fs::write(
            root.join("src/macros/one.sts"),
            "export syntax one:expr {\n  rule { one() } => { 1 }\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/main.sts"),
            "import { one } from \"./macros/one.sts\" for syntax;\nexport const a = one();\n",
        )
        .unwrap();
        std::fs::write(root.join("src/plain.ts"), "export const b: number = 2;\n").unwrap();
        std::fs::write(root.join("src/types.d.ts"), "export {};\n").unwrap();
        std::fs::write(root.join("node_modules/dep/index.ts"), "export {};\n").unwrap();
        root
    }

    #[test]
    fn owns_reads_and_invalidates() {
        let root = fixture("owns");
        let project = open(&root.join("tsconfig.json"), Options::default(), "{}").unwrap();
        let canon = std::fs::canonicalize(&root).unwrap().display().to_string();
        let main = format!("{canon}/src/main.sts");
        let plain = format!("{canon}/src/plain.ts");
        let macros = format!("{canon}/src/macros/one.sts");
        assert!(project.owns(&main), "{:?}", project.files());
        assert!(project.owns(&plain));
        assert!(!project.owns(&format!("{canon}/src/types.d.ts")));
        assert!(!project.owns(&format!("{canon}/node_modules/dep/index.ts")));
        let answer: serde_json::Value =
            serde_json::from_str(&project.get_file(&main, None).unwrap()).unwrap();
        assert!(answer["code"].as_str().unwrap().contains("export const a = 1;"), "{answer}");
        assert_eq!(answer["moduleType"], "ts");
        assert!(
            answer["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .any(|dep| dep.as_str().unwrap().ends_with("macros/one.sts")),
            "{answer}"
        );
        // A macro module change reloads the file that read it.
        let affected = project.invalidate(&macros).unwrap();
        assert!(affected.contains(&main), "{affected:?}");
        // A new file under `include` becomes owned.
        let added = format!("{canon}/src/added.ts");
        std::fs::write(&added, "export {};\n").unwrap();
        assert!(!project.owns(&added));
        assert_eq!(project.invalidate(&added).unwrap(), vec![added.clone()]);
        assert!(project.owns(&added));
        // The same tsconfig and options is the same project.
        let again = open(&root.join("tsconfig.json"), Options::default(), "{}").unwrap();
        assert!(std::sync::Arc::ptr_eq(&project, &again));
        close(&again, "{}");
        close(&project, "{}");
        assert_eq!(module_type("/a/b.stsx"), "tsx");
        let _ = std::fs::remove_dir_all(&root);
    }
}

mod protocol {
    use std::fmt::Write as _;

    use ts_ext_syntax::protocol::{ExpansionRegion, expansion_regions, transform_file};

    /// The scan `expansion_regions` replaced: every segment per invocation.
    fn regions_by_scan(expansion: &ts_ext_syntax::Expansion) -> Vec<ExpansionRegion> {
        let mut out: Vec<ExpansionRegion> = Vec::new();
        for trace in &expansion.traces {
            let (mut low, mut high) = (u32::MAX, 0);
            for segment in &expansion.segments {
                if segment.original_start >= trace.start
                    && segment.original_end <= trace.end
                    && segment.virtual_end > segment.virtual_start
                {
                    low = low.min(segment.virtual_start);
                    high = high.max(segment.virtual_end);
                }
            }
            if low >= high {
                continue;
            }
            let mut label = format!("macro `{}`", trace.macro_name);
            if let Some(module) = &trace.module {
                let _ = write!(label, " from \"{module}\"");
            }
            let region = ExpansionRegion {
                virtual_start: i32::try_from(low).unwrap(),
                virtual_length: i32::try_from(high - low).unwrap(),
                original_start: i32::try_from(trace.start).unwrap(),
                original_length: i32::try_from(trace.end - trace.start).unwrap(),
                label,
            };
            if !out.contains(&region) {
                out.push(region);
            }
        }
        out
    }

    #[test]
    fn indexed_regions_equal_the_full_scan_on_every_case() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../ts_ext_sweetener/cases");
        let mut compared = 0;
        for dir in std::fs::read_dir(root).unwrap() {
            let dir = dir.unwrap().path();
            for name in ["input.sts", "input.stsx"] {
                let path = dir.join(name);
                let Ok(code) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let expansion = ts_ext_syntax::expand(ts_ext_syntax::ExpandRequest {
                    filename: &path.display().to_string(),
                    code: &code,
                    phase: ts_ext_syntax::Phase::Check,
                });
                assert_eq!(
                    expansion_regions(&expansion),
                    regions_by_scan(&expansion),
                    "{}",
                    path.display()
                );
                compared += usize::from(!expansion.traces.is_empty());
            }
        }
        assert!(compared > 50, "{compared}");
    }

    #[test]
    fn expansions_cover_the_output_of_each_invocation() {
        let code = "export syntax twice:expr {\n  rule { twice($x:expr) } => { [$x, $x] }\n}\nconst a = twice(value);\n";
        let response = transform_file("a.sts", code);
        assert_eq!(response.expansions.len(), 1, "{:?}", response.expansions);
        let region = &response.expansions[0];
        assert_eq!(region.label, "macro `twice`");
        let start = region.virtual_start as usize;
        let text = &response.text[start..start + region.virtual_length as usize];
        assert_eq!(text, "[value, value]");
        let original = region.original_start as usize;
        assert_eq!(&code[original..original + region.original_length as usize], "twice(value)");
    }

    #[test]
    fn mapper_does_not_rewrite_typegpu() {
        let code = "function f(a, b) { \"use gpu\"; return a + b }";
        let response = transform_file("a.ts", code);
        assert_eq!(response.text, code);
    }

    #[test]
    fn ordinary_ts_is_unchanged() {
        let code = "function f(a, b) { return a + b }";
        let response = transform_file("a.ts", code);
        assert_eq!(response.text, code);
        assert_eq!(response.extension, ".ts");
    }

    #[test]
    fn unread_item_is_a_warning_with_a_related_location() {
        let code = "const x = foo 1;\n";
        let response = transform_file("a.sts", code);
        assert_eq!(response.diagnostics.len(), 1);
        let item = &response.diagnostics[0];
        assert_eq!(item.category, 0);
        assert_eq!(item.code, 4025);
        assert_eq!(
            item.related,
            vec![("The reader expected ';' or automatic terminator here.".to_owned(), 14, 1)]
        );
    }
}

mod session {
    use ts_ext_syntax::TransformSession;
    use ts_ext_syntax::session::{compose, map_through};

    #[test]
    fn import_specifiers_keep_the_extension_unless_rewriting() {
        let code = "import { b } from \"./b.sts\";\nexport syntax twice:expr {\n  rule { twice($x:expr) } => { [$x, $x] }\n}\nexport const a = twice(b);\n";
        let kept = TransformSession::from_options(&ts_ext_syntax::config::Options::default())
            .transform("/p/a.sts", code);
        assert!(kept.code.contains("from \"./b.sts\""), "{}", kept.code);
        assert!(kept.code.contains("[b, b]"), "{}", kept.code);
        let mut options = ts_ext_syntax::config::Options::default();
        options.sweetener.rewrite_imports = Some(true);
        let rewritten = TransformSession::from_options(&options).transform("/p/a.sts", code);
        assert!(rewritten.code.contains("from \"./b\""), "{}", rewritten.code);
    }

    #[test]
    fn ranges_across_reordered_segments_cover_every_operand() {
        let verbatim = |vs, ve, os| ts_ext_syntax::Segment {
            virtual_start: vs,
            virtual_end: ve,
            original_start: os,
            original_end: os + (ve - vs),
            kind: ts_ext_syntax::SegmentKind::Verbatim,
        };
        // `[b, a]` printed from `pair(a, b)` (a at 5, b at 8).
        let first = ts_ext_syntax::Expansion {
            segments: vec![verbatim(1, 2, 8), verbatim(4, 5, 5)],
            ..Default::default()
        };
        let mapped = map_through(&first, 1, 5).unwrap();
        assert_eq!((mapped.start, mapped.end), (5, 9));
    }

    #[test]
    fn ordinary_file_runs_typegpu_only() {
        let code = "function f(a, b) { \"use gpu\"; return a + b }";
        let out = TransformSession::new().transform("a.ts", code);
        assert!(!out.changed);
        assert_eq!(out.code, code);
        assert!(!out.code.contains("__tsover_add"), "{}", out.code);
    }

    #[test]
    fn check_keeps_the_operator_and_build_emits_the_call() {
        let code = "function add(a, b) { \"use gpu\"; return a + b }";
        let check = TransformSession::check().transform("a.ts", code);
        assert_eq!(check.code, code);
        assert!(!check.code.contains("__TYPEGPU_META__"), "{}", check.code);
        let build = TransformSession::build().transform("a.ts", code);
        assert!(build.code.contains("__tsover_add"), "{}", build.code);
        assert!(build.code.contains("__TYPEGPU_META__"), "{}", build.code);
        assert!(build.code.contains("v: 2"), "{}", build.code);
    }

    #[test]
    fn plain_ts_is_unchanged() {
        let code = "export const n = 1\n";
        let out = TransformSession::new().transform("a.ts", code);
        assert!(!out.changed);
        assert_eq!(out.code, code);
    }

    #[test]
    fn sweetener_file_takes_the_macro_path() {
        let out = TransformSession::new().transform("a.sts", "export const n = 1\n");
        assert_eq!(out.extension, ".ts");
    }

    #[test]
    fn check_keeps_the_author_operator_in_place() {
        let code = "function step(n: number) {\n  \"use gpu\";\n  return n + 1;\n}\n";
        let out = TransformSession::check().transform("a.ts", code);
        assert_eq!(out.code, code);
        let plus = code.find('+').expect("plus") as i32;
        let covering = tuples(&out.mappings)
            .into_iter()
            .find(|item| item[0] <= plus && plus < item[0] + item[1])
            .expect("segment");
        assert_eq!(covering[4], 0, "check does not rewrite the operator");
        assert_eq!(covering[2] + (plus - covering[0]), plus);
        let name = out.code.find("function step").expect("name") as i32;
        let name_seg = tuples(&out.mappings)
            .into_iter()
            .find(|item| item[0] <= name && name < item[0] + item[1])
            .expect("name segment");
        assert_eq!(name_seg[4], 0);
        let from = name_seg[2] as usize;
        let to = from + name_seg[3] as usize;
        let virtual_from = name_seg[0] as usize;
        let virtual_to = virtual_from + name_seg[1] as usize;
        assert_eq!(&code[from..to], &out.code[virtual_from..virtual_to]);
    }

    #[test]
    fn macro_atom_composes_onto_the_author_span() {
        use ts_ext_syntax::{Expansion, Segment, SegmentKind};
        let head = "function step(n: number) {\n  \"use gpu\";\n  ";
        let author_macro = "unless (n < 0) { return n + 1 }";
        let virtual_macro = "if (!(n < 0)) { return n + 1 }";
        let tail = "\n  return kept;\n}\n";
        let author = format!("{head}{author_macro}{tail}");
        let virtual_text = format!("{head}{virtual_macro}{tail}");
        let head_len = head.len() as u32;
        let virtual_end = head_len + virtual_macro.len() as u32;
        let author_end = head_len + author_macro.len() as u32;
        let expansion = Expansion {
            code: virtual_text.clone(),
            segments: vec![
                Segment {
                    virtual_start: 0,
                    virtual_end: head_len,
                    original_start: 0,
                    original_end: head_len,
                    kind: SegmentKind::Verbatim,
                },
                Segment {
                    virtual_start: head_len,
                    virtual_end,
                    original_start: head_len,
                    original_end: author_end,
                    kind: SegmentKind::Atom,
                },
                Segment {
                    virtual_start: virtual_end,
                    virtual_end: virtual_text.len() as u32,
                    original_start: author_end,
                    original_end: author.len() as u32,
                    kind: SegmentKind::Verbatim,
                },
            ],
            diagnostics: Vec::new(),
            dependencies: Vec::new(),
            ..Default::default()
        };
        let rewritten = ts_ext_transform::tsover::rewrite_text("a.ts", &virtual_text).unwrap();
        let segments = compose(&expansion, &rewritten);
        let call = rewritten.code.find("__tsover_add").unwrap() as i32;
        let covering = segments.iter().find(|item| item.0 <= call && call < item.1).unwrap();
        assert_eq!(covering.4, 1);
        assert_eq!(covering.2, head_len as i32);
        assert_eq!(covering.3, author_end as i32);
        assert_eq!(&author[covering.2 as usize..covering.3 as usize], author_macro);
        let kept = rewritten.code.find("return kept").unwrap() as i32;
        let kept_seg = segments.iter().find(|item| item.0 <= kept && kept < item.1).unwrap();
        assert_eq!(kept_seg.4, 0);
        assert_eq!(
            &author[kept_seg.2 as usize..kept_seg.3 as usize],
            &rewritten.code[kept_seg.0 as usize..kept_seg.1 as usize]
        );
    }

    #[test]
    fn sts_specifiers_rewrite_imports_only() {
        let code = concat!(
            "import { a } from \"./logic.sts\";\n",
            "import type { T } from './types.stsx';\n",
            "import value = require(\"./value.sts\");\n",
            "export { b } from \"./panel.stsx\";\n",
            "export * from \"./all.sts\";\n",
            "const loaded = import(\"./dyn.sts\");\n",
            "const phase = import.source(\"./source.stsx\");\n",
            "type Box = import(\"./box.sts\").Box;\n",
            "const text = \"keep ./note.sts in a string\";\n",
            "const tick = `keep ./note.sts in a template`;\n",
            "const mixed = `head ${import(\"./inner.sts\")} tail ./note.sts`;\n",
            "// comment ./comment.sts must stay\n",
            "const query = import(\"./q.sts?x=1\");\n",
            "const plain = import(\"./already.ts\");\n",
            "const mid = \"not.sts.a.module\";\n",
            "const obj = { from: \"./obj.sts\", note: \"./note.sts\" };\n",
            "function nested() { import { z } from \"./inside.sts\"; return import(\"./also.sts\"); }\n",
            "const arrow = () => { import { y } from \"./arrow.sts\"; };\n",
        );
        let mut out = TransformSession::check().transform("main.sts", code);
        let has = |needle: &str| {
            assert!(out.code.contains(needle), "{needle} missing in {}", out.code);
        };
        has("from \"./logic\"");
        has("from './types'");
        has("require(\"./value\")");
        has("from \"./panel\"");
        has("from \"./all\"");
        has("import(\"./dyn\")");
        has("import.source(\"./source\")");
        has("import(\"./box\").Box");
        has("import(\"./inner\")");
        has("import(\"./q?x=1\")");
        has("import(\"./already.ts\")");
        has("\"keep ./note.sts in a string\"");
        has("`keep ./note.sts in a template`");
        has("tail ./note.sts");
        has("// comment ./comment.sts must stay");
        has("\"not.sts.a.module\"");
        has("from \"./inside\"");
        has("import(\"./also\")");
        has("from \"./arrow\"");
        let obj = out.code.find("const obj").expect("obj");
        let line = &out.code[obj..out.code[obj..].find('\n').map_or(out.code.len(), |n| obj + n)];
        assert_eq!(line, "const obj = { from: \"./obj.sts\", note: \"./note.sts\" };", "{line:?}");
        has("./note.sts");
        has("not.sts.a.module");
        // Verbatim pieces still copy the author's bytes. The shortened
        // specifier is an atom over the original literal.
        for [vs, vl, os, ol, kind] in tuples(&out.mappings) {
            let (vs, vl, os, ol) = (vs as usize, vl as usize, os as usize, ol as usize);
            if kind == 0 {
                assert_eq!(&out.code[vs..vs + vl], &code[os..os + ol]);
            }
        }
        let logic = out.code.find("./logic\"").unwrap();
        let covering = tuples(&out.mappings)
            .into_iter()
            .find(|item| item[0] as usize <= logic && logic < item[0] as usize + item[1] as usize)
            .expect("logic specifier segment");
        assert_eq!(covering[4], 1, "rewritten specifier is an atom");
        let original = &code[covering[2] as usize..covering[2] as usize + covering[3] as usize];
        assert!(original.contains("./logic.sts"), "{original}");
        // Text after the shorter specifier still maps to itself.
        let comment = out.code.find("comment ./comment.sts").unwrap();
        let comment_seg = tuples(&out.mappings)
            .into_iter()
            .find(|item| {
                item[0] as usize <= comment && comment < item[0] as usize + item[1] as usize
            })
            .expect("comment segment");
        assert_eq!(comment_seg[4], 0);
        assert_eq!(
            &out.code[comment_seg[0] as usize..comment_seg[0] as usize + comment_seg[1] as usize],
            &code[comment_seg[2] as usize..comment_seg[2] as usize + comment_seg[3] as usize]
        );

        // The same rewrite on a file that is not Sweetener source.
        out = TransformSession::check().transform("main.ts", code);
        assert!(out.code.contains("from \"./logic\""), "{}", out.code);
        assert!(out.code.contains("\"keep ./note.sts in a string\""), "{}", out.code);
    }

    fn tuples(mappings: &str) -> Vec<[i32; 5]> {
        let mut out = Vec::new();
        let mut number = String::new();
        let mut values = Vec::new();
        for ch in mappings.chars() {
            if ch.is_ascii_digit() || ch == '-' {
                number.push(ch);
                continue;
            }
            if !number.is_empty() {
                values.push(number.parse::<i32>().unwrap_or(0));
                number.clear();
            }
            if ch == ']' && values.len() == 5 {
                out.push([values[0], values[1], values[2], values[3], values[4]]);
                values.clear();
            }
        }
        out
    }
}

mod cli_args {
    use ts_ext_syntax::cli::args::{Flag, Takes, flag, parse};

    const FLAGS: &[Flag] = &[
        flag("json", Takes::Nothing),
        flag("stage", Takes::Value),
        flag("map", Takes::OptionalValue),
    ];

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn forms() {
        let parsed = parse(
            &args(&["a.sts", "--stage", "bundle", "--map", "--json", "-", "--", "--x"]),
            FLAGS,
        )
        .unwrap();
        assert_eq!(parsed.value("stage"), Some("bundle"));
        assert!(parsed.has("map") && parsed.value("map").is_none() && parsed.has("json"));
        assert_eq!(parsed.positionals, ["a.sts", "-", "--x"]);
        let parsed = parse(&args(&["--stage=tsc", "--map=out.map"]), FLAGS).unwrap();
        assert_eq!((parsed.value("stage"), parsed.value("map")), (Some("tsc"), Some("out.map")));
        assert!(parse(&args(&["--nope"]), FLAGS).unwrap_err().contains("unknown option --nope"));
        assert!(parse(&args(&["--stage"]), FLAGS).unwrap_err().contains("needs a value"));
        assert!(parse(&args(&["--json=1"]), FLAGS).unwrap_err().contains("takes no value"));
        assert!(parse(&args(&["-x"]), FLAGS).is_err());
        assert!(parse(&args(&["-h"]), FLAGS).unwrap().help);
    }
}

mod cli_docs {
    use ts_ext_syntax::cli::docs::{explain, render, topic, topics};
    use ts_ext_syntax::cli::render::Style;

    #[test]
    fn every_topic_renders() {
        for (name, _) in topics() {
            assert!(topic(Some(&name)).is_some_and(|text| !text.is_empty()), "{name}");
        }
        assert!(topic(Some("nope")).is_none());
    }

    #[test]
    fn plain_rendering_keeps_code_readable() {
        let out = render(
            "# T\n\nUse `x` and **y** [z](#z).\n```ts\nconst a = 1;\n```\n",
            Style { on: false },
        );
        assert_eq!(out, "T\n=\n\nUse `x` and y z.\n    const a = 1;\n");
    }

    #[test]
    fn explain_finds_codes_and_suggests() {
        assert!(explain("SWR4001").unwrap().contains("No rule of the macro accepted"));
        assert!(explain("4001").is_ok());
        assert!(explain("SWR4999").unwrap_err().contains("SWR4001"));
    }
}

mod cli_render {
    use ts_ext_syntax::cli::render::{Diag, Format, Severity, Style, diagnostics, diff, line_col};

    #[test]
    fn line_col_counts_characters() {
        assert_eq!(line_col("ab\ncé d", 7), (2, 4));
        assert_eq!(line_col("x", 99), (1, 2));
    }

    #[test]
    fn diff_marks_changed_lines() {
        let out = diff("a.sts", "a\nb\nc\n", "a\nB\nc\n", Style { on: false });
        assert!(out.contains("@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n"), "{out}");
    }

    #[test]
    fn frame_points_at_the_span() {
        let diag = Diag::new(Severity::Error, "SWR4001: No rule.", 10, 11, vec![]);
        assert_eq!(diag.code, "SWR4001");
        let out =
            diagnostics("a.sts", "const a = x;\n", &[diag], Format::Pretty, Style { on: false });
        assert!(
            out.contains(
                "a.sts:1:11: error SWR4001: No rule.\n  1 | const a = x;\n    |           ^\n"
            ),
            "{out}"
        );
    }
}
