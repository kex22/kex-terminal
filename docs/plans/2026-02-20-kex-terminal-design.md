# kex-terminal 整体设计方案

> 日期：2026-02-20
> 状态：草案

## 1. 项目定位

kex-terminal 是一个现代化终端多路复用器，目标是替代 tmux，提供更直观的命令设计、更简洁的概念模型和 Web 远程操作能力。

**产品组成：**
- `kex-terminal`（开源）：Rust CLI 终端多路复用器
- `kex-terminal-cloud`（闭源）：SaaS 平台 + Web 前端

## 2. 资源模型

### 2.1 与 tmux 的核心区别

tmux 将终端进程和视觉位置绑定在一起（pane = 终端 + 位置），kex 将两者解耦：

- **Terminal**：独立的终端实例，扁平池结构，不属于任何层级
- **Workspace**：纯视图配置，定义哪些 terminal 展示、如何布局

### 2.2 资源层级

```
Server（守护进程）
├── Terminal 1 (vim)
├── Terminal 2 (cargo watch)
├── Terminal 3 (htop)
│
├── Workspace "coding"       → 展示 T1 + T2，左右分割
└── Workspace "monitoring"   → 展示 T3，全屏
```

关键特性：
- 同一 terminal 可出现在多个 workspace
- 创建 terminal 时自动分配临时 workspace 并进入
- Workspace 可后续升级为命名 workspace

## 3. 命令设计

### 3.1 风格

Docker 风格：`kex <资源> <动作> [参数]`

### 3.2 Terminal 命令

```bash
kex terminal create              # 创建终端 → 自动临时 workspace → 进入
kex terminal ls                  # 列出所有终端实例
kex terminal kill <id>           # 销毁终端
kex terminal attach <id>         # 连接到已有终端
```

### 3.3 Workspace 命令

```bash
kex workspace create <name>      # 创建命名工作区
kex workspace add <name> <t-id>  # 将终端加入工作区
kex workspace show <name>        # 切换到工作区视图
kex workspace ls                 # 列出所有工作区
kex workspace rm <name>          # 删除工作区（不影响终端实例）
```

### 3.4 认证命令

```bash
kex login                        # 通过浏览器 OAuth 登录 SaaS 平台
kex logout                       # 登出
kex sync enable                  # 开启同步到平台
kex sync disable                 # 关闭同步
```

## 4. 快捷键与交互

> 详细调研见 [keybinding-research.md](../research/keybinding-research.md)

### 4.1 轻量模式系统

只有两个模式：

| 模式 | 行为 | 进入方式 |
|------|------|----------|
| Normal | 所有输入传递给终端 | 默认模式 / 在 Command 模式按 `Esc` |
| Command | kex 接管键盘，单键操作 | 按 `Ctrl-a` |

### 4.2 Command 模式键位

```
导航：h/j/k/l          ← 切换 pane 焦点
分割：s (水平) / v (垂直)
终端：n (新建) / k (关闭当前)
Workspace：w (列表) / 1-9 (快速切换)
调整：H/J/K/L           ← 调整 pane 大小
退出：Esc               ← 回到 Normal 模式
```

### 4.3 SSH 兼容性

- 不依赖 `Ctrl-Shift-*` 或 `Alt-*` 组合键
- Command 模式下全部单键操作，任何 SSH 环境都能工作
- 切换键 `Ctrl-a` 在所有终端和 SSH 客户端中可靠传递

### 4.4 提示栏

- 底部可选提示栏，显示当前模式和可用键位
- 默认开启，可通过配置关闭

## 5. 系统架构

### 5.1 整体架构

```
┌─────────────────────────────────────────────────┐
│              kex-terminal-cloud (闭源)            │
│                                                   │
│  ┌──────────┐  ┌──────────────┐  ┌────────────┐ │
│  │ Web 前端  │  │ Workers API  │  │ Durable    │ │
│  │ React+Vite│←→│ 认证/路由    │←→│ Objects    │ │
│  │ xterm.js  │  │              │  │ 会话管理    │ │
│  └──────────┘  └──────┬───────┘  └────────────┘ │
│         Cloudflare Pages │ Workers    D.O.        │
└─────────────────────────┼────────────────────────┘
                          │ WebSocket
              ┌───────────┼───────────┐
              │           │           │
         ┌────┴───┐ ┌────┴───┐ ┌────┴───┐
         │ 设备 1  │ │ 设备 2  │ │云终端   │
         │kex CLI │ │kex CLI │ │Container│
         └────────┘ └────────┘ └────────┘
              kex-terminal (开源)
```

### 5.2 技术栈

| 组件 | 技术 | 说明 |
|------|------|------|
| CLI | Rust | 高性能、内存安全 |
| SaaS 后端 | Cloudflare Workers | 边缘计算，全球低延迟 |
| 会话管理 | Durable Objects | 有状态 WebSocket 管理 |
| 存储 | D1 / KV | 用户数据、设备注册、配置 |
| Web 前端 | React 19 + Vite | SPA，部署到 Cloudflare Pages |
| 终端渲染 | xterm.js v6 | WebGL 渲染，插件丰富 |
| 实时通信 | WebSocket | CLI ↔ 平台 ↔ Web |
| 云终端 | Cloudflare Containers | 平台托管终端实例 |
| 认证 | OAuth + 邮箱密码 | CLI 端通过 `kex login` 浏览器授权 |

> 前端技术栈详细调研见 [web-frontend-stack.md](../research/web-frontend-stack.md)

## 6. 协议设计

### 6.1 格式

JSON Schema 定义，跨语言共享。

### 6.2 消息信封

```json
{
  "v": 1,
  "type": "terminal.output",
  "payload": { ... }
}
```

- `v`：协议版本号（整数递增）
- `type`：消息类型（`资源.动作` 格式）
- `payload`：具体数据，由对应版本的 schema 约束

### 6.3 版本兼容

- 新增字段：不破坏兼容，`v` 不变
- 删除/修改字段：破坏性变更，`v` 递增
- 平台同时支持多个 `v` 版本
- CLI 连接时握手上报支持的最高版本

## 7. 安全模型

- **默认离线**：CLI 默认不连接 SaaS 平台，纯本地使用
- **Opt-in 同步**：用户主动执行 `kex sync enable` 才开启
- **认证流程**：`kex login` → 浏览器 OAuth → token 存储本地
- **WebSocket 鉴权**：连接时携带 token，平台验证后建立会话
- **多租户隔离**：用户间数据和终端完全隔离

## 8. 项目结构

### 8.1 kex-terminal（开源，Rust）

```
kex-terminal/
├── src/
├── protocol/
│   ├── schemas/v1/       ← JSON Schema 定义
│   ├── fixtures/         ← 跨项目共享测试用例
│   └── tests/
├── docs/
│   ├── research/
│   └── plans/
├── CLAUDE.md
└── Cargo.toml
```

### 8.2 kex-terminal-cloud（闭源，TS monorepo）

```
kex-terminal-cloud/
├── apps/
│   ├── platform/         ← Cloudflare Workers 后端
│   └── web/              ← React 19 + Vite 前端
├── packages/
│   └── protocol/         ← 从 JSON Schema 生成的 TS 类型
├── pnpm-workspace.yaml
├── CLAUDE.md
└── turbo.json
```

## 9. 开发流程

### 9.1 SDD + TDD

```
Spec（设计文档/接口定义）
  → Test（根据 spec 写测试）
    → Implementation（让测试通过）
```

每个模块都遵循此流程。

### 9.2 跨项目协议测试

- kex-terminal 的 `protocol/fixtures/` 存放标准测试用例
- CLI（Rust）和 SaaS（TS）各自用 fixtures 验证序列化/反序列化一致性
- 协议变更流程：改 schema → 更新 fixtures → 两边跑测试 → 更新实现

## 10. Roadmap

### Phase 1 — CLI 基础

**目标**：可用的本地终端多路复用器

- kex server 守护进程
- 终端实例创建/销毁/列表
- 临时 workspace，创建即进入
- Docker 风格命令体系

> 设计参考：本文档第 2、3 节

### Phase 2 — CLI 完善

**目标**：功能完整的终端多路复用器

- Pane 分割（水平/垂直）+ h/j/k/l 导航
- Normal / Command 模式切换 + 提示栏
- 命名 workspace + 多终端布局
- 终端实例跨 workspace 共享
- 会话持久化（断开重连）
- 配置文件 + Pane 大小调整

> 设计参考：本文档第 4 节，[keybinding-research.md](../research/keybinding-research.md)

### Phase 3 — SaaS 平台基础

**目标**：CLI 可连接到云端

- Cloudflare Workers API + 用户认证（OAuth + 邮箱密码）
- CLI 端 `kex login` + 设备注册
- WebSocket 通道：CLI ↔ 平台 ↔ Web
- Durable Objects 会话管理
- 协议 v1 JSON Schema 定义 + fixtures

> 设计参考：本文档第 5、6、7 节

### Phase 4 — Web 终端体验

**目标**：浏览器中完整操作远程终端

- React 19 + Vite + xterm.js v6 前端
- 完整远程终端操作（输入/输出/resize）
- 多设备管理面板
- Workspace 视图在 Web 端的还原

> 设计参考：[web-frontend-stack.md](../research/web-frontend-stack.md)

### Phase 5 — 云终端

**目标**：平台提供托管终端实例

- Cloudflare Containers 托管终端
- 云终端的创建/销毁/管理
- 与自有设备终端统一的操作体验
- 计费与资源限制

## 11. GitHub 管理

### 11.1 组织与仓库

- **组织**：`kex22`
- **kex-terminal-cloud**（Private）：闭源 monorepo，主开发仓库
- **kex-terminal**（Public）：开源 CLI，由 CI 从 monorepo `cli/` 自动同步
- 初期公开仓库只读，不接受外部 PR

### 11.2 自动同步机制

- 方式：`git subtree split`（保留 commit 历史，未来可迁移到文件复制方案）
- 触发：`main` 分支 `cli/` 目录变更时自动执行
- 认证：Fine-grained PAT（仅限 kex-terminal 仓库 contents:write 权限），存为 `SYNC_PAT` secret
- 安全：`cli/` 中禁止放任何闭源内容

### 11.3 CI/CD 流水线

| 流水线 | 仓库 | 触发条件 | 职责 |
|--------|------|----------|------|
| CLI Tests | cloud (private) | `cli/` 变更 | Rust fmt + clippy + test |
| SaaS Tests | cloud (private) | `apps/` 或 `packages/` 变更 | lint + typecheck + test |
| Sync CLI | cloud (private) | `main` 分支 `cli/` 变更 | subtree push 到公开仓库 |
| Release | terminal (public) | tag `v*` 推送 | 多平台构建 + GitHub Release |

### 11.4 公开仓库保护

- `main` 分支 branch protection：只允许 CI bot 推送
- 禁用直接 push 和 PR 合并（初期）
