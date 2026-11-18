use rustc_hash::FxHashMap;
use tsrs_ast::{Kind, Node, SourceFile, SourceFileMetaData, SourceFileParseOptions};
use tsrs_binder::bind_source_file;
use tsrs_checker::ext_overload::{hover_expression, is_named_directive};
use tsrs_checker::{Program, new_checker};
use tsrs_core::tspath::Path;
use tsrs_core::{
    CompilerOptions, ModuleKind, P, RESOLUTION_MODE_NONE, ResolutionMode, ScriptKind, alloc,
    alloc_slice,
};
use tsrs_module::symlinks::KnownSymlinks;
use tsrs_module::{ModeAwareCache, ResolvedModule, packagejson};
use tsrs_modulespecifiers::ModuleSpecifierGenerationHost;
use tsrs_parser::parse_source_file;
use tsrs_tsoptions::outputpaths::OutputPathsHost;
use tsrs_tsoptions::{ParsedCommandLine, SourceOutputAndProjectReference};

struct TestProgram {
    options: P<CompilerOptions>,
    files: &'static [P<SourceFile>],
    resolved_modules: FxHashMap<Path, ModeAwareCache<P<ResolvedModule>>>,
    packages_map: FxHashMap<String, bool>,
}

impl Program for TestProgram {
    fn options(&self) -> P<CompilerOptions> {
        self.options
    }

    fn source_files(&self) -> &'static [P<SourceFile>] {
        self.files
    }

    fn bind_source_files(&self) {
        for &file in self.files {
            bind_source_file(file);
        }
    }

    fn file_exists(&self, file_name: &str) -> bool {
        self.files.iter().any(|f| f.file_name() == file_name)
    }

    fn get_source_file(&self, file_name: &str) -> Option<P<SourceFile>> {
        self.files.iter().copied().find(|f| f.file_name() == file_name)
    }

    fn get_source_file_for_resolved_module(&self, file_name: &str) -> Option<P<SourceFile>> {
        self.get_source_file(file_name)
    }

    fn get_emit_module_format_of_file(&self, _source_file: P<SourceFile>) -> ModuleKind {
        ModuleKind::default()
    }

    fn get_emit_syntax_for_usage_location(
        &self,
        _source_file: P<SourceFile>,
        _usage_location: P<Node>,
    ) -> ResolutionMode {
        RESOLUTION_MODE_NONE
    }

    fn get_implied_node_format_for_emit(&self, _source_file: P<SourceFile>) -> ModuleKind {
        ModuleKind::default()
    }

    fn get_resolved_module(
        &self,
        _current_source_file: P<SourceFile>,
        _module_reference: &str,
        _mode: ResolutionMode,
    ) -> Option<P<ResolvedModule>> {
        None
    }

    fn get_resolved_modules(&self) -> &FxHashMap<Path, ModeAwareCache<P<ResolvedModule>>> {
        &self.resolved_modules
    }

    fn get_packages_map(&self) -> &FxHashMap<String, bool> {
        &self.packages_map
    }

    fn get_source_file_meta_data(&self, _path: &Path) -> SourceFileMetaData {
        SourceFileMetaData::default()
    }

    fn get_jsx_runtime_import_specifier(&self, _path: &Path) -> (String, Option<P<Node>>) {
        (String::new(), None)
    }

    fn get_import_helpers_import_specifier(&self, _path: &Path) -> Option<P<Node>> {
        None
    }

    fn source_file_may_be_emitted(
        &self,
        _source_file: P<SourceFile>,
        _force_dts_emit: bool,
    ) -> bool {
        false
    }

    fn is_source_file_default_library(&self, _path: &Path) -> bool {
        false
    }

    fn get_project_reference_from_output_dts(
        &self,
        _path: &Path,
    ) -> Option<P<SourceOutputAndProjectReference>> {
        None
    }

    fn get_project_reference_from_source(
        &self,
        _path: &Path,
    ) -> Option<P<SourceOutputAndProjectReference>> {
        None
    }

    fn get_redirect_for_resolution(&self, _file: P<SourceFile>) -> Option<P<ParsedCommandLine>> {
        None
    }

    fn common_source_directory(&self) -> String {
        "/".to_string()
    }

    fn use_case_sensitive_file_names(&self) -> bool {
        true
    }

    fn get_current_directory(&self) -> String {
        "/".to_string()
    }

    fn get_default_resolution_mode_for_file(&self, _file: P<SourceFile>) -> ResolutionMode {
        RESOLUTION_MODE_NONE
    }

    fn get_mode_for_usage_location(
        &self,
        _file: P<SourceFile>,
        _module_specifier: P<Node>,
    ) -> ResolutionMode {
        RESOLUTION_MODE_NONE
    }

    fn as_module_specifier_generation_host(&self) -> &dyn ModuleSpecifierGenerationHost {
        self
    }
}

impl OutputPathsHost for TestProgram {
    fn common_source_directory(&self) -> String {
        "/".to_string()
    }

    fn content_mapper_extensions(&self) -> Vec<String> {
        Vec::new()
    }

    fn get_current_directory(&self) -> &'static str {
        "/"
    }

    fn use_case_sensitive_file_names(&self) -> bool {
        true
    }
}

impl ModuleSpecifierGenerationHost for TestProgram {
    fn get_symlink_cache(&self) -> P<KnownSymlinks> {
        P::new(KnownSymlinks::default())
    }

    fn get_global_typings_cache_location(&self) -> String {
        String::new()
    }

    fn get_project_reference_from_source(
        &self,
        _path: &Path,
    ) -> Option<P<SourceOutputAndProjectReference>> {
        None
    }

    fn get_redirect_targets(&self, _path: &Path) -> Vec<String> {
        Vec::new()
    }

    fn get_source_of_project_reference_if_output_included(&self, _file: P<SourceFile>) -> String {
        String::new()
    }

    fn file_exists(&self, path: &str) -> bool {
        Program::file_exists(self, path)
    }

    fn get_nearest_ancestor_directory_with_package_json(&self, _dirname: &str) -> String {
        String::new()
    }

    fn get_package_json_info(
        &self,
        _pkg_json_path: &str,
    ) -> Option<P<packagejson::InfoCacheEntry>> {
        None
    }

    fn get_default_resolution_mode_for_file(&self, _file: P<SourceFile>) -> ResolutionMode {
        RESOLUTION_MODE_NONE
    }

    fn get_resolved_module_from_module_specifier(
        &self,
        _file: P<SourceFile>,
        _module_specifier: P<Node>,
    ) -> Option<P<ResolvedModule>> {
        None
    }

    fn get_mode_for_usage_location(
        &self,
        _file: P<SourceFile>,
        _module_specifier: P<Node>,
    ) -> ResolutionMode {
        RESOLUTION_MODE_NONE
    }
}

fn parse(file_name: &str, text: &str) -> P<SourceFile> {
    parse_source_file(
        SourceFileParseOptions {
            file_name: file_name.to_string(),
            path: Path::new(file_name.to_string()),
            ..Default::default()
        },
        text,
        ScriptKind::TS,
    )
}

fn find_first_binary_expr(node: P<Node>) -> Option<P<Node>> {
    if node.kind() == Kind::BinaryExpression {
        return Some(node);
    }
    let mut found = None;
    node.for_each_child(&mut |child| {
        if found.is_none() {
            found = find_first_binary_expr(child);
        }
        found.is_some()
    });
    found
}

#[test]
fn hover_expression_respects_tsover_and_gpu_scopes() {
    let plain = parse("/plain.ts", "const x = 1 + 2;");
    let plain_bin = find_first_binary_expr(plain.as_node()).unwrap();
    assert!(hover_expression(plain_bin).is_none());
    assert!(hover_expression(plain_bin.as_binary_expression().operator_token).is_none());

    let tsover = parse("/tsover.ts", "\"use tsover\";\nconst x = 1 + 2;");
    let first_stmt = tsover.statements.nodes()[0];
    assert!(is_named_directive(first_stmt.expression().unwrap(), "tsover"));
    assert!(!is_named_directive(first_stmt.expression().unwrap(), "gpu"));

    let tsover_bin = find_first_binary_expr(tsover.as_node()).unwrap();
    assert_eq!(hover_expression(tsover_bin), Some(tsover_bin));
    assert_eq!(
        hover_expression(tsover_bin.as_binary_expression().operator_token),
        Some(tsover_bin)
    );
    assert!(hover_expression(tsover_bin.as_binary_expression().left).is_none());

    let fn_gpu =
        parse("/gpu.ts", "function shader() { 'use gpu'; return 1 + 2; }\nconst y = 3 + 4;");
    let fn_decl = fn_gpu.statements.nodes()[0];
    let gpu_bin = find_first_binary_expr(fn_decl).unwrap();
    assert_eq!(hover_expression(gpu_bin), Some(gpu_bin));

    let outer_stmt = fn_gpu.statements.nodes()[1];
    let outer_bin = find_first_binary_expr(outer_stmt).unwrap();
    assert!(hover_expression(outer_bin).is_none());
}

#[test]
fn check_binary_resolves_operator_plus_and_defer_operation() {
    let code = r#"
"use tsover";
interface SymbolConstructor {
    readonly operatorPlus: unique symbol;
    readonly deferOperation: unique symbol;
}
declare var Symbol: SymbolConstructor;

interface Scalar {
    [Symbol.operatorPlus](lhs: Scalar, rhs: Vec2): typeof Symbol.deferOperation;
}
interface Vec2 {
    x: number;
    y: number;
    [Symbol.operatorPlus](lhs: Vec2, rhs: Vec2): Vec2;
    [Symbol.operatorPlus](lhs: Scalar, rhs: Vec2): Vec2;
}
declare const s: Scalar;
declare const v: Vec2;
const sum1 = v + v;
const sum2 = s + v;
"#;
    let file = parse("/overload.ts", code);
    let program = alloc(TestProgram {
        options: P::new(CompilerOptions::default()),
        files: alloc_slice(&[file]),
        resolved_modules: FxHashMap::default(),
        packages_map: FxHashMap::default(),
    });
    let mut checker = new_checker(program);

    let stmts = file.statements.nodes();
    let sum1_bin = find_first_binary_expr(stmts[stmts.len() - 2]).unwrap();
    let sum2_bin = find_first_binary_expr(stmts[stmts.len() - 1]).unwrap();

    let sum1_type = checker.get_type_at_location(sum1_bin);
    let sum2_type = checker.get_type_at_location(sum2_bin);
    assert_eq!(sum1_type.symbol().map(|s| s.name()), Some("Vec2"));
    assert_eq!(sum2_type.symbol().map(|s| s.name()), Some("Vec2"));
}
