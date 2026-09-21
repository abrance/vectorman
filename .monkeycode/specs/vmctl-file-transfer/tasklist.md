# 需求实施计划

- [x] 1. 扩展 vmctl HTTP 传输
  - [x] 1.1 增加 `RequestBody` / `HttpResponse` / `Transport::exchange`，`send` 走 `exchange`
  - [x] 1.2 `UreqTransport` 支持 GET/POST/DELETE、JSON、multipart、二进制响应；读超时 300 秒
  - [x]* 1.3 FakeTransport 记录 method/url/body 变体

- [x] 2. jobs submit 按 kind 分流
  - [x] 2.1 clap：`--agent-id`/`--script-file` 可选；增加 `--kind` 与文件传输参数
  - [x] 2.2 脚本作业保持无 `kind` 字段的 JSON；文件标志冲突退出 1
  - [x] 2.3 Agent 互传提交 `source`/`destination` 为 agent
  - [x] 2.4 `--upload` 先 POST `/api/gse/job-files` 再提交 `server_temp` → agent
  - [x]* 2.5 单测：脚本回归、互传 body、上传两步、kind/标志冲突

- [x] 3. jobs files 与 rerun
  - [x] 3.1 `jobs files list|upload|download|delete`
  - [x] 3.2 `jobs rerun --dest-path`
  - [x]* 3.3 单测：list/upload/download 字节一致/delete 204、rerun dest_path、文件作业 Wait 退出码

- [x] 4. 检查点 - 确保所有测试通过
  - `cargo fmt`、`clippy -D warnings`、`cargo test -p vmctl`
