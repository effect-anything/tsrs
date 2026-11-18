# `effect-anything/tsrs` Fork Reference (`@effect-x/tsc`)

本文档记录 `effect-anything/tsrs` 相对于上游 `oxc-project/tsrs` 的基线版本、全部扩展改动边界、上游侵入点清单，以及每次 `git rebase` 上游的标准操作与验证流程。

---

## 1. 上游基线记录（每次 Rebase 后更新此表）

| 上游仓库 | 远程名 / 来源 | 当前基线 Commit / Tag | 日期 | 说明 |
| :--- | :--- | :--- | :--- | :--- |
| [`oxc-project/tsrs`](https://github.com/oxc-project/tsrs) | `origin/main` | `7eb504db5ce54d160a73399fadd30f7f695681c1` | 2026-11-17 | `ci(release): publish npm packages to latest (#1014)`（`tsrs 7.1.0-beta.tsrs2`） |
| [`microsoft/TypeScript`](https://github.com/microsoft/TypeScript) | `npm/tsrs/UPSTREAM.json` | `3e9236361fa97a9e6c685f4833ac457ae2d8c50a` | — | 随上游 `tsrs` 锁定的 TypeScript 7.1.0-beta SDK 基线 |
| [`yielded-dev/effect-tsrs`](https://github.com/yielded-dev/effect-tsrs) | 树内移植至 `crates/ts_ext_effect` | `947f60cac7ff65cf30f001c9f5655f2679ab16bd` (`v0.2.3`) | 2026-10-29 | Effect v3 / v4 130 条类型感知规则（MIT），已适配纯 VFS 与 `.sts` / `.stsx` |

---

## 2. 独立新增目录（Rebase 零冲突区）

以下目录完全由本 Fork 新增，不与上游 `oxc-project/tsrs` 文件重叠：

- **`crates/ts_ext_sweetener/`**：Sweetener（`.sts` / `.stsx`）卫生宏展开引擎、增量缓存与 `SpanMap` 映射生成。
- **`crates/ts_ext_typegpu/`**：`"use tsover"` / `"use gpu"` 运算符重载（`Symbol.operator*`）重写与 TypeGPU `__TYPEGPU_META__` / `__TYPEGPU_AUTONAME__` 元数据注入。
- **`crates/ts_ext_transform/`**：面向 `tsrs_compiler` 的轻量 `precheck` / `rewrite_emitted_javascript` 桥接层（避免循环依赖）。
- **`crates/ts_ext_syntax/`**：
  - `tsc-ext` CLI 二进制（Content Mapper 子进程入口 + 命令行展开/诊断工具）；
  - `tsc-ext.node` N-API 原生扩展（供 `@effect-x/tsc/ext` 与 `@effect-x/tsc/unplugin` 调用）；
  - `crates/ts_ext_syntax/js/`：JS 封装层与 Vite / Rollup / esbuild / Webpack `unplugin` 适配器。
- **`crates/ts_ext_effect/`**：内嵌 `@effect/language-service` 诊断引擎（`TS377xxx`），当 `tsconfig.json` 的 `compilerOptions.plugins` 配置 `{ "name": "@effect/language-service" }` 时在单次类型检查 pass 中原生运行，支持 `.ts` / `.tsx` / `.sts` / `.stsx` 位置回映、`// @effect-diagnostics` 注释指令与 `overrides`。
- **`npm/fork/` & `scripts/fork/`**：`@effect-x/tsc` 与各平台 `@effect-x/tsc-<os>-<arch>` npm 包组装（`pack.mjs`、`npm-pack.sh`）、端到端 tarball 冒烟测试（`npm-test.sh`）与 npm Trusted Publishing 配置脚本（`trust-setup.sh`），完全不触碰上游 `npm/build.mjs`。
- **`.github/workflows/fork-release.yml`**：6 平台矩阵构建（Darwin / Linux glibc / Linux musl / Windows）与 `@effect-x/tsc` 发布工作流。
- **`ext/`**：扩展设计文档（`ext/docs/`）、示例工程（`ext/examples/`）与基准测试（`ext/bench/`）。

---

## 3. 上游 Crate 最小侵入点清单（Rebase 关注区）

为将后续合并冲突降到最低，所有核心逻辑均放在独立扩展模块或 sidecar 文件中，上游原有文件仅保留极薄的转发调用。

### 3.1 Workspace 根配置
- `Cargo.toml`：在 `[workspace.members]` 与 `[workspace.dependencies]` 注册 `ts_ext_sweetener`、`ts_ext_typegpu`、`ts_ext_transform`、`ts_ext_syntax`、`ts_ext_effect`，以及 `napi` / `napi-derive` / `napi-build`。

### 3.2 SpanMap 与 Content Mapper 协议扩展
- **`crates/tsrs_spanmap/`**
  - 新增 `src/spanmap_ext.rs`（保存宏展开区域 `Expansion`、`syntax_imports` 与宏依赖列表 `MapperInfo`）；
  - `src/lib.rs`、`src/spanmap.rs`：暴露 `MAPPER_NAME`、`emits_javascript` 及 `SpanMap::mapper_info` 读写接口。
- **`crates/tsrs_tsoptions/`**
  - 新增 `src/native_mapper.rs`：将 `@effect-x/tsc` 声明的 `./bin/tsc-ext` 自动解析为平台包内的原生 `lib/tsc-ext` 二进制，消除每次启动的 Node 进程开销；
  - `src/contentmappers.rs`（1 行调用 `native_mapper`）、`src/outputpaths.rs`（1 行允许内部 `MAPPER_NAME` 正常产出 `.js`）。
- **`crates/tsrs_contentmapper/`**
  - 新增 `src/mapper_protocol.rs`：解码 `expansions`、`dependencies`、`syntaxImports` 与带 `category` / `relatedInformation` 的扩展诊断；
  - `src/hostimpl.rs`、`src/transform.rs`：挂接 `MappedOutputExt` / `DiagnosticExt` 与 `related_information` 文件归属。

### 3.3 TypeChecker 钩子（运算符重载 + Effect 规则只读查询）
- **`crates/tsrs_checker/`**
  - **独立新增文件**：
    - `src/ext_overload.rs`：`Symbol.operator*` 二元/一元运算符重载类型推导与赋值检查；
    - `src/effect_stability_context.rs`：Effect `apiStabilityLeak` 规则所需的声明稳定性遍历辅助。
  - **上游文件极小改动**：
    - `src/checker_06.rs`（8 行）：在二元/前缀一元表达式检查处调用 `check_overloaded_*`；
    - `src/exports.rs`（末尾追加只读 `*_exported` 访问器，不改动内部结构体字段可见性）；
    - `src/checker.rs`、`src/checker_02.rs`、`src/checker_04.rs`、`src/checker_05.rs`、`src/checker_15.rs`、`src/evaluator.rs`、`src/relater_2.rs`、`src/types.rs`、`src/lib.rs`：暴露 `relation_errors` 与少量 `pub(crate)` / `pub` 查询接口供 `ts_ext_effect` 使用。

### 3.4 Compiler 管线钩子（Precheck / Emit / 宏依赖追踪 / Effect 检查）
- **`crates/tsrs_compiler/`**
  - **独立新增文件**：
    - `src/mapper_deps.rs`：汇总 `.sts` 展开读取的 `for syntax` 宏模块依赖，供 Watch / LSP 失效重算使用；
    - `src/line_map.rs`：UTF-8 字节偏移到行列号转换辅助。
  - **上游文件钩子**：
    - `src/fileloader.rs`：解析普通 TS/TSX 文件前调用 `ts_ext_transform::precheck`（处理 `"use tsover"` / `"use gpu"`）；
    - `src/emitter.rs`：JS 写出阶段调用 `ts_ext_transform::rewrite_emitted_javascript` 注入 TypeGPU 元数据并组合 SourceMap；
    - `src/program.rs`：
      1. 输出路径冲突检查对 `.sts` + `.ts` 同名产物使用虚拟扩展名区分（避免误报 TS5056）；
      2. 挂接 `ProgramEffectState`（`OnceLock`）：在 `get_config_file_parsing_diagnostics` 追加 Effect 配置诊断，在 `get_bind_and_check_diagnostics_with_checker` 追加单文件 Effect 语义诊断，并在启用 `@effect/language-service` 时保留叶节点 Region 以支持跨文件 Effect 规则查询；
    - `src/diagnosticwriter.rs`：内部 `MAPPER_NAME` 诊断前缀保持 `"TS"`。
- **`crates/tsrs_incremental/src/programtosnapshot.rs`**：增量构建快照同步携带内部 `precheck` SpanMap 状态。

### 3.5 Watch、Project、LSP 与 API 扩展
- **`crates/tsrs_execute/src/watcher.rs`** & **`crates/tsrs_project/`**（`compilerhost.rs`、`projectcollectionbuilder.rs`）：将 `mapper_deps` 中的 `for syntax` 宏模块纳入文件监听与 Project 脏标记范围，修改宏文件自动触发引用方重检。
- **`crates/tsrs_ls/`**
  - 新增 `src/ext_ls.rs`（宏展开 Hover 提示、`"Expanded from macro ..."` 关联诊断信息、禁止重命名宏卫生生成标识符、`ts-ext/showExpansion`）；
  - `src/diagnostics.rs`、`src/hover.rs`、`src/lsconv/converters.rs` 少量转发调用。
- **`crates/tsrs_lsp/`**、**`crates/tsrs_lsproto/`**、**`crates/tsrs_cli/src/lsp.rs`**：
  - 新增 `crates/tsrs_lsp/src/ext_lsp.rs`，在 `tools/gen-lsproto/generate.mts` 注册自定义请求 `custom/tsExtShowExpansion`（若上游重新生成 `lsp_generated.rs`，运行其生成脚本即可保留）。
- **`crates/tsrs_api/`**
  - 新增 `src/ext_api.rs`（扩展协议方法），`src/session.rs` 仅 3 行分发调用。

---

## 4. 每次 Rebase 上游的标准步骤

### Step 1：拉取上游并查看变更范围
```bash
git fetch origin
# 查看上游自上次基线以来的提交，重点留意 tsrs_checker / tsrs_compiler / tsrs_contentmapper / tsrs_ls
git log --oneline <OLD_UPSTREAM_SHA>..origin/main
git diff --stat <OLD_UPSTREAM_SHA>..origin/main -- \
  crates/tsrs_checker crates/tsrs_compiler crates/tsrs_contentmapper \
  crates/tsrs_spanmap crates/tsrs_ls crates/tsrs_lsp crates/tsrs_project
```

### Step 2：创建备份分支并执行 Rebase
```bash
git checkout wip
git branch backup/wip-before-rebase-$(date +%Y%m%d)
git rebase origin/main
```

> **常见冲突消解提示：**
> 1. **`Cargo.lock`**：待代码合并完成后直接运行 `cargo check --workspace` 重新生成即可。
> 2. **`crates/tsrs_checker/src/exports.rs`**：本 Fork 新增的导出函数全部位于文件最末尾；保留上游修改并将本 Fork 的 `*_exported` 块放在文件末尾即可。
> 3. **`crates/tsrs_checker/src/checker_*.rs`**：上游偶尔会按行数重切分 `checker_01.rs`..`checker_NN.rs`。若某个 `pub(crate)` 方法被移动到相邻分片文件，用 `rg "fn get_this_argument_of_call|fn new_function_type|check_binary_like_expression"` 定位后补回该行钩子即可。
> 4. **`crates/tsrs_lsproto/src/lsp_generated.rs`**：若上游更新了 LSP 协议生成代码，保留 `tools/gen-lsproto/generate.mts` 中的扩展定义并重跑生成脚本。

### Step 3：必跑回归验证清单（全部通过后再推送）
```bash
# 1. 上游源码规范与脚本静态检查（检查 unsafe 注释、Send/Sync、测试目录规范等）
node tools/check-scripts.mjs
node tools/lint/source.mjs

# 2. 核心扩展与钩子 Clippy 检查
cargo clippy -p tsrs_checker -p ts_ext_effect -p tsrs_compiler --tests -- -D warnings

# 3. 运行全部扩展集成测试套件
cargo build -p tsrs_cli --bin tsrs
cargo test -p ts_ext_sweetener \
           -p ts_ext_typegpu \
           -p ts_ext_effect \
           -p tsrs_spanmap \
           -p tsrs_tsoptions \
           -p tsrs_contentmapper \
           -p tsrs_checker \
           -p tsrs_compiler \
           -p tsrs_ls \
           -p tsrs_api
cargo test -p ts_ext_syntax --no-default-features

# 4. 构建 Release 产物并运行 @effect-x/tsc npm 打包与端到端冒烟测试
cargo build --release -p tsrs_cli --bin tsrs --features noembed
cargo build --release -p ts_ext_syntax --bin tsc-ext --lib
mkdir -p target/fork-ext
cp target/release/tsc-ext target/fork-ext/tsc-ext
# macOS 为 .dylib，Linux 为 .so
cp target/release/libts_ext_syntax.dylib target/fork-ext/tsc-ext.node
scripts/fork/npm-pack.sh --name @effect-x/tsc --package-version 0.1.0-local \
  --ext darwin-arm64=target/fork-ext target/fork-npm target/release/tsrs
scripts/fork/npm-test.sh --name @effect-x/tsc target/fork-npm target/fork-npm-test
```

### Step 4：更新本文档并推送到 Fork 远端
1. 将本文档第 1 节表格中的 **`oxc-project/tsrs` 当前基线 Commit**（以及 `npm/tsrs/UPSTREAM.json` 若有变更）更新为新的 `origin/main` SHA 与日期。
2. 提交并推送至 `fork`（**切勿推送到 `origin`**）：
```bash
git add fork.md
git commit -m "docs(fork): record rebase onto oxc-project/tsrs <NEW_SHORT_SHA>"
git push --force-with-lease fork wip
```
