# kex-terminal

> A modern terminal multiplexer, reimagined.

## 项目概述

kex-terminal 是一个现代化的终端多路复用器（类 tmux），采用 Rust 构建。它是 kex 产品的开源 CLI 部分，配套闭源 SaaS 平台 `kex-terminal-cloud` 提供 Web 远程终端体验。

## 核心设计理念

### 资源模型（与 tmux 的关键区别）
- **终端实例与视图解耦**：终端实例是扁平的池，view 是纯视图配置
- **两层而非四层**：只有 terminal + view，没有 tmux 的 session → window → pane 层级
- **同一终端可出现在多个 view 中**
- **临时 view**：创建终端时自动分配默认视图并进入，无需手动管理

### 命令设计
- Docker 风格：`kex <资源> <动作> [参数]`
- 示例：`kex terminal create`, `kex view ls`

### 快捷键
- 轻量模式系统：只有 Normal（输入传递终端）和 Command（kex 接管）两个模式
- SSH 友好：不依赖复杂组合键，Command 模式下全部单键操作
- 语义化键位：`s` split, `k` kill, `h/j/k/l` 方向

## 技术栈

- **语言**：Rust
- **协议格式**：JSON Schema（跨项目共享，位于 `protocol/` 目录）
- **版本兼容**：消息信封 + 版本号，多版本 schema 并存

## 开发流程

### SDD + TDD
1. **Spec 先行**：先写设计文档/接口定义
2. **测试先行**：根据 spec 写测试
3. **实现跟进**：让测试通过
4. 每个模块都遵循此流程，不仅仅是协议层

### 单元测试要求
- 所有主要模块必须有单元测试
- 协议层使用共享 fixtures（`protocol/fixtures/`）确保与 kex-terminal-cloud 的一致性
- 使用 `cargo test` 运行测试

### 协议管理
- JSON Schema 定义位于 `protocol/schemas/`
- 测试 fixtures 位于 `protocol/fixtures/`
- 协议变更流程：改 schema → 更新 fixtures → 更新测试 → 更新实现

## 项目结构

```
kex-terminal/
├── src/                  ← Rust 源码
├── protocol/
│   ├── schemas/          ← JSON Schema 定义
│   ├── fixtures/         ← 跨项目测试用例
│   └── tests/            ← Schema 验证测试
└── CLAUDE.md             ← 本文件
```

## 关联项目

- **kex-terminal-cloud**（闭源）：SaaS 平台，包含 Cloudflare Workers 后端 + React Web 前端
- 两个项目通过 `protocol/` 中的 JSON Schema 协作，面向协议编程
- 设计文档和调研文档统一维护在 kex-terminal-cloud 的 `docs/` 目录中（不同步到本仓库）

## CLAUDE.md 维护规则

- 每次重大设计决策后更新本文件
- 新增模块时补充项目结构
- 协议变更时更新协议管理章节
- 保持简洁，指向详细文档而非在此重复内容
