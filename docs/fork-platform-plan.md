# 分身生成器技术方案

> 状态：M1 已实现；M2/M3 待实施（保留原始方案供后续参考）
> 日期：2026-09-16
> 基线：`kaiwu-clone@07708c5`；架构师分身资产见 Artpal 仓库 `产品文档及计划/artpal-architect-knowledge/`

## 1. 背景与目标

立项时，`kaiwu-clone` 是身份凭据生成器（Tauri 2 + React 19）：本机生成并管理 Buzz/Nostr 身份，支持 NIP-19 与 Hex 展示、重命名和删除。

目标是把客户端升级为"分身工坊"：从**身份凭证生成 → 分身生成 → 分身运行 → 分身事实更新**的完整体验，让同事无需接触 Docker Compose、快照发布器和 Hermes 配置，几步操作即可拥有一个接入 Buzz、可被同事 @ 的专属分身。

已确认的范围决策：

- **运行位置**：先做本机版（同事电脑上的 Docker Desktop），预留远端服务模式。
- **事实来源**：绑定 Git 仓库（按 commit 快照）与本地文档目录（目录打包快照）两种。
- **一期**：先完成本方案设计与评审，再实施。

## 2. 可复用资产盘点

Artpal 仓库已有一套经过生产验证的架构师分身实现（`产品文档及计划/artpal-architect-knowledge/`），本方案最大化复用：

| 资产 | 位置 | 复用方式 |
|---|---|---|
| Hermes 容器方案 | `hermes/Dockerfile`、`compose.yaml`、`runtime-entrypoint.sh` | 通用化为标准镜像与运行参数 |
| 只读知识插件 | `hermes/plugins/artpal_readonly_knowledge/` | 改名通用化；索引格式不变 |
| 快照构建器 | `tools/build_snapshot.py` | **Rust 重写**（消除 Python 依赖），保留哈希清单与 fail-closed 语义 |
| 快照与索引格式 | `snapshots/`、`snapshot-receipts/`（`MANIFEST.json`、`public-index.json`） | 格式冻结，客户端与插件共用 |
| 人格与路由规则 | `hermes/SOUL.md`、`SKILL.md` | 改造为通用模板（去 Artpal 业务化），可自定义 |
| Buzz 接入经验 | NIP-OA owner 标签、Agent Directory（kind 10100）、`BUZZ_ALLOW_ALL_USERS`、Relay 成员授权（`403 relay_membership_required`） | 固化为客户端内置流程与提示 |
| 部署陷阱 | `BUZZ_AUTH_TAG` JSON 注入、只读根文件系统、能力裁剪 | 由客户端生成 compose 参数，用户不再手写 |

## 3. 产品形态

保留现有身份管理，新增两个一级导航：

```text
┌─ 身份        ── 现有功能（生成/查看/重命名/删除）
├─ 分身        ── 创建向导、配置、启动/停止、状态与日志、事实更新
└─ 设置        ── Docker 检测、模型 Provider 凭据、Buzz Relay 配置
```

### 分身状态机

```text
草稿 ──构建快照──▶ 就绪 ──启动──▶ 运行中 ──事实更新──▶ 更新中 ──▶ 运行中
                              └──停止──▶ 已停止 ──启动──▶ 运行中
```

## 4. 架构设计

### 4.1 数据模型（本机）

```jsonc
// app_data_dir/forks/<fork-id>/fork.json
{
  "id": "…", "name": "宋十木的分身",
  "identityId": "…",                    // 关联 kaiwu-clone 身份
  "persona": { "soul": "…", "skill": "…" },   // 内联文本，来自模板或自定义
  "knowledgeSources": [
    { "type": "git", "repoPath": "/path/to/repo", "branch": "main",
      "include": ["README.md", "src/**/*.py"], "excludePatterns": [] },
    { "type": "folder", "path": "/path/to/docs", "include": ["**/*.md"] }
  ],
  "model": { "provider": "deepseek", "apiKeyRef": "keyring://fork/<id>" },
  "buzz": { "relayUrl": "https://buzz.artpalstudio.com", "homeChannelId": "…",
            "allowAllUsers": true, "requireMention": true },
  "runtime": { "containerName": "buzz-fork-<id>", "image": "buzz-fork-hermes:<ver>",
               "snapshotId": "…", "state": "running" }
}
```

凭据（身份私钥、模型 API Key）一律存 OS keyring，不落 JSON。

### 4.2 知识快照构建（Rust 实现）

替代 `build_snapshot.py` 的核心语义，输出格式与现有插件完全兼容：

- **Git 源**：以指定 commit tree 为唯一来源（`git2` crate 读取 tree，工作树改动不进入快照）；执行 `include`/`exclude` 匹配；拒绝符号链接与非普通文件。
- **目录源**：递归扫描目录并按模式过滤；同样拒绝符号链接。
- **输出**：
  - `snapshots/<snapshot-id>/`：公开文件，只读，ID 全局唯一（时间戳 + 内容哈希前缀）。
  - `snapshot-receipts/<snapshot-id>/`：`MANIFEST.json`（文件哈希清单）、`public-index.json`（受保护索引，供容器内插件校验）、`sources.json`（来源与 commit/目录记录）。
- **敏感扫描**：私钥格式、常见 Token 前缀、带凭据 URL——命中即拒绝构建。
- **不可变**：快照 ID 已存在时构建失败，不原地覆盖。

插件侧无需改动逻辑：仍按 `public-index.json` 的精确路径、大小和 SHA-256 校验，未登记文件、路径穿越和符号链接一律拒绝。

### 4.3 容器与运行（本机 Docker）

- **Docker 访问**：客户端调用 `docker` CLI，先做 `docker info` 探测并在设置页给出安装/启动引导。不直接操作 Docker socket（避免 macOS/Windows 权限差异）。
- **通用镜像** `buzz-fork-hermes:<version>`：由本仓库 `runtime/`（新增）维护 Dockerfile，CI 构建推送 ACR；与架构师分身镜像同构（Hermes 基础镜像 + Buzz CLI + 通用知识插件 + 控制目录）。
- **每个分身的运行参数**由客户端生成，等价于架构师分身的 compose 安全基线：
  - 只读根文件系统、`cap_drop: ALL`、`no-new-privileges`、`pids_limit`
  - 仅挂载：分身状态卷（`/opt/data`）、快照（`/knowledge:ro`）、受保护索引（只读 secret）
  - `BUZZ_RELAY_URL` 等非敏感配置经环境变量传入；身份私钥与模型 Key 通过启动时临时只读文件挂载，入口脚本读入后清理宿主机文件
  - 停止后再次启动重建容器并保留状态卷；不启用 Docker 自动重启，避免重启时缺少已清理的临时凭据
- **SOUL/SKILL 装载**：沿用"镜像控制目录 + 入口脚本安装到 profile"模式；分身差异部分由挂载只读文件传入，不重建镜像。

### 4.4 Buzz 接入流程

1. 客户端用分身身份探测 Relay；若返回 `403 relay_membership_required`，展示明确的引导卡片：请 Relay 管理员在 Buzz 中把该公钥添加为成员。
2. 成员授权后，客户端可选发布 Agent Directory（kind 10100）记录并设置 profile（显示名、头像）。
3. 配置解读沿用验证过的语义：`BUZZ_ALLOW_ALL_USERS=true` + 空 allowlist = Relay 全体成员可 @；`BUZZ_REQUIRE_MENTION=true` 保持 @ 门槛。

### 4.5 事实更新闭环

```text
点击“更新事实”
  → git fetch（或用户指定新 commit）/ 重扫目录
  → 展示变更预览（新增/修改/删除文件数，含提交摘要）
  → 构建新快照（旧快照保留）
  → 重启容器并切到新快照（秒级中断）
  → 状态回显新快照 ID 与来源 commit
```

回滚：客户端保留最近 N 个快照，一键切回上一快照并重启。

### 4.6 会话记忆

- Hermes 内置 `memory` 工具负责判断是否抽取和保存记忆，客户端不自动把每条消息写入记忆。
- 频道（含 thread）使用频道级目录，DM 使用 DM 级目录；目录名由 Relay 地址与会话 ID 哈希得到，不把原始 ID 暴露在路径中。
- 只启用共享 `MEMORY.md`，关闭 `USER.md`，避免把频道内容变成跨会话用户画像；缺少 Buzz 会话身份时 fail-closed。
- `memory.write_approval=false`：Buzz Gateway 不提供 Hermes CLI 的审批入口，记忆写入由分身依据上述最小化策略自动决定；仍保留 skills 写入审批。

## 5. 关键决策与权衡

| 决策 | 选择 | 理由 | 放弃的替代方案 |
|---|---|---|---|
| 快照构建器实现 | Rust 重写（`fork-core` 独立 crate） | 客户端零 Python 依赖；格式已冻结；与 UI 解耦后可独立编译测试 | 打包 Python 运行时（体积/维护成本高） |
| Git 读取方式 | `git` CLI（`ls-tree -z` + `cat-file --batch`） | 避免 libgit2 跨平台构建复杂度；与调用 docker CLI 的模式一致；batch 模式一次进程读取全部 blob | git2 crate（vendored 构建在 Windows CI 显著变慢） |
| Docker 调用 | CLI + compose | 跨平台一致、错误信息可读 | Docker Engine API（socket 权限复杂）/ bollard（绑定 depth） |
| 镜像策略 | 预构建推送 ACR | 同事端免构建、版本可控 | 客户端本地构建（首次体验差、Rust 工具链依赖） |
| 知识隔离 | 沿用受保护索引 + fail-closed | 已验证的边界，防止内部文件误入 | 纯目录挂载（无索引校验） |
| 凭据存储 | OS keyring | 私钥/API Key 不落盘明文 | 加密文件（主密钥管理复杂） |
| 远端模式预留 | 数据模型 + 运行抽象层留接口 | 二期可加远端 Provider | 一期直接做双模式（复杂度高） |

## 6. 实施里程碑

**M1 — 分身最小闭环（本机 Docker）**
- Fork 数据模型与本地存储（含 keyring 集成）
- 知识快照 Rust 构建器（Git 源 + 目录源，敏感扫描，格式兼容）
- Docker 探测与运行编排（生成参数、拉起/停止容器、状态查询）
- 创建向导 UI：身份 → 知识源 → 人格（内置模板）→ 模型 → Buzz 配置 → 启动
- 验收：新机器上仅凭客户端 + Docker Desktop 拉起一个可被 @ 的分身

**M2 — 事实更新与可观测**
- 事实更新闭环（变更预览、快照重建、切换重启、回滚）
- 运行日志查看、容器状态面板、Buzz 连接状态
- 多分身管理优化（列表、复制配置、批量启停）

**M3 — 打磨与远端预留**
- 人格模板库与编辑器体验；快照 diff 展示
- Agent Directory / profile 设置 UI
- 运行抽象层接口定型（LocalDockerProvider），远端 Provider 留桩

## 7. 风险与开放问题

- **Docker Desktop 依赖**：同事需自行安装 Docker Desktop（Windows/macOS）；安装引导质量直接影响上手成功率。
- **镜像分发**：正式发布包在 CI 构建时使用 `ACR_NAMESPACE` 注入 ACR 镜像地址；本地开发默认使用 `buzz-fork-hermes:dev`。ACR 拉取需要同事网络可达，私有仓库需先执行 `docker login`；若不可达需提供离线导入包。
- **模型凭据费用**：每个分身自带模型 Key（已确认，2026-09-16），不由团队共享额度。
- **Relay 成员授权**：新分身身份需要 Relay 管理员授权，无法完全自助；客户端只能做好引导。
- **Windows 卷路径与换行符**：快照文件哈希对 EOL 敏感，构建器需明确以字节为准并在文档说明。

## 8. 已确认决策（2026-09-16）

- **产品命名与仓库策略**：原地升级 `kaiwu-clone` 仓库，不新建仓库。
- **模型费用**：每分身自带模型 Key，存入 OS keyring。
- **镜像 ACR**：地址稍后提供；开发期本地构建，CI 推送配置留空待填。
