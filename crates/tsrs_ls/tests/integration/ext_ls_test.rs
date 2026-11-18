use tsrs_ast::{SourceFileParseOptions, new_diagnostic};
use tsrs_contentmapper::{Mapper, TransformResultFiles, parse_result};
use tsrs_core::new_text_range;
use tsrs_core::tspath::Path;
use tsrs_diagnostics as diagnostics;
use tsrs_ls::ext_ls::{
    invocation, is_synthesized_content_mapped_diagnostic, with_expansion_context,
};
use tsrs_spanmap::{Expansion, Feature, Kind, Segment};

#[test]
fn diagnostics_in_macro_output_point_at_the_invocation() {
    // `const a = twice(value);` expanded to `const a = [value, value];`:
    // `[` and `, value]` are synthesized, `value` is copied.
    let original = "const a = twice(value);";
    let text = "const a = [value, value];";
    let map = tsrs_spanmap::new(&[
        Segment {
            virtual_end: 10,
            original_end: 10,
            kind: Kind::VERBATIM,
            features: Feature::All,
            ..Default::default()
        },
        Segment {
            virtual_start: 11,
            virtual_end: 16,
            original_start: 16,
            original_end: 21,
            kind: Kind::VERBATIM,
            features: Feature::All,
        },
    ])
    .with_expansions(vec![Expansion {
        virtual_start: 10,
        virtual_end: 24,
        original_start: 10,
        original_end: 22,
        label: "macro `twice`".to_string(),
    }]);
    let result = TransformResultFiles {
        text: text.to_string(),
        virtual_extension: ".ts".to_string(),
        mappings: Some(map),
        ..Default::default()
    };
    let options = SourceFileParseOptions {
        file_name: "/a.sts".to_string(),
        path: Path("/a.sts".into()),
        ..Default::default()
    };
    let files = parse_result(options, original, &Mapper::default(), "", result).unwrap();
    let file = files.canonical.unwrap();
    // `, value]` (16..24) is synthesized: it belongs to the invocation,
    // not the top of the file.
    let synthesized = new_diagnostic(
        Some(file),
        new_text_range(16, 24),
        &diagnostics::VIRTUAL_CODE_PRODUCED_BY_THE_CONTENT_MAPPER_0_HAS_PROBLEMS_WITH_NO_CORRESPONDING_LOCATION_IN_THIS_FILE,
        &[&"m"],
    );
    assert!(!is_synthesized_content_mapped_diagnostic(synthesized));
    let with_context = with_expansion_context(synthesized);
    let related = with_context.related_information();
    assert_eq!(related.len(), 1);
    assert_eq!(related[0].loc(), Some(new_text_range(10, 22)));
    // Outside every expansion nothing is added.
    let plain = new_diagnostic(
        Some(file),
        new_text_range(6, 7),
        &diagnostics::VIRTUAL_CODE_PRODUCED_BY_THE_CONTENT_MAPPER_0_HAS_PROBLEMS_WITH_NO_CORRESPONDING_LOCATION_IN_THIS_FILE,
        &[&"m"],
    );
    assert_eq!(with_expansion_context(plain).related_information().len(), 0);
    let map = file.span_map().unwrap();
    assert_eq!(invocation(&map, new_text_range(16, 24)), Some(new_text_range(10, 22)));
}
