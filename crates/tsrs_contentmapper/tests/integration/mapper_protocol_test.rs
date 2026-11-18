use tsrs_ast as ast;
use tsrs_contentmapper::mapper_protocol::{
    DiagnosticExt, ExpansionInformation, MappedOutputExt, RelatedInformation, attach_mapper_info,
    mapper_info,
};
use tsrs_contentmapper::{Diagnostic as MapperDiagnostic, MappedOutput, ProtocolJson};
use tsrs_core::{TextRange, json};
use tsrs_diagnostics::Category;
use tsrs_spanmap::SpanMap;

#[test]
fn diagnostic_related_information_round_trips() {
    let input = r#"{"messageText":"m","start":1,"length":2,"code":4001,"category":1,"relatedInformation":[{"messageText":"r","start":5,"length":3}]}"#;
    let diagnostic = MapperDiagnostic::unmarshal_json(json::unmarshal(input).unwrap(), "").unwrap();
    assert_eq!(diagnostic.ext.category, Some(1));
    assert_eq!(
        diagnostic.ext.related_information,
        vec![RelatedInformation { message_text: "r".to_string(), start: 5, length: 3 }]
    );
    let out = json::marshal(&diagnostic.marshal_json()).unwrap();
    assert!(
        out.contains(r#""relatedInformation":[{"messageText":"r","start":5,"length":3}]"#),
        "{out}"
    );
}

#[test]
fn mapper_diagnostic_category_defaults_to_error() {
    let of = |category| DiagnosticExt { category, ..Default::default() }.category();
    assert_eq!(of(None), Category::Error);
    assert_eq!(of(Some(1)), Category::Error);
    assert_eq!(of(Some(0)), Category::Warning);
    assert_eq!(of(Some(2)), Category::Suggestion);
    assert_eq!(of(Some(3)), Category::Message);
    assert_eq!(of(Some(9)), Category::Error);
}

#[test]
fn diagnostic_ext_apply_attaches_related_information() {
    let diag = ast::new_external_diagnostic(
        None,
        TextRange::new(1, 3),
        "ext",
        Category::Error,
        4001,
        "main",
    );
    let ext = DiagnosticExt {
        category: Some(0),
        related_information: vec![RelatedInformation {
            message_text: "secondary".to_string(),
            start: 5,
            length: 3,
        }],
    };
    ext.apply(Some(diag), u32::try_from);
    assert_eq!(diag.category(), Category::Warning);
    let rel = diag.related_information();
    assert_eq!(rel.len(), 1);
    assert_eq!(rel[0].message_text(), "secondary");
    assert_eq!(rel[0].pos(), Some(5));
    assert_eq!(rel[0].end(), Some(8));
}

#[test]
fn expansions_decode_and_attach_to_the_span_map() {
    // `twice(value)` at 10..22 expanded to `[value, value]` at 10..24.
    let input = r#"{"text":"const a = [value, value];","extension":".ts","mappings":[[0,10,0,10,0],[10,14,10,12,1]],"expansions":[{"virtualStart":10,"virtualLength":14,"originalStart":10,"originalLength":12,"label":"macro `twice`"}]}"#;
    let output = MappedOutput::unmarshal_json(json::unmarshal(input).unwrap(), "").unwrap();
    assert_eq!(output.ext.expansions.len(), 1);
    let encoded = json::marshal(&output.marshal_json()).unwrap();
    assert!(encoded.contains(r#""expansions":[{"virtualStart":10"#), "{encoded}");
    let raw_mappings = json::marshal(output.mappings.as_ref().unwrap()).unwrap();
    let mut mappings = Some(tsrs_spanmap::unmarshal(raw_mappings.as_bytes()).unwrap());
    attach_mapper_info(&mut mappings, &output.ext, u32::try_from, u32::try_from);
    let map = mappings.as_deref();
    let inside = SpanMap::expansions_containing_virtual(map, TextRange::new(11, 16));
    assert_eq!(inside.len(), 1);
    assert_eq!(inside[0].label, "macro `twice`");
    assert_eq!((inside[0].original_start, inside[0].original_end), (10, 22));
    assert_eq!(SpanMap::expansions_containing_virtual(map, TextRange::new(0, 5)).len(), 0);
    assert_eq!(SpanMap::expansions_at_original(map, 12).len(), 1);
}

#[test]
fn dependencies_decode_and_attach_to_the_span_map() {
    let input = r#"{"text":"const a = 1;","extension":".ts","mappings":[[0,12,0,12,0]],"dependencies":["/p/macros.sts"]}"#;
    let output = MappedOutput::unmarshal_json(json::unmarshal(input).unwrap(), "").unwrap();
    assert_eq!(output.ext.dependencies, vec!["/p/macros.sts".to_string()]);
    let encoded = json::marshal(&output.marshal_json()).unwrap();
    assert!(encoded.contains(r#""dependencies":["/p/macros.sts"]"#), "{encoded}");
    let raw_mappings = json::marshal(output.mappings.as_ref().unwrap()).unwrap();
    let mut mappings = Some(tsrs_spanmap::unmarshal(raw_mappings.as_bytes()).unwrap());
    attach_mapper_info(&mut mappings, &output.ext, u32::try_from, u32::try_from);
    assert_eq!(SpanMap::dependencies(mappings.as_deref()), ["/p/macros.sts"]);
}

#[test]
fn syntax_imports_decode_and_attach_to_the_span_map() {
    let input = r#"{"text":"x;","extension":".ts","mappings":[[0,2,50,2,0]],"syntaxImports":[{"specifier":"./m.sts","resolved":"/p/m.sts","bindings":[{"imported":"twice","local":"t"}],"shadowsCore":true,"originalStart":0,"originalLength":48}]}"#;
    let output = MappedOutput::unmarshal_json(json::unmarshal(input).unwrap(), "").unwrap();
    let encoded = json::marshal(&output.marshal_json()).unwrap();
    assert!(
        encoded.contains(r#""syntaxImports":[{"specifier":"./m.sts","resolved":"/p/m.sts","bindings":[{"imported":"twice","local":"t"}],"shadowsCore":true,"originalStart":0,"originalLength":48}]"#),
        "{encoded}"
    );
    let raw_mappings = json::marshal(output.mappings.as_ref().unwrap()).unwrap();
    let mut mappings = Some(tsrs_spanmap::unmarshal(raw_mappings.as_bytes()).unwrap());
    attach_mapper_info(&mut mappings, &output.ext, u32::try_from, u32::try_from);
    let imports = SpanMap::syntax_imports(mappings.as_deref());
    assert_eq!(imports.len(), 1);
    assert_eq!(imports[0].resolved.as_deref(), Some("/p/m.sts"));
    assert_eq!(imports[0].bindings[0].local, "t");
    assert_eq!((imports[0].original_start, imports[0].original_end), (0, 48));
}

#[test]
fn a_shared_map_still_gets_the_report() {
    let map = tsrs_spanmap::unmarshal(b"[[0,3,0,3,0]]").unwrap();
    let keep = map;
    let mut mappings = Some(map);
    let ext = MappedOutputExt {
        dependencies: vec!["/p/b.sts".into(), "/p/a.sts".into(), "/p/a.sts".into()],
        ..Default::default()
    };
    attach_mapper_info(&mut mappings, &ext, u32::try_from, u32::try_from);
    let attached = mappings.as_deref();
    assert_eq!(SpanMap::dependencies(attached), ["/p/a.sts", "/p/b.sts"]);
    assert_eq!(attached.unwrap().segments(), keep.segments());
    assert_eq!(SpanMap::dependencies(Some(&keep)), [] as [&str; 0]);
}

#[test]
fn out_of_range_entries_are_dropped() {
    let ext = MappedOutputExt {
        expansions: vec![ExpansionInformation {
            virtual_start: -1,
            virtual_length: 2,
            ..Default::default()
        }],
        ..Default::default()
    };
    assert!(mapper_info(&ext, u32::try_from, u32::try_from).is_empty());
}
