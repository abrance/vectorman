# Requirements Document

> **已归档**（2026-09-26 文档整理）：本 feature 已实现并合入 main，本文件压缩为需求索引——完整 EARS 条款见 git 历史（本文件重写前的最后一个版本），design.md 全文保留为实施细节档案。

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

- AS 运维人员, I want 给每个 App 挂上自定义 Tag, so that 能按业务或环境给入口分组。
- 验收：WHEN 用户提交新建或编辑 App 的表单，THE Console 服务 SHALL 接受该 App 的 Tag 列表并随 App 一并保存；THE 每个 App 的 Tag 数量上限 SHALL 为 10。
### Requirement 2

- AS 运维人员, I want 在桌面表单里填写 Tag, so that 不必离开新增/编辑流程。
- 验收：THE 新建 App 表单 SHALL 提供 Tag 输入；THE 编辑 App 表单 SHALL 展示该 App 已有 Tag 并允许增删。
### Requirement 3

- AS 运维人员, I want 按 Tag 筛选桌面图标, so that 只看某一类入口。
- 验收：THE 桌面 SHALL 在图标网格上方提供 Tag 筛选入口；THE Tag 筛选入口 SHALL 包含「全部」，以及当前 App 目录中出现过的全部 Tag（去重后按字典序排列）。
### Requirement 4

- AS 运维人员, I want 已有 App 目录在升级后仍能打开, so that 不必重填链接。
- 验收：WHEN App 目录中某条记录缺少 Tag 字段，THE Console 服务 SHALL 把该 App 的 Tag 列表视为空并正常列出；WHEN 新建、编辑成功，THE Console 服务 SHALL 在 HTTP 响应返回前把含 Tag 的 App 目录写入持久化存储。
