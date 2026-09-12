# Implementation Plan: gse-cli / vmctl

## Tasks

- [x] 1. 新增 `bins/vmctl` crate（clap 子命令、ureq HTTP、Wait 轮询、退出码）
- [x] 2. 单元测试：URL 拼接、script-file、list 过滤、submit/rerun body、Wait 终态与超时
- [x] 3. workspace / `build-package.sh` / `install.sh` / `ctl.sh` 接入 vmctl（一次性 CLI）
- [x] 4. README 与 GSE 文档补充 vmctl 用法
