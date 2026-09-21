# AGENTS.md

本文件为 AI 编程助手在此代码仓库中工作时提供指引。

## 项目定位

开物分身（Kaiwu Clone）是团队的**分身工坊**桌面客户端（Tauri 2 + React 19）：

1. **身份**：为 Buzz/Nostr 生成本机身份（公私钥），展示 NIP-19 与 Hex 格式。
2. **分身**：把身份 + 知识来源（Git 仓库 / 本地目录）+ 模型凭据组装成"知识分身"，在本机 Docker 中运行并接入 Buzz Relay，供 Relay 成员 @ 提问。
3. **设置**：检测 Docker 环境。

架构方案与已确认决策见 `docs/fork-platform-plan.md`。

## 常用命令

```bash
pnpm dev                 # Vite 前端开发服务器（端口 1420）
pnpm build               # 前端构建
pnpm tauri dev           # 桌面客户端开发运行（需本机 Rust 工具链）
pnpm tauri build         # 打包安装包

# fork-core 独立测试（不依赖 Tauri 系统库，可在容器内运行）
cd src-tauri && cargo test -p fork-core
```

**包管理器：pnpm（必须）。** Rust 侧工作目录是 `src-tauri/`。

## 技术栈

- **前端**：React 19 + Vite 8 + TailwindCSS 4 + Radix UI + lucide-react；别名 `@/*` -> `src/*`
- **桌面**：Tauri 2（identifier `com.artpalstudio.kaiwu-clone`）
- **Rust**：edition 2021，rust-version 1.77.2；`bech32`/`secp256k1`（Nostr 密钥）、`keyring`（系统凭据）

## 目录结构

```text
src/                           # React 前端
├── App.jsx                    # 三栏导航壳（身份/分身/设置）
├── pages/
│   ├── IdentitiesPage.jsx     # 身份生成与管理（调用 list/generate/…_identity 命令）
│   ├── ForksPage.jsx          # 分身列表、创建向导、详情（构建/启动/停止/日志/连接状态）
│   └── SettingsPage.jsx       # Docker 检测
├── components/ui/             # Radix 封装的基础组件（button/input/alert-dialog）
└── lib/utils.js               # cn() 等工具

src-tauri/
├── src/lib.rs                 # Tauri 入口 + 身份命令（list/generate/get/rename/delete_identity）
├── src/fork_cmd.rs            # 分身 Tauri 命令层（薄封装，转发到 fork-core）
├── fork-core/                 # 分身核心逻辑独立 crate（无 Tauri 依赖，可单独测试）
│   └── src/
│       ├── snapshot.rs        # 知识快照构建（Git commit / 目录源、敏感扫描、受保护索引）
│       ├── store.rs           # Fork 配置存储 + OS keyring + 挂载配置生成
│       └── docker.rs          # Docker CLI 编排（探测/run/stop/status/logs/连接状态）
└── Cargo.toml                 # 主 crate，依赖 fork-core

runtime/                       # 分身运行时容器资产（与客户端解耦，独立构建推送）
├── Dockerfile                 # 通用 Hermes 镜像 + Buzz CLI + 只读知识插件
├── entrypoint.sh              # 从 /fork-config 装载 SOUL/SKILL/config 到 profile
├── config.base.json           # 安全基线配置（工具禁用、记忆关闭、知识工具白名单）
├── merge_config.py            # 基线 + 客户端模型 overlay 合并（只接受 model 键）
├── plugins/fork_knowledge/    # 只读知识插件（knowledge_read / knowledge_search）
└── templates/                 # SOUL.md / SKILL.md 通用人格模板（含 {{FORK_NAME}} 等占位符）
```

## 架构约定（重要）

### fork-core 与 UI 严格分层

知识快照、Fork 存储、Docker 编排都写在 `fork-core`，不依赖 Tauri；`src/fork_cmd.rs` 只做参数校验和转发。**新增逻辑放 fork-core 并补单元测试**，命令层保持薄。

### 安全基线不可放宽

- 容器运行参数固定：只读根文件系统、`cap_drop: ALL`、`no-new-privileges`、非 root（10000）、仅挂载状态卷 / 快照（只读）/ 受保护索引（只读）/ 挂载配置（只读）及启动时临时凭据文件（只读）。停止后再次启动会重建容器并保留状态卷；不启用 Docker 自动重启。
- `merge_config.py` **只接受 overlay 的 `model` 键**；plugins、工具集、记忆开关等完全由镜像内 `config.base.json` 控制。客户端无权通过配置文件扩大能力。
- 知识插件 fail-closed：索引缺失/未登记路径/哈希不符/符号链接一律拒绝；新增文件类型进 `TEXT_SUFFIXES` 时同步审查。
- 快照构建默认拒绝：`.env*`、密钥后缀（`.key/.pem/.p12/.pfx`）、`node_modules/`、`target/` 等；命中私钥块、`nsec1`、`sk-`、`AKIA` 模式直接拒绝构建。

### 快照与容器插件的契约（冻结）

`public-index.json` 的 `schema_version`、文件条目字段（`path`/`sha256`/`size_bytes`/`source_type`/`repository`/`commit`/`source_path`）是客户端构建器与容器内插件的共享契约，修改需两端同步升级。快照不可变：同 ID 构建必须失败，不得原地覆盖。

### 凭据处理

- 身份私钥保存在应用数据目录（`identities/`，0600）。
- 模型 API Key 保存在 **OS keyring**（service `kaiwu-clone-fork`，账号 `<fork-id>:model`），不写入 JSON 或仓库。
- 启动分身时私钥/API Key 通过临时只读文件挂载，运行时读取后客户端删除宿主机临时文件；不得通过 `docker run -e KEY=value` 传递秘密。Docker 管理员仍可访问运行中的容器。

## CI 发布

- `.github/workflows/build.yml`：推送 main 后构建三平台桌面安装包（macOS arm64/x64、Windows），使用 `ACR_NAMESPACE` 注入正式运行时镜像。发版前同步更新三个版本号。
- `.gitlab-ci.yml`：推送 Tag 时校验对应提交属于 GitLab 默认分支历史，通过后构建并推送 `buzz-fork-hermes` 镜像到 ACR。需要受保护且掩码的变量：`ACR_USERNAME`、`ACR_PASSWORD`、`ACR_NAMESPACE`；Runner 须启用 Docker-in-Docker 的特权模式。

## 注意事项

- 前端通过 `window.__TAURI__.core.invoke` 调用命令（见 `src/App.jsx` 顶部）；新增命令需在 `src-tauri/src/lib.rs` 的 `generate_handler!` 注册。
- Rust 编译验证可完全在容器中进行（`rust:1.95.0-bookworm` + webkit2gtk 系列系统库），无需本机工具链；`fork-core` 无系统库依赖，可直接 `cargo test`。
- 连接状态读取容器内 `/opt/data/profiles/fork/gateway_state.json` 的 `platforms.buzz`（`state`/`error_code`/`needs_attention`）；`relay_membership_required` 由 UI 转成授权引导卡片。
- `src-tauri/gen/`、`src-tauri/target/`、`src-tauri/fork-core/target/` 均为构建产物，勿提交。
