# 需求实施计划

- [x] 1. 协议层与 Agent 分块读写
  - [x] 1.1 在 `crates/gse-proto` 增加 `FileEndpoint`、`FileReadReq/Reply`、`FileWriteReq/Reply` 及 JSON 往返测试
    - 对应设计：RPC DTO、`max_bytes`、稳定错误码
  - [x] 1.2 实现 `crates/gse-agent-core/src/file_io.rs` 并注册 `file_read` / `file_write`
    - 绝对路径校验、普通文件探测、旁路写入、`already_exists`、块校验、启动残留约定
    - 对应需求 2/6 与设计 FileIo 节
  - [x]* 1.3 FileIo 单测：相对路径拒绝、分块 eof/sha256、目录/缺失、目标已存在、父目录创建、错误块校验和

- [x] 2. Server 临时文件与台账扩展
  - [x] 2.1 实现 `job_file_store.rs`：put/get/head/delete/list_expired，校验 `file_id`
    - 对应需求 5/8 与设计 JobFileStore
  - [x] 2.2 扩展 `ServerConfig` 文件传输配置项与环境变量
    - 对应设计配置表
  - [x] 2.3 `jobs` 表增量列、`JobRecord`/`NewJob` 字段、列表过滤与 `mark_lost_by_agent` 覆盖源/目标 Agent
    - 对应需求 7 与设计 Data Models
  - [x]* 2.4 JobFileStore 单测：非法 id、隔离目录、超期列出

- [x] 3. 检查点 - 确保所有测试通过
  - 确保所有测试通过,如有疑问请询问用户

- [x] 4. 文件作业编排与 HTTP
  - [x] 4.1 实现 `file_transfer.rs`：提交校验、后台分块中转、三种路径、超时/离线/校验失败
    - 对应需求 1-6
  - [x] 4.2 扩展 `POST /jobs` 按 `kind` 分流；新增 `/api/gse/job-files` 上传/下载/列表/删除
    - 对应需求 5/8/9 与设计 HTTP 表
  - [x] 4.3 文件作业重做复制源/目标；另存为模板对文件作业返回 400
    - 对应设计重做与模板边界
  - [x]* 4.4 httptest：缺字段 400、离线 409、同源同路径 400、缺失 file_id 404、上传超限、脚本回归
  - [x]* 4.5 e2e：Agent→Agent、Agent→临时目录、上传→Agent、源缺失、超限、目标已存在

- [x] 5. 检查点 - 确保所有测试通过
  - 确保所有测试通过,如有疑问请询问用户

- [x] 6. 前端作业平台
  - [x] 6.1 扩展 `GseJobAdapter` 与 `Job` 类型：kind、source/destination、上传/列表/删除/下载 URL
    - 对应设计前端适配器
  - [x] 6.2 提交抽屉 Radio 脚本/文件传输、上传回填、列表种类列、详情文件字段与下载
    - 对应需求 9
  - [x]* 6.3 适配器与表单构造单测

- [x] 7. 检查点 - 确保所有测试通过
  - 确保所有测试通过,如有疑问请询问用户
