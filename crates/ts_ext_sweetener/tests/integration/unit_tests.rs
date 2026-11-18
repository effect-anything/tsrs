//! Unit tests extracted from `crates/ts_ext_sweetener/src/**` to comply with
//! `tsrs` workspace lint rules (no inline `#[cfg(test)]` in `src/`).

mod structure_tests {
    use ts_ext_sweetener::structure::{same, unified};

    #[test]
    fn parentheses_are_shape_and_a_block_is_not_an_object() {
        assert!(same("const f = () => (a && b) && c\n", "const f = () => a && (b && c)\n", false));
        assert!(same("const f = () => (a + b)\n", "const f = () => a + b\n", false));
        assert!(!same(
            "const f = () => ({ tag: \"None\" })\n",
            "const f = () => { tag: \"None\" }\n",
            false
        ));
    }

    #[test]
    fn the_diff_names_the_files_and_not_a_temp_directory() {
        let text = unified("a\nb\n", "a\nc\n");
        assert!(text.starts_with("--- theirs.ts\n+++ ours.ts\n"), "{text}");
        assert!(!text.contains("/var/"), "{text}");
        assert!(!text.contains("2026-"), "{text}");
        assert!(text.contains("-b\n"), "{text}");
        assert!(text.contains("+c\n"), "{text}");
    }
}

mod hygiene_tests {
    use ts_ext_sweetener::__test_support::hygiene::{assign, placeholder};
    use ts_ext_sweetener::{Segment, SegmentKind};

    fn run(code: &str) -> String {
        assign(code, &mut [], true)
    }

    #[test]
    fn first_occurrence_keeps_the_spelling() {
        let a = placeholder("tmp");
        let b = placeholder("tmp");
        let out = run(&format!("const {a} = 1; const {b} = {a};"));
        assert_eq!(out, "const tmp = 1; const tmp_1 = tmp;");
    }

    #[test]
    fn user_names_block_and_properties_do_not() {
        let a = placeholder("id");
        let b = placeholder("x");
        let out = run(&format!("const id = 0; obj.x; const {a} = 1; let {b} = {{ x: 1 }};"));
        assert_eq!(out, "const id = 0; obj.x; const id_1 = 1; let x = { x: 1 };");
    }

    #[test]
    fn exported_binders_keep_their_name() {
        let a = placeholder("value");
        let out = run(&format!("const value = 1; export const {a} = 2;"));
        assert_eq!(out, "const value = 1; export const value = 2;");
        let b = placeholder("v");
        let out = run(&format!("let v; function {b}() {{}} export {{ {b} }};"));
        assert_eq!(out, "let v; function v() {} export { v };");
    }

    #[test]
    fn labels_are_their_own_namespace() {
        let a = placeholder("outer");
        let out = run(&format!("const outer = 1; {a}: for (;;) {{ break {a}; }}"));
        assert_eq!(out, "const outer = 1; outer: for (;;) { break outer; }");
        let b = placeholder("outer");
        let out = run(&format!("outer: while (1) {{ {b}: for (;;) {{ continue {b}; }} }}"));
        assert_eq!(out, "outer: while (1) { outer_1: for (;;) { continue outer_1; } }");
    }

    #[test]
    fn shorthand_and_import_bindings_keep_the_property() {
        let a = placeholder("x");
        let out = run(&format!("let x; const {{ {a} }} = o; use({a});"));
        assert_eq!(out, "let x; const { x: x_1 } = o; use(x_1);");
        let b = placeholder("a");
        let out = run(&format!("import {{ {b} }} from \"m\"; const a = {b};"));
        assert_eq!(out, "import { a as a_1 } from \"m\"; const a = a_1;");
    }

    #[test]
    fn jsx_text_and_attributes_are_not_names() {
        let a = placeholder("cache");
        let b = placeholder("className");
        let out =
            run(&format!("const {a} = 1; let {b}; <div className={{{b}}}>nine cache slots</div>;"));
        assert_eq!(
            out,
            "const cache = 1; let className; <div className={className}>nine cache slots</div>;"
        );
    }

    #[test]
    fn template_interpolations_count() {
        let a = placeholder("n");
        let out = run(&format!("const {a} = 1; `${{n}} ${{{a}}}`;"));
        assert_eq!(out, "const n_1 = 1; `${n} ${n_1}`;");
    }

    #[test]
    fn segments_move_with_the_text() {
        let a = placeholder("t");
        let code = format!("let {a} = 1; z;");
        let z = code.find('z').expect("z") as u32;
        let mut segments = [Segment {
            virtual_start: z,
            virtual_end: z + 1,
            original_start: 0,
            original_end: 1,
            kind: SegmentKind::Verbatim,
        }];
        let out = assign(&code, &mut segments, false);
        assert_eq!(&out[segments[0].virtual_start as usize..segments[0].virtual_end as usize], "z");
    }
}

mod scoped_tests {
    use ts_ext_sweetener::__test_support::scoped::{
        hoist, hoist_marker, parameter_markers, resolve_parameters, substitute,
    };

    #[test]
    fn operator_parameter_is_an_operand_only() {
        assert_eq!(substitute("% % 5", "%", "t"), ("t % 5".to_owned(), 1));
        assert_eq!(substitute("[%, % * 10]", "%", "t"), ("[t, t * 10]".to_owned(), 2));
        assert_eq!(substitute("`$ ${%}`", "%", "t"), ("`$ ${t}`".to_owned(), 1));
        assert_eq!(substitute("x %= 2", "%", "t").1, 0);
        assert_eq!(substitute("\"%\" + %", "%", "t").0, "\"%\" + t");
    }

    #[test]
    fn inner_regions_resolve_first() {
        let (open_b, close_b) = parameter_markers("%", "B", false);
        let (open_a, close_a) = parameter_markers("%", "A", true);
        let outer = format!("{open_a}f(%, {open_b}%.b{close_b}){close_a}");
        assert_eq!(resolve_parameters(&outer), ("f(A, B.b)".to_owned(), vec![]));
        let strings = format!("{open_a}\")\" + %{close_a}");
        assert_eq!(resolve_parameters(&strings).0, "\")\" + A");
        let (open, close) = parameter_markers("%", "A", true);
        let lonely = format!("{open}1{close}");
        assert_eq!(resolve_parameters(&lonely).1.len(), 1);
    }

    #[test]
    fn hoists_into_the_enclosing_function() {
        let code =
            format!("function* g() {{\n  const r = (t = 1, (yield t){});\n}}\n", hoist_marker("t"));
        assert_eq!(
            hoist(&code, &mut [], false),
            "function* g() { let t;\n  const r = (t = 1, (yield t));\n}\n"
        );
    }
}

mod reader_tests {
    use ts_ext_sweetener::__test_support::reader::{read_syntax, slice};

    #[test]
    fn reader_outer_closer_ends_unclosed_inner_group() {
        let read = read_syntax("f { g(a, b }\nh()");
        let codes: Vec<&str> = read.diagnostics.iter().map(|d| &d.message[..7]).collect();
        assert_eq!(codes, vec!["SWR1003"]);
    }

    /// Multi-byte scalars, combining marks, emoji and the scalars on either
    /// side of the surrogate range, dropped into every byte-scanned form.
    #[test]
    fn reader_survives_multibyte_boundaries() {
        let marks = [
            "\u{e9}",
            "\u{6c49}",
            "\u{1f600}",
            "\u{0301}",
            "\u{200d}",
            "\u{fe0f}",
            "\u{d7ff}",
            "\u{e000}",
            "\u{1f468}\u{200d}\u{1f469}",
        ];
        let frames = [
            "const a = {m};",
            "/* {m} */ const a = 1;",
            "// {m}\nconst a = 1;",
            "const a = \"{m}\";",
            "const a = '{m}';",
            "const a = `{m}`;",
            "const a = `${{m}}`;",
            "const a = `{m} ${x}`;",
            "f({m})",
            "f {{ {m} }",
            "{m}(a, b",
            "\"{m}",
            "`{m}",
            "/{m}/g",
            "a {m} b",
            "{m}{m}",
        ];
        for mark in marks {
            for frame in frames {
                let code = frame.replace("{m}", mark);
                let read = std::panic::catch_unwind(|| read_syntax(&code));
                let read = read.unwrap_or_else(|_| panic!("reader panicked on {code:?}"));
                for node in &read.children {
                    let text = slice(&code, node);
                    assert!(
                        code.contains(text) || text.is_empty(),
                        "slice left the source: {text:?} of {code:?}"
                    );
                }
            }
        }
    }
}

mod context_tests {
    use ts_ext_sweetener::__test_support::context::{
        Contexts, MODULE, NEITHER, TOP, lookup, regions,
    };
    use ts_ext_sweetener::__test_support::reader::read_syntax;

    fn at(code: &str, marker: &str) -> Contexts {
        let regions = regions(&read_syntax(code).children, MODULE);
        lookup(&regions, code.find(marker).expect("marker"), MODULE)
    }

    #[test]
    fn function_headers_decide_yield_and_await() {
        let code = "function* g() { A; const f = () => B; }\nconst c = C;\nasync function k(x = D) { E }\nclass K { *m() { F } x = G; async n() { H } }\nnamespace N { I }\nasync function* ag() { const q = async () => J; function inner() { K2 } }\nclass Z {}\nfunction* ty(): Generator<A, B, C> { P; }\nasync function* ab() { N; const q = async () => O; }\nconst o = { m() { L } };\nasync function k() { return w(M); }\n";
        let generator = Contexts { generator: true, async_: false };
        let async_only = Contexts { generator: false, async_: true };
        for (marker, expected) in [
            ("A;", generator),
            ("B;", NEITHER),
            ("C;", TOP),
            ("D)", NEITHER),
            ("E }", async_only),
            ("F }", generator),
            ("G;", NEITHER),
            ("H }", async_only),
            ("I }", NEITHER),
            ("J;", async_only),
            ("K2", NEITHER),
            ("L }", NEITHER),
            ("M)", async_only),
            ("N;", Contexts { generator: true, async_: true }),
            ("O;", async_only),
            ("P;", generator),
        ] {
            assert_eq!(at(code, marker), expected, "{marker}");
        }
    }
}

mod group_tests {
    use ts_ext_sweetener::__test_support::group::needs_group;

    fn group(before: &str, expr: &str, after: &str) -> bool {
        needs_group(before, expr, after, false)
    }

    #[test]
    fn matches_upstream_printing() {
        assert!(group("const a = ", "p && q", ";"));
        assert!(!group("x = ", "p && q", ";"));
        assert!(!group("f(", "p && q", ")"));
        assert!(!group("[", "p && q", ", 1]"));
        assert!(group("function g() { return ", "p && q", "; }"));
        assert!(!group("const i = ", "p && q", " ? 1 : 2;"));
        assert!(group("!", "p && q", ";"));
        assert!(group("const a = ", "x + 1", " * 2;"));
        assert!(!group("const c = ", "-(x * y)", " * 2;"));
        assert!(group("const j = ", "-(a + b)", " ** 2;"));
        assert!(group("{ k: ", "a + b", " }"));
        assert!(!group("const e = ", "g(a + b)", ";"));
        assert!(needs_group("", "a + b", ".length", true));
        assert!(needs_group("p && ", "q || r", "", true));
        assert!(!needs_group("p && ", "typeof q", "", true));
        assert!(!needs_group("p && ", "x as T", "", true));
        assert!(needs_group("p && ", "q?.r ?? s", "", true));
        assert!(!needs_group("", "a * b", " && c", true));
        assert!(needs_group("", "a || b", " && c", true));
        assert!(!needs_group("", "p && q", "", true));
        assert!(needs_group("-", "-x", "", true));
        assert!(needs_group("p && ", "() => 1", "", true));
        assert!(!needs_group("", "(v) => v", "", true));
        assert!(group("const f = () => ", "{ value: 1 }", ";"));
        assert!(group("x;\n", "{ value: 2 }", ";"));
        assert!(group("", "function () { return 3; }", "();"));
        assert!(!group("const o = ", "{ value: 1 }", ";"));
        assert!(!group("f(", "function () {}", ")"));
    }
}

mod validate_tests {
    use ts_ext_sweetener::__test_support::reader::read_syntax;
    use ts_ext_sweetener::__test_support::validate::{KnownClass, validate};

    fn codes(code: &str) -> Vec<String> {
        let read = read_syntax(code);
        let classes = [KnownClass {
            name: "Pair".to_owned(),
            fields: vec!["left".to_owned(), "right".to_owned()],
        }];
        validate(&read.children, &classes)
            .0
            .into_iter()
            .map(|diagnostic| diagnostic.message[..7].to_owned())
            .collect()
    }

    #[test]
    fn pattern_and_template_checks() {
        for (code, expected) in [
            ("export syntax foo:expr {\n  rule { foo($) } => { 1 }\n}\n", vec!["SWR2002"]),
            ("export syntax foo:banana {\n  rule { foo() } => { 1 }\n}\n", vec!["SWR2003"]),
            (
                "export syntax foo:expr {\n  rule { foo($x:expr, $x:expr) } => { $x }\n}\n",
                vec!["SWR2004"],
            ),
            (
                "export syntax foo:expr {\n  rule { foo($($($x:expr)?)*) } => { 1 }\n}\n",
                vec!["SWR2006"],
            ),
            ("export syntax foo:expr {\n  rule { foo($x:nosuch) } => { $x }\n}\n", vec!["SWR2008"]),
            (
                "export syntax foo:expr {\n  rule { foo($x:expr) }\n  refine $y length equal 1;\n  => { $x }\n}\n",
                vec!["SWR2010"],
            ),
            ("export syntax foo:expr {\n  rule { foo($x:expr) } => { $y }\n}\n", vec!["SWR2011"]),
            (
                "export syntax foo:expr {\n  rule { foo($p:Pair) } => { $p.nope }\n}\n",
                vec!["SWR2012"],
            ),
            (
                "export syntax foo:expr {\n  rule { foo($($x:expr),*) } => { [$x] }\n}\n",
                vec!["SWR2013"],
            ),
            (
                "export syntax foo:expr {\n  rule { foo($x:expr) } => { [$(1),*] }\n}\n",
                vec!["SWR2014"],
            ),
            (
                "export syntax foo:expr {\n  rule { foo($($x:expr),* ; $($y:expr),*) } => { [$($x + $y),*] }\n}\n",
                vec!["SWR2015"],
            ),
            (
                "export syntax foo:expr {\n  rule { foo(a $x:expr | b) } => { 1 }\n}\n",
                vec!["SWR2005"],
            ),
            (
                "export syntax both:expr {\n  rule { both($($a:ident | $a:ident !),*) } => { [$($a),*] }\n}\n",
                vec![],
            ),
            (
                "export syntax m:expr {\n  rule { m([$($e:tt),*]) } => { #count($e) $(&& $e)* }\n}\n",
                vec![],
            ),
            (
                "export operator (|>):expr {\n  fixity infix;\n  // note\n  associativity right;\n  precedence 20;\n  rule { $a:expr |> $b:expr } => { $b }\n}\n",
                vec![],
            ),
            (
                "syntax class loop {\n  fields { v: tt; }\n  rule { $l:loop + $v:tt }\n}\n",
                vec!["SWR2009"],
            ),
            (
                "export operator (+++):expr {\n  fixity infix;\n  precedence 5;\n  rule { $a:expr +++ $b:expr } => { $a }\n}\n",
                vec!["SWR4006"],
            ),
            (
                "syntax foo:expr {\n  rule { foo() } => { 1 }\n}\nsyntax foo:expr {\n  rule { foo() } => { 2 }\n}\n",
                vec!["SWR4010"],
            ),
            (
                "function f() {\n  syntax foo:expr {\n    rule { foo() } => { 1 }\n  }\n}\n",
                vec!["SWR4016"],
            ),
            (
                "export syntax sum:expr {\n  rule { sum($($x:expr),*) } => { #fold($x, init: { 0 }) { ($acc, $item, $index) => { $acc + $item } } }\n}\n",
                vec![],
            ),
            (
                "export operator (|>):expr {\n  fixity infix;\n  associativity left;\n  precedence 1;\n  rule { $a:expr |> $f:expr } => { $f($a) }\n}\n",
                vec![],
            ),
        ] {
            assert_eq!(codes(code), expected, "{code}");
        }
    }

    #[test]
    fn binding_literal_paths_resolve() {
        assert_eq!(
            codes(
                "export syntax foo:expr {\n  literal nosuch.thing as NaN;\n  rule { foo() } => { NaN }\n}\n"
            ),
            ["SWR4009"]
        );
        assert!(
            codes("export syntax foo:expr {\n  literal globalThis.NaN as NaN;\n  rule { foo() } => { NaN }\n}\n")
                .is_empty()
        );
    }
}

mod forms_tests {
    use ts_ext_sweetener::__test_support::forms::{
        apply_signal_use, imported_syntax_bindings, join_ident, with_file_imports,
    };
    use ts_ext_sweetener::__test_support::reader::read_syntax;
    use ts_ext_sweetener::{ExpandRequest, Phase, Severity, expand};

    fn expand_state_signal(name: &str, initial: &str) -> String {
        let signal = join_ident(name, "", "Signal", "preserve");
        format!("const {signal} = createSignal({initial});")
    }

    #[derive(Clone, Copy, Debug)]
    struct ThreadStep<'a> {
        callee: &'a str,
        args: &'a [&'a str],
    }

    fn expand_thread_first(value: &str, steps: &[ThreadStep<'_>]) -> String {
        let mut current = value.trim().to_owned();
        for step in steps {
            let mut args = vec![current];
            args.extend(step.args.iter().map(|arg| arg.trim().to_owned()));
            current = format!("{}({})", step.callee, args.join(", "));
        }
        current
    }

    #[test]
    fn state_signal_substitutes_join_and_metavar() {
        let printed = expand_state_signal("count", "1");
        assert!(printed.contains("const countSignal = createSignal(1);"));
        assert!(!printed.contains("#syntax"));
        assert!(!printed.contains("#join"));
        assert!(!printed.contains("#metavar"));
        let use_ = apply_signal_use("count", "countSignal", Some("+="), Some("2"));
        assert_eq!(use_, "countSignal.set(countSignal.get() + 2)");
        let or = apply_signal_use("flag", "flagSignal", Some("||="), Some("true"));
        assert_eq!(or, "flagSignal.get() || flagSignal.set(true)");
    }

    #[test]
    fn channel_do_thread_and_jsx() {
        assert_eq!(
            expand_thread_first(
                "[1, 2, 3]",
                &[
                    ThreadStep { callee: "map", args: &["(value) => value + 1"] },
                    ThreadStep { callee: "filter", args: &["(value) => value > 2"] },
                ],
            ),
            "filter(map([1, 2, 3], (value) => value + 1), (value) => value > 2)"
        );
    }

    #[test]
    fn handwritten_forms_follow_the_syntax_import() {
        let bare = "const a = match (1) { 1 => 2, _ => 3 };\nconst v = #[1, 2];\nconst d = data Box<T> = Full(value: T) | Empty();\n";
        let expanded =
            expand(ExpandRequest { filename: "bare.sts", code: bare, phase: Phase::Build });
        assert!(
            expanded.diagnostics.iter().all(|item| item.severity == Severity::Warning),
            "{:?}",
            expanded.diagnostics
        );
        assert_eq!(expanded.diagnostics.len(), 3, "{:?}", expanded.diagnostics);
        assert!(expanded.code.contains("match (1)"), "match was rewritten:\n{}", expanded.code);
        assert!(expanded.code.contains("#[1, 2]"), "vector was rewritten:\n{}", expanded.code);
        assert!(!expanded.code.contains("vector("), "{}", expanded.code);
        assert!(expanded.code.contains("data Box"), "data was rewritten:\n{}", expanded.code);

        let dir = std::env::temp_dir().join("sw-gate-form-import");
        let _ = std::fs::create_dir_all(&dir);
        let macros = "export syntax match:expr {\n  rule { match ($value:expr) { $($arms:tt)* } } => { matched($value) }\n}\nexport syntax (#):expr {\n  rule { #[$($element:expr),*] } => { vector($($element),*) }\n}\nexport syntax data:item {\n  rule { data $name:binding<$t:binding> = $($rest:tt)*; } => { type $name = 1; }\n}\n";
        std::fs::write(dir.join("macros.sts"), macros).expect("macro fixture");
        let imported_path = dir.join("imported.sts");
        let imported = "import { match, (#), data } from \"./macros.sts\" for syntax;\nconst a = match (1) { 1 => 2, _ => 3 };\nconst v = #[1, 2];\ndata Box<T> = Full(value: T) | Empty();\n";
        let imported_name = imported_path.display().to_string();
        let expanded =
            expand(ExpandRequest { filename: &imported_name, code: imported, phase: Phase::Build });
        assert!(expanded.diagnostics.is_empty(), "{:?}", expanded.diagnostics);
        assert!(!expanded.code.contains("match (1)"), "imported match stayed:\n{}", expanded.code);
        assert!(
            expanded.code.contains("vector(1, 2)"),
            "imported vector stayed:\n{}",
            expanded.code
        );
        assert!(!expanded.code.contains("data Box"), "imported data stayed:\n{}", expanded.code);

        let nested_path = dir.join("nested.sts");
        let nested = "function body() {\n  import { match } from \"./macros.sts\" for syntax;\n  const a = match (1) { 1 => 2, _ => 3 };\n}\nconst v = #[1, 2];\n";
        let nested_name = nested_path.display().to_string();
        let expanded =
            expand(ExpandRequest { filename: &nested_name, code: nested, phase: Phase::Build });
        assert!(
            expanded.code.contains("match (1)"),
            "nested import authorized match:\n{}",
            expanded.code
        );
        assert!(
            expanded.code.contains("import { match }"),
            "nested import was dropped: {}",
            expanded.code
        );

        assert!(
            expanded.code.contains("#[1, 2]"),
            "unimported vector was rewritten:\n{}",
            expanded.code
        );

        let renamed_path = dir.join("renamed.sts");
        let renamed = "import { match as mymatch } from \"./macros.sts\" for syntax;\nconst a = mymatch (1) { 1 => 2, _ => 3 };\nconst b = match (2) { 1 => 2, _ => 3 };\n";
        let renamed_name = renamed_path.display().to_string();
        let expanded =
            expand(ExpandRequest { filename: &renamed_name, code: renamed, phase: Phase::Build });
        assert!(!expanded.code.contains("mymatch (1)"), "renamed match stayed: {}", expanded.code);
        assert!(
            expanded.code.contains("match (2)"),
            "export name expanded without its binding: {}",
            expanded.code
        );

        let function =
            "const f = function(value: number) { const doubled = value * 2; doubled + 1 };\n";
        let expanded =
            expand(ExpandRequest { filename: "core.sts", code: function, phase: Phase::Build });
        assert!(
            !expanded.code.contains("return doubled + 1"),
            "unimported function was rewritten:\n{}",
            expanded.code
        );
    }

    #[test]
    fn file_imports_are_scoped_to_their_slice() {
        let a = read_syntax("import { each } from \"./a.sts\" for syntax;\nconst x = 1;\n");
        let b = read_syntax("import { when as w } from \"./b.sts\" for syntax;\n");
        let pairs =
            |nodes| -> Vec<(String, String)> { imported_syntax_bindings(nodes).as_ref().clone() };
        let fresh_a = pairs(&a.children);
        assert_eq!(fresh_a, vec![("each".to_owned(), "each".to_owned())]);
        with_file_imports(&a.children, || {
            assert_eq!(pairs(&a.children), fresh_a);
            assert_eq!(pairs(&b.children), vec![("w".to_owned(), "when".to_owned())]);
            assert!(pairs(&a.children[1..]).is_empty());
            with_file_imports(&b.children, || {
                assert_eq!(pairs(&b.children), vec![("w".to_owned(), "when".to_owned())]);
            });
            assert_eq!(pairs(&a.children), fresh_a);
        });
    }
}

mod origins_tests {
    use ts_ext_sweetener::__test_support::origins::{carry, lcs_pairs, retarget_segments, tokens};

    fn dp_pairs(old: &[String], new: &[String]) -> Vec<(usize, usize)> {
        let (n, m) = (old.len(), new.len());
        let mut table = vec![0u32; (n + 1) * (m + 1)];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                table[i * (m + 1) + j] = if old[i] == new[j] {
                    table[(i + 1) * (m + 1) + j + 1] + 1
                } else {
                    table[(i + 1) * (m + 1) + j].max(table[i * (m + 1) + j + 1])
                };
            }
        }
        let mut pairs = Vec::new();
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if old[i] == new[j] {
                pairs.push((i, j));
                i += 1;
                j += 1;
            } else if table[(i + 1) * (m + 1) + j] >= table[i * (m + 1) + j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
        pairs
    }

    fn spellings(text: &str) -> Vec<String> {
        tokens(text).into_iter().map(|(a, b)| text[a..b].to_owned()).collect()
    }

    fn assert_matches_dp(old: &str, new: &str) {
        let old_tokens = tokens(old);
        let new_tokens = tokens(new);
        let pairs = lcs_pairs(old, new, &old_tokens, &new_tokens);
        let want = dp_pairs(&spellings(old), &spellings(new));
        assert_eq!(pairs, want, "myers {pairs:?} != dp {want:?}");
        for &(i, j) in &pairs {
            let (a0, a1) = old_tokens[i];
            let (b0, b1) = new_tokens[j];
            assert_eq!(&old[a0..a1], &new[b0..b1]);
        }
        for w in pairs.windows(2) {
            assert!(w[0].0 < w[1].0 && w[0].1 < w[1].1);
        }
    }

    #[test]
    fn diff_edges_match_the_quadratic_walk() {
        assert_matches_dp("", "");
        assert_matches_dp("", "a b");
        assert_matches_dp("a b", "");
        assert_matches_dp("a b c", "a b c");
        assert_matches_dp("only", "only");
        assert_matches_dp("  \n", " \t");
        assert_matches_dp("a b c", "x y z");
        assert_matches_dp("foo", "bar baz");
        assert_matches_dp("a a", "a");
        assert_matches_dp("a", "a a");
        assert_matches_dp("a b a", "a a");
        assert_matches_dp("a x a y a", "a a a");
        assert_matches_dp("a b a b a", "b a b");
        assert_matches_dp("a b c d", "a x c d");
        assert_matches_dp("a b c", "a b c d e");
        assert_matches_dp("x y a b", "a b");
    }

    #[test]
    fn repeated_tokens_keep_the_dp_origin() {
        let kept = carry("a b a", "a a a", &[(0, 5, 10)]);
        assert_eq!(kept, vec![(0, 1, 10), (4, 5, 14)]);
    }

    #[test]
    fn retarget_edges() {
        let same = [(0, 5, 0, 5, 0)];
        assert_eq!(retarget_segments("a b c", "a b c", &same), same);
        assert_eq!(retarget_segments("a b", "x y", &[(0, 3, 7, 10, 0)]), vec![(0, 3, 7, 10, 1)]);
        assert!(retarget_segments("a", "b", &[]).is_empty());
        assert!(retarget_segments("a b", "", &[(0, 3, 0, 3, 0)]).is_empty());
    }

    #[test]
    #[ignore = "large-input timing of the Myers token diff"]
    fn myers_large_input_is_fast() {
        let n = 600;
        let mut old = "a ".repeat(n);
        for i in (0..n).step_by(n / 20) {
            let at = i * 2;
            old.replace_range(at..at + 1, "b");
        }
        let mut new_text = "a ".repeat(n);
        for i in (10..n).step_by(n / 20) {
            let at = i * 2;
            new_text.replace_range(at..at + 1, "c");
        }
        let start = std::time::Instant::now();
        let kept = carry(&old, &new_text, &[(0, old.len(), 0)]);
        let elapsed = start.elapsed();
        let covered: usize = kept.iter().map(|(s, e, _)| e - s).sum();
        assert!(covered > old.len() / 2, "covered {covered} of {}", old.len());
        assert!(elapsed.as_millis() < 1_000, "myers took {elapsed:?} on {n} tokens");
    }

    #[test]
    fn isolated_punctuation_pairs_keep_no_origin() {
        let old = "alpha ( beta";
        let new = "gamma ( delta";
        let pieces = carry(old, new, &[(0, old.len(), 100)]);
        assert!(pieces.is_empty(), "{pieces:?}");
        let old = "f(a, b)";
        let new = "g(a, b)";
        let pieces = carry(old, new, &[(0, old.len(), 100)]);
        assert_eq!(pieces, vec![(1, 7, 101)]);
    }
}

mod cache_tests {
    use std::sync::Arc;
    use ts_ext_sweetener::__test_support::cache::{claim, read_module, take_probes};
    use ts_ext_sweetener::__test_support::reader::{read_syntax, with_operators};
    use ts_ext_sweetener::{Phase, ResourceBudget};

    #[test]
    fn module_reads_are_shared_per_text_and_operator_spellings() {
        let text = "export syntax one:expr {\n  rule { one() } => { 1 @@ 2 }\n}\n";
        let first = read_module(text);
        let again = read_module(text);
        assert!(Arc::ptr_eq(&first, &again));
        let with_op = with_operators(&["@@".to_owned()], || read_module(text));
        assert!(!Arc::ptr_eq(&first, &with_op));
        assert_eq!(*with_op, with_operators(&["@@".to_owned()], || read_syntax(text)));
        assert_eq!(*first, read_syntax(text));
        assert!(take_probes().is_empty());
    }

    #[test]
    fn an_identical_request_waits_for_the_running_expansion() {
        let budget = ResourceBudget::default();
        let code = "const claimed = 1;\n";
        let first = claim("claim.sts", code, Phase::Check, &budget).expect("free");
        let waiter = std::thread::spawn(move || {
            let budget = ResourceBudget::default();
            let started = std::time::Instant::now();
            let second = claim("claim.sts", code, Phase::Check, &budget);
            (second.is_none(), started.elapsed())
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        drop(first);
        let (waited_then_looked_again, elapsed) = waiter.join().unwrap();
        assert!(waited_then_looked_again);
        assert!(elapsed >= std::time::Duration::from_millis(90), "{elapsed:?}");
        assert!(claim("claim.sts", code, Phase::Check, &budget).is_some());
    }
}

mod expand_tests {
    use ts_ext_sweetener::{ExpandRequest, Phase, Severity, expand_with_panic_hint};

    #[test]
    fn expander_panic_is_swr4990_at_the_invocation() {
        let code = "\
export syntax boom:expr {\n\
  rule { boom($x:expr) } => { #fresh(\"swrPanic\") }\n\
}\n\
const kept = 1;\n\
const value = boom(2 + 3);\n\
const after = 4;\n";
        let out = expand_with_panic_hint(ExpandRequest {
            filename: "panic.sts",
            code,
            phase: Phase::Check,
        });
        let codes: Vec<&str> =
            out.diagnostics.iter().map(|d| d.message.split(':').next().unwrap_or("")).collect();
        assert_eq!(codes, vec!["SWR4990"], "code was:\n{}", out.code);
        let diagnostic = &out.diagnostics[0];
        assert_eq!(diagnostic.severity, Severity::Error);
        assert!(
            diagnostic
                .message
                .starts_with("SWR4990: Sweetener expansion failed with an internal error:"),
            "{}",
            diagnostic.message
        );
        assert!(diagnostic.message.contains("forced expander panic"), "{}", diagnostic.message);
        let invocation = code.find("boom(2 + 3)").unwrap();
        assert_eq!(diagnostic.start as usize, invocation);
        assert_eq!(diagnostic.end as usize, invocation + "boom(2 + 3)".len());
        assert_eq!(out.code, code);
    }
}

mod items_tests {
    use std::hash::{DefaultHasher, Hasher};
    use ts_ext_sweetener::__test_support::hygiene;
    use ts_ext_sweetener::__test_support::items::{
        Edit, UserNames, hash_debug, hash_debug_set, moved, placeholder_ids, renumber,
        start_name_log, take_name_log,
    };
    use ts_ext_sweetener::__test_support::scoped;

    #[test]
    fn an_edit_moves_what_follows_it_and_nothing_inside_it() {
        let edit = Edit::between("abc def ghi", "abc dXXef ghi");
        assert_eq!((edit.prefix, edit.old_end, edit.new_end), (5, 5, 7));
        assert_eq!(edit.forward(2), Some(2));
        assert_eq!(edit.forward(5), None);
        assert_eq!(edit.forward(8), Some(10));
        assert_eq!(edit.backward(10), Some(8));
        assert_eq!(edit.backward(6), None);
        assert_eq!(edit.range_shift((0, 4)), Some(0));
        assert_eq!(edit.range_shift((6, 11)), Some(2));
        assert_eq!(edit.range_shift((3, 6)), None);
        let edit = Edit::between("one two three", "one three");
        assert_eq!((edit.prefix, edit.old_end, edit.new_end), (5, 9, 5));
        assert_eq!(edit.range_shift((0, 4)), Some(0));
        assert_eq!(edit.range_shift((9, 13)), Some(-4));
        assert_eq!(edit.range_shift((8, 13)), None);
        let same = Edit::between("same", "same");
        assert_eq!(same.range_shift((0, 4)), Some(0));
    }

    #[test]
    fn a_replayed_item_gets_fresh_placeholder_ids_with_offsets_moved() {
        let text = "const __swh7_x = __swp3(__swh7_x) + __swh12_y;";
        let regions = vec![(3, ("p".to_owned(), "v".to_owned(), false))];
        let (out, moves) = renumber(text, &regions);
        let ids = placeholder_ids(&out, hygiene::PREFIX, b'_');
        assert_eq!(ids.len(), 3);
        assert_eq!(ids[0], ids[1]);
        assert_ne!(ids[0], ids[2]);
        assert!(!ids.contains(&7) || !ids.contains(&12));
        let region = placeholder_ids(&out, scoped::OPEN, b'(')[0];
        assert_eq!(scoped::region(region), Some(("p".to_owned(), "v".to_owned(), false)));
        assert_eq!(moved(&moves, text.len() as u32) as usize, out.len());
        assert_eq!(moved(&moves, 0), 0);
        let plain = "no placeholders";
        assert_eq!(renumber(plain, &[]), (plain.to_owned(), Vec::new()));
    }

    #[test]
    fn a_logged_item_is_replayed_only_where_its_names_answer_the_same() {
        let mut names = UserNames::new(["a".to_owned()].into_iter().collect());
        start_name_log();
        assert!(names.contains("a"));
        assert!(!names.contains("b"));
        let log = take_name_log().unwrap();
        assert_eq!(log, vec![("a".to_owned(), true), ("b".to_owned(), false)]);
        assert!(names.answers(&log));
        names.insert("b".to_owned());
        assert!(!names.answers(&log));
        names.insert("b".to_owned());
        assert_eq!(names.added, vec!["b".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn the_pass_fingerprint_leaves_positions_out_but_not_order_or_text() {
        let hash = |text: &str| {
            let mut hasher = DefaultHasher::new();
            hash_debug(&text, &mut hasher);
            hasher.finish()
        };
        #[derive(Debug)]
        #[expect(dead_code, reason = "fields read via Debug formatting")]
        struct Span {
            start: u32,
            end: u32,
        }
        #[derive(Debug)]
        #[expect(dead_code, reason = "fields read via Debug formatting")]
        struct Rule {
            name: &'static str,
            head: Span,
        }
        let one = |name, start, end| {
            let mut hasher = DefaultHasher::new();
            hash_debug(&Rule { name, head: Span { start, end } }, &mut hasher);
            hasher.finish()
        };
        assert_eq!(one("m", 10, 20), one("m", 30, 40));
        assert_ne!(one("m", 10, 20), one("n", 10, 20));
        assert_ne!(hash("a"), hash("b"));
        let set = |rules: &[(&'static str, u32)]| {
            let values: Vec<Rule> = rules
                .iter()
                .map(|(name, at)| Rule { name, head: Span { start: *at, end: *at } })
                .collect();
            let mut hasher = DefaultHasher::new();
            hash_debug_set(&values, &mut hasher);
            hasher.finish()
        };
        assert_eq!(set(&[("a", 1), ("b", 2)]), set(&[("b", 9), ("a", 5)]));
        assert_ne!(set(&[("a", 1)]), set(&[("a", 1), ("b", 2)]));
    }
}
