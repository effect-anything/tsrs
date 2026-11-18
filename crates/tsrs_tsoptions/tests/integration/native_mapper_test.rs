use tsrs_tsoptions::contentmappers::resolve_content_mapper_manifest;
use tsrs_tsoptions::native_mapper::node_platform;
use tsrs_tsoptions::tsoptionstest::new_vfs_parse_config_host;

/// An installed platform package replaces `exec`; a missing one keeps it.
#[test]
fn an_installed_platform_package_replaces_the_launcher() {
    let (platform, arch) = node_platform();
    let native_pkg = format!("@x/m-{platform}-{arch}");
    let native_pkg_json_path = format!("/project/node_modules/{native_pkg}/package.json");
    let native_pkg_json = format!(r#"{{"name":"{native_pkg}"}}"#);
    let native_bin_path = format!("/project/node_modules/{native_pkg}/lib/m");
    let files = [
        (
            "/project/node_modules/@x/m/package.json",
            r#"{"name":"@x/m","version":"1.0.0","typescript":{"contentMapper":{"exec":["./bin/m"]}}}"#,
        ),
        (
            "/project/node_modules/missing/package.json",
            r#"{"name":"missing","typescript":{"contentMapper":{"exec":["./bin/m"]}}}"#,
        ),
        (native_pkg_json_path.as_str(), native_pkg_json.as_str()),
        (native_bin_path.as_str(), ""),
    ];
    let host = new_vfs_parse_config_host(&files, "/project", true);
    let containing = "/project/tsconfig.json";

    let (manifest, _, diagnostic) = resolve_content_mapper_manifest(host, containing, "@x/m");
    assert!(diagnostic.is_none());
    assert_eq!(manifest.exec, vec![native_bin_path]);

    let (manifest, _, diagnostic) = resolve_content_mapper_manifest(host, containing, "missing");
    assert!(diagnostic.is_none());
    assert_eq!(manifest.exec, vec!["./bin/m".to_string()]);
}
