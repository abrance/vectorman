# Requirements Document

## Introduction

在 Console 桌面门户的 App 目录上增加自定义 Tag。运维在新建或编辑 App 时为该 App 挂上若干 Tag；桌面按 Tag 筛选后只展示匹配的图标。本特性建立在已交付的 `console-desktop` 之上，不改变独立进程、无鉴权、新标签打开目标 URL 的既有行为。

## Glossary

- **Console 服务**: 独立进程，提供 App 目录 HTTP API 与桌面前端。
- **App**: 目录中的一条记录，含显示名称、目标 URL，以及本特性增加的 Tag 列表。
- **Tag**: 运维为某个 App 填写的短文本标签；同一 App 可挂多个 Tag。
- **桌面**: Console 前端首页，以图标网格展示 App。
- **Tag 筛选**: 桌面上的筛选入口；可同时选中多个 Tag，网格只展示 Tag 列表包含全部选中 Tag 的 App。
- **App 目录**: Console 服务持久化保存的 App 列表。

## Requirements

### Requirement 1

**User Story:** AS 运维人员, I want 给每个 App 挂上自定义 Tag, so that 能按业务或环境给入口分组。

#### Acceptance Criteria

1. WHEN 用户提交新建或编辑 App 的表单，THE Console 服务 SHALL 接受该 App 的 Tag 列表并随 App 一并保存。
2. THE 每个 App 的 Tag 数量上限 SHALL 为 10。
3. WHEN 某个 Tag 去掉首尾空白后长度为 1 至 32 个字符，THE Console 服务 SHALL 接受该 Tag。
4. IF 某个 Tag 去掉首尾空白后长度为 0 或超过 32 个字符，THE Console 服务 SHALL 返回 HTTP 400 并说明 Tag 校验失败原因。
5. IF 提交的 Tag 数量超过 10，THE Console 服务 SHALL 返回 HTTP 400 并说明已达单个 App 的 Tag 上限。
6. WHEN 同一 App 的 Tag 列表中出现去掉首尾空白后文本全等的重复项，THE Console 服务 SHALL 只保留一份。
7. WHEN 请求省略 Tag 列表，THE Console 服务 SHALL 将该 App 的 Tag 列表视为空。

### Requirement 2

**User Story:** AS 运维人员, I want 在桌面表单里填写 Tag, so that 不必离开新增/编辑流程。

#### Acceptance Criteria

1. THE 新建 App 表单 SHALL 提供 Tag 输入。
2. THE 编辑 App 表单 SHALL 展示该 App 已有 Tag 并允许增删。
3. WHEN 用户保存成功，THE 桌面 SHALL 按保存后的 Tag 列表刷新该 App。
4. Tag 由运维在 App 表单中自由输入；THE Console 服务 v1 SHALL 把 Tag 作为 App 字段存储，不提供独立的 Tag 字典管理接口。

### Requirement 3

**User Story:** AS 运维人员, I want 按 Tag 筛选桌面图标, so that 只看某一类入口。

#### Acceptance Criteria

1. THE 桌面 SHALL 在图标网格上方提供 Tag 筛选入口。
2. THE Tag 筛选入口 SHALL 包含「全部」，以及当前 App 目录中出现过的全部 Tag（去重后按字典序排列）。
3. WHEN 用户选择「全部」，或当前选中的 Tag 集合为空，THE 桌面 SHALL 展示 App 目录中的全部 App。
4. WHEN 用户选中一个或多个 Tag，THE 桌面 SHALL 仅展示 Tag 列表同时包含全部选中 Tag 的 App。
5. WHEN 用户再次点击已选中的 Tag，THE 桌面 SHALL 从选中集合中去掉该 Tag 并按剩余选中集合刷新网格。
6. WHEN 当前 App 目录中没有任何 Tag，THE 桌面 SHALL 仍展示「全部」并展示全部 App。
7. THE 每个 App 图标区域 SHALL 展示该 App 的 Tag 列表；Tag 列表为空时图标区域只展示名称。

### Requirement 4

**User Story:** AS 运维人员, I want 已有 App 目录在升级后仍能打开, so that 不必重填链接。

#### Acceptance Criteria

1. WHEN App 目录中某条记录缺少 Tag 字段，THE Console 服务 SHALL 把该 App 的 Tag 列表视为空并正常列出。
2. WHEN 新建、编辑成功，THE Console 服务 SHALL 在 HTTP 响应返回前把含 Tag 的 App 目录写入持久化存储。
3. WHEN Console 服务进程退出后再次启动，THE Console 服务 SHALL 加载上次保存的 Tag 列表。
4. GET App 列表与 POST/PUT 成功响应中的 App 对象 SHALL 包含 Tag 列表字段。

## Out of Scope (v1)

- 全局 Tag 字典的独立增删改接口。
- 按 Tag 颜色、图标或排序权重。
- 鉴权与多租户隔离。
