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
- Date: 2026-09-06
- Context: Discovered by Agent while running `cargo test` for the gse feature in this workspace
- Category: Environment Configuration
- Instructions:
  - The sandbox background terminal shell does not have cargo/rustc on PATH (`cargo: not found`). Compile/build/test commands that must run in a managed background terminal shall use the absolute path `/root/.cargo/bin/cargo` explicitly.

[User Instruction Summary]
- Date: 2026-09-07
- Context: User corrected the node app preview flow after a Vite-only `npm run dev` deploy
- Instructions:
  - Deploy the frontend by building first (`npm run build -w @vectorman/node`), then start/mount the backend (gse-server), then serve the built assets with reverse proxy to GSE HTTP. Do not treat `vite --host` dev server as the deploy path.