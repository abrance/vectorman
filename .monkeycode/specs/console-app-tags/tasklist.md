# 需求实施计划

- [x] 1. 后端 App.tags、normalize 与 HTTP
  - [x] 1.1 `App` 增加 `tags`，缺字段视为 `[]`；`create`/`update` 写入前 `normalize_tags`
    - 对齐 Requirement 1、4，design `catalog.rs`
  - [x] 1.2 HTTP `AppInput.tags` 可省略；400 `invalid_tag` / `tag_limit_exceeded`
    - 对齐 Requirement 1.4-1.7、4.4，design `http.rs`
  - [x] 1.3 省略 tags、去重、超长、超个数、旧文件缺字段的单元测试
    - 对齐 design Test Strategy 后端项

- [x] 2. 桌面表单 Tag 输入
  - [x] 2.1 `DesktopApp.tags`；create/update 请求体带 `tags`
    - 对齐 Requirement 2.4、design frontend api
  - [x] 2.2 新建/编辑表单 Chip + 回车添加，保存后刷新
    - 对齐 Requirement 2.1-2.3

- [x] 3. 桌面筛选与图标 Tag
  - [x] 3.1 筛选条「全部」+ 去重字典序；多选 AND；再点取消
    - 对齐 Requirement 3.1-3.6
  - [x] 3.2 图标下方展示 Tag；表单提交、AND 筛选、点「全部」的前端测试
    - 对齐 Requirement 3.7、design Test Strategy 前端项

- [x] 4. 检查点
  - 运行 `cargo test -p console` 与 `npm run test -w @vectorman/desktop`
