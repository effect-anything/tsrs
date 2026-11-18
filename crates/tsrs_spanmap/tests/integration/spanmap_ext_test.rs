use tsrs_core::TextRange;
use tsrs_spanmap::{
    Expansion, Feature, Fidelity, Kind, MAPPER_NAME, MapperInfo, Segment, SpanMap, SyntaxBinding,
    SyntaxImport, emits_javascript, new, virtual_to_original_span,
};

#[test]
fn cross_segment_ranges_are_unions_and_exact_when_chained() {
    let seg = |vs, ve, os, oe, kind| Segment {
        virtual_start: vs,
        virtual_end: ve,
        original_start: os,
        original_end: oe,
        kind,
        features: Feature::All.difference(Feature::None),
    };
    // Macro output `[b, a]` from `pair(a, b)`: originals out of order.
    let m = new(&[
        seg(0, 1, 0, 9, Kind::ATOM),
        seg(1, 2, 8, 9, Kind::VERBATIM),
        seg(2, 4, 0, 9, Kind::ATOM),
        seg(4, 5, 5, 6, Kind::VERBATIM),
        seg(5, 6, 0, 9, Kind::ATOM),
    ]);
    let (span, fidelity) = virtual_to_original_span(Some(&m), TextRange::new(1, 5));
    assert_eq!((span.source_pos(), span.source_end()), (Some(0), Some(9)));
    assert_eq!(fidelity, Fidelity::Approximate);
    let pieces = SpanMap::virtual_to_original_spans(Some(&m), TextRange::new(1, 5));
    let spans: Vec<_> = pieces
        .iter()
        .map(|p| (p.span.source_pos().unwrap(), p.span.source_end().unwrap(), p.fidelity))
        .collect();
    assert_eq!(
        spans,
        vec![(8, 9, Fidelity::Exact), (0, 9, Fidelity::Atom), (5, 6, Fidelity::Exact)]
    );
    // Only verbatim, out of order, no atoms: Go gave (8, 6) clamped to (8, 8).
    let m = new(&[seg(0, 1, 8, 9, Kind::VERBATIM), seg(1, 2, 5, 6, Kind::VERBATIM)]);
    let (span, _) = virtual_to_original_span(Some(&m), TextRange::new(0, 2));
    assert_eq!((span.source_pos(), span.source_end()), (Some(5), Some(9)));
    // Two verbatim segments that continue each other are one exact span.
    let m = new(&[seg(0, 3, 10, 13, Kind::VERBATIM), seg(3, 6, 13, 16, Kind::VERBATIM)]);
    let (span, fidelity) = virtual_to_original_span(Some(&m), TextRange::new(1, 5));
    assert_eq!(
        (span.source_pos(), span.source_end(), fidelity),
        (Some(11), Some(15), Fidelity::Exact)
    );
}

#[test]
fn mapper_info_and_emits_javascript_helpers() {
    assert!(emits_javascript(""));
    assert!(emits_javascript(MAPPER_NAME));
    assert!(!emits_javascript("some-external-mapper@1.0.0"));

    let base = new(&[]);
    let attached = base.with_mapper_info(MapperInfo {
        expansions: vec![
            Expansion {
                virtual_start: 10,
                virtual_end: 15,
                original_start: 20,
                original_end: 25,
                label: "inner".to_string(),
            },
            Expansion {
                virtual_start: 10,
                virtual_end: 30,
                original_start: 18,
                original_end: 40,
                label: "outer".to_string(),
            },
        ],
        dependencies: vec!["/p/m.sts".to_string()],
        syntax_imports: vec![SyntaxImport {
            specifier: "./m.sts".to_string(),
            resolved: Some("/p/m.sts".to_string()),
            bindings: vec![SyntaxBinding { imported: "twice".to_string(), local: "t".to_string() }],
            shadows_core: false,
            original_start: 0,
            original_end: 17,
        }],
    });
    assert!(SpanMap::mapper_info(Some(&base)).is_empty());
    assert_eq!(SpanMap::expansions(Some(&attached))[0].label, "outer");
    assert_eq!(SpanMap::expansions(Some(&attached))[1].label, "inner");
    let containing =
        SpanMap::expansions_containing_virtual(Some(&attached), TextRange::new(11, 14));
    assert_eq!(containing.len(), 2);
    assert_eq!(containing[0].label, "inner");
    assert_eq!(containing[1].label, "outer");
    let at_orig = SpanMap::expansions_at_original(Some(&attached), 22);
    assert_eq!(at_orig.len(), 2);
    assert_eq!(at_orig[0].label, "inner");
    assert_eq!(SpanMap::dependencies(Some(&attached)), ["/p/m.sts"]);
    assert_eq!(SpanMap::syntax_imports(Some(&attached)).len(), 1);
}
