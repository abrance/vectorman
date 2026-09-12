# 需求实施计划

- [ ] 1. 搭建 console 后端骨架
- [x] 1. 搭建 console 后端骨架
  - [ ] 1.1 新增 workspace member `bins/console`（Cargo.toml、main.rs、lib.rs 模块入口）
  - [x] 1.1 新增 workspace member `bins/console`（Cargo.toml、main.rs、lib.rs 模块入口）
    - 对齐 Requirement 1.1、design `bins/console`
  - [ ] 1.2 实现配置加载 `console.toml` + `CONSOLE_*` 环境变量，默认 `listen=0.0.0.0:7200`
    - 对齐 Requirement 6.4、design 配置表
  - [ ]* 1.3 配置非法与缺文件的单元测试

- [ ] 2. 实现 App 目录与校验落盘
- [x] 2. 实现 App 目录与校验落盘
  - [ ] 2.1 实现 App 模型、名称/URL 校验、100 条上限、名称冲突
  - [x] 2.1 实现 App 模型、名称/URL 校验、100 条上限、名称冲突
    - 对齐 Requirement 4、5，design 校验规则
  - [ ] 2.2 实现 `apps.json` 加载/原子写入，写失败回滚内存
    - 对齐 Requirement 6.1-6.2、design 落盘与 Correctness
  - [ ] 2.3 为校验、冲突、上限、重启加载、损坏文件编写单元测试
    - 对齐 design Test Strategy 后端项

- [ ] 3. 实现 Console HTTP API 与静态托管
- [x] 3. 实现 Console HTTP API 与静态托管
  - [ ] 3.1 实现 `/health` 与 `/api/console/apps` CRUD，无鉴权，错误 JSON `error`+`message`
  - [x] 3.1 实现 `/health` 与 `/api/console/apps` CRUD，无鉴权，错误 JSON `error`+`message`
    - 对齐 Requirement 4-6、design HTTP API
  - [ ] 3.2 同端口托管 `web_dir`，缺 `index.html` 时仅 API
    - 对齐 Requirement 2.1、design Error Handling
  - [ ] 3.3 HTTP 状态码与持久化往返的单元测试
    - 对齐 design Test Strategy

- [ ] 4. 检查点 - 确保后端测试通过
  - 运行 `cargo test -p console`

- [ ] 5. 实现 @vectorman/desktop 桌面
- [x] 5. 实现 @vectorman/desktop 桌面
  - [ ] 5.1 新增 Vite 应用：壁纸、图标网格、空目录添加入口、加载失败重试
  - [x] 5.1 新增 Vite 应用：壁纸、图标网格、空目录添加入口、加载失败重试
    - 对齐 Requirement 2
  - [ ] 5.2 点击图标 `window.open` 新标签 noopener；添加/编辑/删除对接 `/api/console`
    - 对齐 Requirement 3-5、7.2
  - [ ] 5.3 vite 反代 `/api` -> `127.0.0.1:7200`，`allowedHosts` 含 `.monkeycode-ai.online`
    - 对齐 Requirement 7.1
  - [ ] 5.4 空目录、打开新标签、删除确认的前端测试
    - 对齐 design Test Strategy 前端项

- [ ] 6. 接入打包与安装
- [x] 6. 接入打包与安装
  - [ ] 6.1 `build-package.sh` 增加 console 组件与 `build:desktop` -> `console/web/`
  - [x] 6.1 `build-package.sh` 增加 console 组件与 `build:desktop` -> `console/web/`
    - 对齐 Requirement 1.2、1.5
  - [ ] 6.2 `install.sh`/`ctl.sh`/systemd unit 按常驻进程管理 console
    - 对齐 Requirement 1.2、1.3

- [ ] 7. 检查点 - 确保前后端测试通过
  - 运行 `cargo test -p console` 与 `npm run test -w @vectorman/desktop`
  - [x] 1.2 实现配置加载 `console.toml` + `CONSOLE_*` 环境变量，默认 `listen=0.0.0.0:7200`
  - [x] 2.2 实现 `apps.json` 加载/原子写入，写失败回滚内存
  - [x] 2.3 为校验、冲突、上限、重启加载、损坏文件编写单元测试
  - [x] 3.2 同端口托管 `web_dir`，缺 `index.html` 时仅 API
  - [x] 3.3 HTTP 状态码与持久化往返的单元测试
- [x] 4. 检查点 - 确保后端测试通过
  - [x] 5.2 点击图标 `window.open` 新标签 noopener；添加/编辑/删除对接 `/api/console`
  - [x] 5.3 vite 反代 `/api` -> `127.0.0.1:7200`，`allowedHosts` 含 `.monkeycode-ai.online`
  - [x] 5.4 空目录、打开新标签、删除确认的前端测试
  - [x] 6.2 `install.sh`/`ctl.sh`/systemd unit 按常驻进程管理 console
- [x] 7. 检查点 - 确保前后端测试通过
