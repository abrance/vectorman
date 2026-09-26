# User Instruction Memory

This file records user instructions, preferences, and teachings for reference in future interactions.

## Format

### User Instruction Entry
User instruction entries should follow this format:

[User Instruction Summary]
- Date: [YYYY-MM-DD]
- Context: [Mentioned scenario or time]
- Instructions:
  - [Content of user teaching or instruction, described line by line]

### Project Knowledge Entry
Entries discovered by the Agent during task execution should follow this format:

[Project Knowledge Summary]
- Date: [YYYY-MM-DD]
- Context: Discovered by Agent while performing [specific task description]
- Category: [Operations & Deployment|Build Methods|Testing Methods|Troubleshooting & Debugging|Workflow & Collaboration|Environment Configuration]
- Instructions:
  - [Specific knowledge points, described line by line]

## Deduplication Strategy
- Before adding a new entry, check for similar or identical instructions.
- If a duplicate is found, skip the new entry or merge it with the existing one.
- When merging, update the context or date information.
- This helps avoid redundant entries and keeps the memory file tidy.

## Entries

[Project Knowledge Summary]
- Date: 2026-09-11
- Context: Discovered by Agent while diagnosing musl vmctl SIGSEGV during packaging
- Category: Build Methods
- Instructions:
  - musl 静态链接时只设置 `CC_x86_64_unknown_linux_musl=musl-gcc`。不要设置 `CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc`：ring/ureq（`vmctl`、`dpc`）启动会 SIGSEGV（`--help` 即崩，exit 139）。rustc musl target 默认 rust-lld 链接的二进制 `ldd` 仍为 statically linked。

[Project Knowledge Summary]
- Date: 2026-09-06
- Context: Discovered by Agent while running `cargo test` for the gse feature in this workspace
- Category: Environment Configuration
- Instructions:
  - The sandbox background terminal shell does not have cargo/rustc on PATH (`cargo: not found`). Compile/build/test commands that must run in a managed background terminal shall use the absolute path `/root/.cargo/bin/cargo` explicitly.

[User Instruction Summary]
- Date: 2026-09-07 (updated 2026-09-10)
- Context: User corrected the frontend preview flow; later unified the frontend into a single console artifact
- Instructions:
  - Deploy the frontend by building first (`cd frontend && npm run build:console`), then start/mount the backend (gse-server), then serve `frontend/apps/console/dist` via gse-server `http_web_dir`. Do not treat `vite --host` dev server as the deploy path.
  - Frontend architecture (2026-09-10): backend stays a single gse-server; frontend ships one artifact `@vectorman/console`. `@vectorman/node` and `@vectorman/job` are UI packages (barrel + `exports`) composed by console; do not build or deploy them independently. Packaging entry is `packaging/build-package.sh` -> `npm run build:console`.
  - Dataplane frontend (2026-09-15): the ingest dataplane UI is a second artifact `@vectorman/dataplane` (`cd frontend && npm run build:dataplane`), shipped as `dataserver/web/` and hosted by dataserver via its own `http_web_dir = "web"`. `packaging/build-package.sh` assembles it and `packaging/deploy/install.sh` copies `web/` for both `gse-server` and `dataserver`.

[Project Knowledge Summary]
- Date: 2026-09-15
- Context: Discovered by Agent while committing formatting fixes on branch 260913-feat-gse-dataplane-ingest
- Category: Workflow & Collaboration
- Instructions:
  - This repo ships a local `.git/hooks/prepare-commit-msg` that auto-appends `Co-authored-by: monkeycode-ai <monkeycode-ai@chaitin.com>` from git config `coauthor.*`. Do not add the co-author trailer manually; doing so produces duplicate trailers.
  - Rust CI (`abrance/yoc/.github/workflows/rust-ci.yml@v1.0.0`) runs, in order: `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-features`, `cargo build --all-features`. Verify these four locally before pushing; the fmt step is a common failure.

[User Instruction Summary]
- Date: 2026-09-26
- Context: 文档规整专项（spec 压缩 + 索引 + 死链检查）
- Instructions:
  - `.monkeycode/specs/` 的 `design.md` 是长效档案，随代码演进维护；已实现 feature 的 `requirements.md` 压缩为需求索引（编号 + User Story + 验收摘要），完整 EARS 条款查 git 历史。源码注释按「requirements.md Requirement N」引用，Requirement 编号与文件名不可改。
  - 功能现状以 README「能力清单」与代码为准；`.monkeycode/specs/README.md` 的索引表记状态与截至日期。
  - 文档死链检查：`scripts/check-docs.sh`（已挂 rust-ci.yml 的 `doc-links` job），改 md 或挪文件后本地先跑。
  - 踩坑记录分流：能进代码注释的进代码注释（随代码走）；流程性/跨会话的进 `.monkeycode/MEMORY.md`；README 只留使用者会踩的。
