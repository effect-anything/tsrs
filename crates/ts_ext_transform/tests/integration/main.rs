use std::fmt::Write as _;

use ts_ext_transform::align::{align, retarget};
use ts_ext_transform::precheck::{
    MAPPER_NAME, checked, compute_ecma_line_starts, emit_output, emit_output_file,
    emits_javascript, offset_to_line_character, rewrite_emitted_file,
};
use ts_ext_transform::tsover::rewrite_text;
use ts_ext_transform::typegpu::embed_metadata_js;
use tsrs_ast::{
    ContentMapperSourceFileInfo, ExternalModuleIndicatorOptions, SourceFile, SourceFileParseOptions,
};
use tsrs_core::arena::Region;
use tsrs_core::tspath::{ComparePathsOptions, Path};
use tsrs_core::{P, ScriptKind, alloc_str};
use tsrs_parser::parse_source_file_owned;
use tsrs_sourcemap::{Mapping, RawSourceMap, decode_mappings, new_generator};
use tsrs_spanmap::{self as spanmap, Feature, Kind, Segment};

fn mapped<'a>(old: &'a str, new: &str, at: &str) -> Option<&'a str> {
    let pos = i32::try_from(new.find(at)?).ok()?;
    align(old, new).into_iter().find(|seg| seg.4 == 0 && seg.0 <= pos && pos < seg.1).map(|seg| {
        let start = usize::try_from(seg.2 + (pos - seg.0)).unwrap_or(0);
        &old[start..start + at.len()]
    })
}

#[test]
fn a_hoisted_function_keeps_its_tokens() {
    let old = "console.log(add);\nfunction add(a, b) {\n  'use gpu';\n  return a + b;\n}\n";
    let new = "const add = ($ => set($.f = function add(a, b) {\n  'use gpu';\n  return __tsover_add(a, b);\n}, {v: 2}) && $.f)({});\nconsole.log(add);\n";
    assert_eq!(mapped(old, new, "'use gpu'"), Some("'use gpu'"));
    assert_eq!(mapped(old, new, "return"), Some("return"));
    assert_eq!(mapped(old, new, "console"), Some("console"));
    let segs = align(old, new);
    let call = i32::try_from(new.rfind("a, b)").unwrap() + 1).unwrap();
    assert!(
        segs.iter().any(|s| {
            let s2 = usize::try_from(s.2).unwrap();
            let s3 = usize::try_from(s.3).unwrap();
            s.4 == 1 && s.0 <= call && call < s.1 && &old[s2..s3] == "+"
        }),
        "{segs:?}"
    );
}

#[test]
fn retarget_composes() {
    let old = "let x = f(1);";
    let first = vec![(0, 8, 0, 8, 0), (8, 12, 8, 14, 1), (12, 13, 14, 15, 0)];
    let new = "let x = f(1);\n";
    assert_eq!(
        retarget(old, new, &first),
        vec![(0, 8, 0, 8, 0), (8, 12, 8, 14, 1), (12, 13, 14, 15, 0)]
    );
}

#[test]
fn a_long_unchanged_run_is_grown_once() {
    let mut old = String::new();
    for k in 0..40_000 {
        let _ = writeln!(old, "let v{k} = {k};");
    }
    let new = format!("{old}export {{}};\n");
    let started = std::time::Instant::now();
    assert_eq!(mapped(&old, &new, "v39999 ="), Some("v39999 ="));
    assert!(started.elapsed().as_secs() < 10, "align is quadratic again");
}

#[test]
fn nested_gpu_operators_do_not_keep_a_stray_character() {
    let code = "function f() {\n  \"use gpu\";\n  const corner = id - segment * 3;\n  const turn = (segment / 216) * 6.2831855;\n  const theta = angle + spin + (1.5707963 - 3.14159265 / petals);\n}\n";
    let rewritten = rewrite_text("a.ts", code).unwrap();
    assert!(
        rewritten.code.contains("__tsover_sub(id, __tsover_mul(segment, 3))"),
        "{}",
        rewritten.code
    );
    assert!(
        rewritten.code.contains("__tsover_mul((__tsover_div(segment, 216)), 6.2831855)"),
        "{}",
        rewritten.code
    );
    assert!(
        !rewritten.code.contains("s__tsover") && !rewritten.code.contains("1__tsover"),
        "{}",
        rewritten.code
    );
}

fn parse_mapped_in_current_region(
    virtual_text: &str,
    original: &str,
    segments: &[Segment],
) -> P<SourceFile> {
    let file_name = "/a.ts".to_owned();
    let opts = SourceFileParseOptions {
        path: Path::from(file_name.clone()),
        file_name,
        external_module_indicator_options: ExternalModuleIndicatorOptions::default(),
    };
    let file = parse_source_file_owned(opts.clone(), virtual_text.to_owned(), ScriptKind::TS);
    file.set_content_mapper_info(ContentMapperSourceFileInfo {
        content_mapper: "sweetener",
        transform_identity: "sweetener",
        parse_options: opts,
        virtual_file_name: "/a.ts.ts",
        original_text: alloc_str(original),
        span_map: Some(spanmap::new(segments)),
        diagnostic_directives: &[],
        supplemental_source_files: &[],
        canonical_source_file: None,
    });
    checked(Some(file)).unwrap()
}

fn at(hay: &str, needle: &str) -> u32 {
    u32::try_from(hay.find(needle).unwrap_or_else(|| panic!("{needle}"))).unwrap_or(0)
}

fn seg(v0: u32, v1: u32, o0: u32, o1: u32, kind: Kind) -> Segment {
    Segment {
        virtual_start: v0,
        virtual_end: v1,
        original_start: o0,
        original_end: o1,
        kind,
        features: Feature::All,
    }
}

#[test]
fn check_keeps_the_author_operator() {
    let region = Region::new(64 << 10);
    let _guard = region.enter();
    let author = "function step(n: number) {\n  \"use gpu\";\n  return n + 1;\n}\n";
    let end = u32::try_from(author.len()).unwrap_or(0);
    let parsed =
        parse_mapped_in_current_region(author, author, &[seg(0, end, 0, end, Kind::VERBATIM)]);
    assert!(parsed.text().contains("return n + 1"), "{}", parsed.text());
    assert!(!parsed.text().contains("__tsover_add"), "{}", parsed.text());
    assert!(!parsed.text().contains("declare function"), "{}", parsed.text());
    let map = parsed.span_map().expect("span map");
    let original = parsed.original_text();
    assert!(map.validate(parsed.text(), original).is_none());
    let plus = at(parsed.text(), "+");
    let (pos, fidelity) = map.virtual_to_original_position(plus);
    assert!(fidelity.is_exact());
    assert_eq!(pos, at(original, "+"));
}

#[test]
fn hoists_stay_below_the_prologue() {
    let code = "\"use strict\";\nimport tgpu from 'typegpu';\nconst f = () => {\n    'use gpu';\n    return 1 + 2;\n};\n";
    let out = embed_metadata_js("a.js", code);
    assert!(out.starts_with("\"use strict\";"), "{out}");
}

#[test]
fn type_only_wrappers_are_erased_in_tgsl() {
    let ts = "import tgpu from 'typegpu';\nconst f = (a: number[] | undefined) => {\n    'use gpu';\n    return (a! as number[])[0]! satisfies number;\n};\n";
    let js =
        "import tgpu from 'typegpu';\nconst f = (a) => {\n    'use gpu';\n    return (a)[0];\n};\n";
    let from_ts = embed_metadata_js("a.ts", ts);
    let from_js = embed_metadata_js("a.js", js);
    let ast = |out: &str| {
        out[out.find("ast:").expect(out)..].split("externals").next().unwrap().to_owned()
    };
    assert_eq!(ast(&from_ts), ast(&from_js), "{from_ts}");
}

#[test]
fn shells_are_wrapped_without_a_directive() {
    let code =
        "import tgpu from 'typegpu';\nexport const main = tgpu.fn([])(() => {\n    return;\n});\n";
    let out = embed_metadata_js("a.js", code);
    assert_ne!(out, code, "{out}");
    assert!(out.contains("main"), "{out}");
}

#[test]
fn rewritten_javascript_map_keeps_the_author_column() {
    let javascript = "\"use tsover\";\nexport const value = a + b;\n";
    let output = emit_output("a.js", javascript);
    let rewritten = &output.code;
    let call = rewritten.find("__tsover_add(").expect("rewritten call");
    let plus = javascript.find(" + ").unwrap() + 1;
    let tail = javascript.rfind(';').unwrap();
    assert!(
        rewritten.len() > javascript.len(),
        "rewrite {rewritten:?} did not grow {javascript:?}"
    );

    let mut generator = new_generator("a.js", "", "", ComparePathsOptions::default());
    let source = generator.add_source("a.ts");
    let before = line_column_of(javascript, javascript.find("export").unwrap());
    generator.add_source_mapping(before.0, before.1, source, 1, 7).unwrap();
    let operator = line_column_of(javascript, plus);
    generator.add_source_mapping(operator.0, operator.1, source, 1, 22).unwrap();
    let after = line_column_of(javascript, tail);
    generator.add_source_mapping(after.0, after.1, source, 1, 30).unwrap();
    let stale = generator.string();

    let (rewritten_js, map_opt) = emit_output_file("a.js", javascript, javascript, Some(&stale));
    assert_eq!(&rewritten_js, rewritten);
    let map = map_opt.expect("source map");
    assert_ne!(map, stale, "the rewrite must move generated columns");
    let value = tsrs_core::json::unmarshal(&map).unwrap();
    let decoded = RawSourceMap::from_json(&value).unwrap();
    assert_eq!(decoded.sources, ["a.ts"]);
    let mut decoder = decode_mappings(&decoded.mappings);
    let mappings = decoder.values().collect::<Vec<_>>();

    let (export_line, export_column) = line_column_of(rewritten, rewritten.find("export").unwrap());
    assert_eq!(
        generated_at(&mappings, 1, 7),
        (export_line, export_column),
        "text before the call does not move"
    );
    let (call_line, call_column) = line_column_of(rewritten, call);
    assert_eq!(
        generated_at(&mappings, 1, 22),
        (call_line, call_column),
        "the author's + is the start of __tsover_add, map {map}"
    );
    let (tail_line, tail_column) = line_column_of(rewritten, rewritten.rfind(';').unwrap());
    assert_eq!(
        generated_at(&mappings, 1, 30),
        (tail_line, tail_column),
        "the tail shifts by the inserted call, map {map}"
    );
}

#[test]
fn wrapped_call_keeps_the_operand_columns() {
    let javascript = "function add(a, b) {\n  \"use gpu\";\n  return a + b;\n}\n";
    let output = emit_output("a.js", javascript);
    let call = output.code.rfind("__tsover_add(").expect(&output.code);
    let left = call + "__tsover_add(".len();
    let right = output.code[call..].find(", b)").expect(&output.code) + call + 2;
    let plus = u32::try_from(javascript.find('+').unwrap()).unwrap();
    let author_left = u32::try_from(javascript.rfind("a +").unwrap()).unwrap();
    let author_right = u32::try_from(javascript.rfind("b;").unwrap()).unwrap();
    assert_eq!(
        original_of(&output.segments, call),
        plus,
        "wrapper moved the operator off its token\n{}",
        output.code
    );
    assert_eq!(original_of(&output.segments, left), author_left);
    assert_eq!(original_of(&output.segments, right), author_right);
}

fn original_of(segments: &[Segment], generated: usize) -> u32 {
    let offset = u32::try_from(generated).unwrap_or(0);
    let segment = segments
        .iter()
        .find(|segment| offset >= segment.virtual_start && offset < segment.virtual_end)
        .unwrap_or_else(|| panic!("no segment at {generated}"));
    if segment.kind == Kind::VERBATIM {
        segment.original_start + (offset - segment.virtual_start)
    } else {
        segment.original_start
    }
}

fn generated_at(mappings: &[Mapping], source_line: u32, source_column: u32) -> (u32, u32) {
    let mapping = mappings
        .iter()
        .find(|mapping| {
            mapping.is_source_mapping()
                && mapping.source_line == Some(source_line)
                && mapping.source_character == Some(source_column)
        })
        .unwrap_or_else(|| panic!("no mapping for {source_line}:{source_column}"));
    (mapping.generated_line, mapping.generated_character)
}

fn line_column_of(text: &str, offset: usize) -> (u32, u32) {
    let lines = compute_ecma_line_starts(text);
    let offset = u32::try_from(offset).unwrap_or(0);
    offset_to_line_character(&lines, text, offset)
}

#[test]
fn an_unrewritten_file_keeps_its_map() {
    let map = r#"{"version":3,"file":"a.js","sources":["a.ts"],"names":[],"mappings":"AAAA"}"#;
    let js = "export const n = 1;\n";
    let (out_js, out_map) = emit_output_file("a.js", js, js, Some(map));
    assert_eq!(out_js, js);
    assert_eq!(out_map.as_deref(), Some(map));
}

#[test]
fn rewrite_emitted_file_skips_unchanged_and_rewrites_tsover() {
    let region = Region::new(64 << 10);
    let _guard = region.enter();
    let opts = SourceFileParseOptions {
        path: Path::from("/a.ts"),
        file_name: "/a.ts".to_owned(),
        external_module_indicator_options: ExternalModuleIndicatorOptions::default(),
    };
    let sf = parse_source_file_owned(opts, "export const n = 1;\n".to_owned(), ScriptKind::TS);
    assert!(rewrite_emitted_file(sf, "export const n = 1;\n", None).is_none());
    let rewritten = rewrite_emitted_file(sf, "\"use tsover\";\nexport const n = 1 + 2;\n", None)
        .expect("rewritten");
    assert!(rewritten.0.contains("__tsover_add(1, 2)"));
    assert!(emits_javascript(""));
    assert!(emits_javascript(MAPPER_NAME));
    assert!(!emits_javascript("sweetener"));
}
