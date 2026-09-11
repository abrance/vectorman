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
