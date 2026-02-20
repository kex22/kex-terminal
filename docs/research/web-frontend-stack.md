# Web 前端技术栈调研

> 数据来源：Reddit (r/reactjs, r/tmux, r/webdev, r/electronjs)、GitHub

## 1. 终端渲染库：xterm.js

### 现状
- **最新版本**：v6.0.0（2025-12-22 发布）
- **GitHub Stars**：~20,000
- **活跃维护**：最近更新 2026-02-19
- **v6.0 重要特性**：
  - ESM 支持（via esbuild）
  - Shadow DOM 支持（WebGL 渲染器）
  - 连字（ligature）支持
  - 从 VS Code 移植了滚动条组件
  - 同步输出支持（DEC mode 2026）

### 竞品：Ghostty Web（新兴）
- **来源**：Coder 团队开发，基于 Ghostty 终端的 WASM 编译版本
- **核心优势**：使用 Ghostty 原生 VT100 解析器（WASM 编译），解析精度高于 xterm.js 的纯 JS 实现
- **API 兼容**：刻意兼容 xterm.js API，迁移成本低
- **体积**：WASM bundle ~400KB，零运行时依赖
- **现状**：较新项目，生态和插件远不如 xterm.js 成熟

### 对比

| 维度 | xterm.js v6 | Ghostty Web |
|------|-------------|-------------|
| 成熟度 | 极高（VS Code、Replit 等在用） | 早期 |
| 渲染 | WebGL2 + Canvas 双模式 | WASM |
| 插件生态 | 丰富（Search、Fit、WebLinks 等） | 几乎没有 |
| VT100 兼容性 | 好 | 更好（原生解析器） |
| 社区 | ~20k stars，活跃 | 新项目 |

### 结论
**推荐 xterm.js v6**。Ghostty Web 值得关注但目前太早期，缺乏插件生态。可以在架构上预留抽象层，未来有需要时切换。

## 2. 前端框架：React SPA vs SSR

### Reddit 社区观点（来源：r/reactjs，多个高赞帖）

**帖子：「Thinking of abandoning SSR/Next.js for "Pure" React + TanStack Router」**（216pts, 242 comments）
- 最高赞评论（453pts）："SSR is the most over hyped and over recommended thing in FE this decade. If you're building an app rather than a website it's straight up a worse option. More complexity, more risk, more cost, zero advantage."
- 多位开发者反馈：Next.js 的 Vercel 锁定、Edge Request 计费、自托管复杂度是主要痛点
- 共识：**对于应用型产品（非内容网站），SPA 是更好的选择**

**帖子：「React recently feels biased against Vite and SPA」**（126pts, 63 comments）
- React 官方团队被批评偏向 SSR/Next.js，忽视 SPA 场景
- 社区强烈支持 Vite 作为 SPA 构建工具
- 多人指出 Vercel 的商业利益影响了 React 的技术方向

### 对 kex-terminal 的分析
kex-terminal Web 端是一个**重交互的应用**（实时终端操作），不是内容网站，因此：
- 不需要 SEO（终端界面无需被搜索引擎索引）
- 不需要 SSR（首屏渲染速度不是核心指标，WebSocket 连接建立才是）
- 需要部署到 Cloudflare Pages（纯静态 SPA 天然适配）

### 结论
**React 19 + Vite + TanStack Router** 是最佳选择。不用 Next.js。

## 3. 最终推荐技术栈

| 层 | 选型 | 理由 |
|----|------|------|
| 终端渲染 | xterm.js v6 | 事实标准，WebGL 渲染，插件丰富 |
| 前端框架 | React 19 | 生态成熟，社区大 |
| 构建工具 | Vite | 快速，SPA 友好，Cloudflare Pages 原生支持 |
| 路由 | TanStack Router | 类型安全，SPA 专用，社区推荐 |
| 部署 | Cloudflare Pages | 静态 SPA 托管，全球 CDN |
