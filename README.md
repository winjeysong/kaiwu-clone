# 开物分身 · Kaiwu Clone

团队的 Buzz 分身工坊桌面客户端。可以在本机管理 Nostr 身份，将 Git 仓库或本地目录构建为知识快照，并用 Docker 运行接入 Buzz Relay 的分身。

身份私钥保存在本机应用数据目录；模型 API Key 保存在系统凭据存储。启动分身时，客户端临时挂载凭据文件供容器读取，随后删除临时文件，凭据不会写入 Docker 的环境变量配置。拥有本机 Docker 管理权限的人仍可访问运行中的容器，请只在可信设备上使用。

## 运行分身

安装并启动 Docker Desktop，在客户端「设置」中确认检测通过。创建身份和分身、构建知识快照后即可启动。正式安装包首次启动会拉取运行时镜像；ACR 镜像仓库需要允许拉取，私有仓库需先在本机执行 `docker login`。源码开发时先执行 `docker build -t buzz-fork-hermes:dev runtime`。

停止后再次启动会重建容器并保留分身状态卷。为避免凭据在宿主机长期留存，容器不会随 Docker 自动重启；Docker 重启后请在客户端手动启动分身。旧版已运行的分身也需停止并重新启动一次，才能移除旧容器配置中存留的环境变量凭据。

知识搜索支持原有文本文件，以及 PDF、`.docx`、`.xlsx`、`.pptx` 和常见图片（PNG、JPEG、WebP、TIFF、BMP、GIF）。图片与无文字层的扫描版 PDF 仅做中英文 OCR，不理解画面；旧版 `.doc`、`.xls`、`.ppt` 暂不支持。单个上述文件限 16MB，PDF 最多 100 页（其中最多 20 页 OCR）。更新 Git 知识后，需重新构建快照并切换分身；仅提交文件不会自动更新运行中的快照。

## 下载客户端

每次推送到 `main` 分支后，GitHub Actions 会读取应用版本并创建对应的 `v<版本号>` Release。构建时使用仓库 Secret `ACR_NAMESPACE` 注入正式运行时镜像地址；本地开发默认使用 `buzz-fork-hermes:dev`：

- Windows x64：NSIS `.exe`
- macOS Apple Silicon：`.dmg`
- macOS Intel：`.dmg`

发布新版本前需同步更新 `src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml` 和 `package.json` 中的版本号。

安装包暂未配置商业代码签名。macOS 首次打开时可能需要在“隐私与安全性”中手动允许。

## Docker 构建

```bash
docker build --output type=local,dest=dist/linux .
```

产物位于 `dist/linux/`。Docker 仅用于可选的 Linux 构建；Windows 与 macOS 由 GitHub Actions 原生构建。
