//! Check and LSP see the author's `+`. This locks the type that produces.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The debug `tsrs` CLI binary: `TSC_BIN` when set, else `<target>/debug/tsrs`,
/// where the target directory is `CARGO_TARGET_DIR`, else `cargo metadata`'s
/// `target_directory`, else `<root>/target`.
fn tsc() -> PathBuf {
    if let Some(bin) = std::env::var_os("TSC_BIN").filter(|bin| !bin.is_empty()) {
        return PathBuf::from(bin);
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(PathBuf::from)
        .expect("workspace root");
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .filter(|dir| !dir.is_empty())
        .map(|dir| root.join(dir))
        .or_else(|| metadata_target_dir(&root))
        .unwrap_or_else(|| root.join("target"));
    target.join("debug").join(format!("tsrs{}", std::env::consts::EXE_SUFFIX))
}

/// `target_directory` from `cargo metadata`, if cargo runs and says.
fn metadata_target_dir(root: &Path) -> Option<PathBuf> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(root)
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let key = "\"target_directory\":\"";
    let start = text.find(key)? + key.len();
    let end = start + text[start..].find('"')?;
    Some(PathBuf::from(text[start..end].replace("\\\\", "\\")))
}

#[test]
fn vec2f_plus_is_vec2f_and_a_number_is_an_error() {
    let bin = tsc();
    assert!(
        bin.is_file(),
        "tsc (the cargo `tsrs` binary) missing at {}: build it with `cargo build -p tsrs_cli --bin tsrs`, or set TSC_BIN",
        bin.display()
    );
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/overload");
    let output = Command::new(&bin)
        .args(["--noEmit", "--pretty", "false", "-p"])
        .arg(&fixture)
        .output()
        .unwrap_or_else(|err| panic!("run tsc (the cargo `tsrs` binary): {err}"));
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        report.contains("Type 'Vec2f' is not assignable to type 'number'"),
        "a + b must be Vec2f, not number:\n{report}"
    );
    assert!(
        report.contains("Operator '+' cannot be applied to types 'Vec2f' and 'number'"),
        "Vec2f + number must be rejected:\n{report}"
    );
    assert!(
        !report.contains("cannot be applied to types 'Vec2f' and 'Vec2f'")
            && !report.contains("cannot be applied to types 'number' and 'Vec2f'"),
        "a legal overload was rejected:\n{report}"
    );
    let errors = report.matches("error TS").count();
    assert_eq!(errors, 2, "expected the two intentional errors only:\n{report}");
}

#[test]
fn emit_rewrites_js_and_map_without_touching_dts() {
    let bin = tsc();
    assert!(bin.is_file(), "tsc missing at {}", bin.display());
    let dir = std::env::temp_dir().join(format!("tsrs_ext_emit_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let runtime = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/overload/tsover-runtime.d.ts");
    std::fs::copy(runtime, dir.join("tsover-runtime.d.ts")).expect("copy runtime d.ts");
    std::fs::write(
        dir.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "strict": true,
    "target": "esnext",
    "module": "esnext",
    "moduleResolution": "bundler",
    "sourceMap": true,
    "declaration": true,
    "outDir": "dist",
    "skipLibCheck": true
  },
  "files": ["main.ts"]
}"#,
    )
    .expect("write tsconfig");
    std::fs::write(
        dir.join("main.ts"),
        r#""use tsover";
import { Operator } from "./tsover-runtime";

export class Vec2f {
  x: number;
  y: number;
  constructor(x: number, y: number) {
    this.x = x;
    this.y = y;
  }
  [Operator.plus](lhs: Vec2f, rhs: Vec2f): Vec2f {
    return new Vec2f(lhs.x + rhs.x, lhs.y + rhs.y);
  }
}

export function marked(a: Vec2f, b: Vec2f): Vec2f {
  "use gpu";
  return a + b;
}
"#,
    )
    .expect("write main.ts");

    let output = Command::new(&bin)
        .args(["--pretty", "false", "-p"])
        .arg(&dir)
        .output()
        .expect("run tsrs emit");
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "expected clean emit:\n{report}");

    let js = std::fs::read_to_string(dir.join("dist/main.js")).expect("read main.js");
    let map = std::fs::read_to_string(dir.join("dist/main.js.map")).expect("read main.js.map");
    let dts = std::fs::read_to_string(dir.join("dist/main.d.ts")).expect("read main.d.ts");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(js.contains("__tsover_add("), "expected operator rewrite in main.js:\n{js}");
    assert!(js.contains("__TYPEGPU_META__"), "expected TypeGPU metadata in main.js:\n{js}");
    assert!(
        js.contains("//# sourceMappingURL=main.js.map"),
        "expected sourceMappingURL trailer in main.js:\n{js}"
    );
    assert!(map.contains("\"sources\":[\"../main.ts\"]"), "expected valid source map:\n{map}");
    assert!(
        !dts.contains("__tsover_add") && !dts.contains("__TYPEGPU_META__"),
        "declaration output must not contain runtime wrappers:\n{dts}"
    );
}
