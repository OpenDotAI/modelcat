# ModelCat

Share token and receive coin.

ModelCat 是模型提供者（MSP）的本地客户端。连接 CAT 网关、导入自己明确选择的 Codex / Claude Code / Kiro 登录、识别模型并提交报价，审核后提供服务。订阅凭据保存在本机。

[下载最新版](https://github.com/OpenDotAI/modelcat/releases/latest) · [全部版本与更新说明](https://github.com/OpenDotAI/modelcat/releases)

## macOS

1. 按芯片下载 DMG：Apple Silicon 为 `aarch64-apple-darwin`，Intel 为 `x86_64-apple-darwin`。
2. 打开 DMG，把 **ModelCat.app** 拖到 **Applications**，然后打开。App 自动启动本地服务并在浏览器中打开管理页。
3. 从网站 MSP 工作台下载节点绑定文件，在 ModelCat 的“绑定与设置”中导入。
4. 在“工具与模型”明确导入登录信息，识别模型、多选 ID、填写报价并提交审核；通过后恢复接单。

包内已设置执行权限，不需要用户手动 chmod。当前未配置 Apple Developer ID 时发布为 ad-hoc 签名，仍需按 macOS 的提示，在“系统设置 → 隐私与安全性”批准首次运行。没有声称已公证，不要求关闭 Gatekeeper 或给整个磁盘授权。导入其他 App 的钥匙串或受保护文件时，系统仍可能要求用户授权。

## Linux / 命令行

解压对应平台的 tar.gz，运行：

```sh
./modelcat start
./modelcat open
./modelcat status
./modelcat stop
```

新安装默认 profile 为 `~/.modelcat`；已有 `~/.channel-node` 时继续复用。`MODELCAT_DATA` / `--data-dir` 可指定独立 profile，旧 `CHANNEL_NODE_DATA` 仍兼容，同时设置时旧变量优先以保持已有部署。默认只监听 `127.0.0.1:19700`。

## 买家网关配置

网站买家工作台提供 ZIP 安装脚本。解压后双击 `.command`，首次输入买家 API Key；随后创建 `~/Applications/ModelCat Codex…app`、`ModelCat Claude…app` 或 `ModelCat DSH…app`。安装器在独立目录中保存配置，保留原厂客户端及登录。再次执行会通过网关 `/v1/models` 刷新模型，不会自动请求推理。

```sh
modelcat setup-client --client codex --gateway https://YOUR-GATEWAY
modelcat setup-client --client claude --gateway https://YOUR-GATEWAY
modelcat setup-client --client dsh --gateway https://YOUR-GATEWAY
```

缺少官方客户端时从厂商官方下载并校验 macOS 签名；缺少 Node.js 24.9+ 时为 DSH 下载私有 Node 运行时并校验 SHA-256。下载失败时安装不会被标为成功，可按厂商官网安装后重试。Claude Desktop 使用官方的第三方推理配置；Claude Code 的环境变量不适用于 Desktop。DSH 为独立的 Web UI 入口。

## 构建与发布

仓库只包含 ModelCat 客户端与必要的开源依赖，不含 CAT 网关、运行数据或用户凭据。维护者在 CAT 工作区运行 `make release-modelcat`，先通过 baseline，再用本机已登录的 gh 同步客户端。每次客户端源码同步后，GitHub Actions 构建 macOS Apple Silicon / Intel 与 Linux x64 / ARM64 客户端，通过测试后创建带版本、校验和与更新说明的 Release。失败的构建不会更新 latest。历史 Release 资产不覆盖。

```sh
cargo build --release --locked --manifest-path apps/seller/Cargo.toml
```

macOS 包可以使用 Developer ID 签名与 Apple 公证：设置 `MODELCAT_SIGN_IDENTITY` 和 `MODELCAT_NOTARY_PROFILE` 后运行 `scripts/package-modelcat.py`。正式签名凭据不进入源码。

ModelCat 不隶属于 OpenAI、Anthropic、DeepSeek 或 Kiro。依赖许可见 THIRD_PARTY_NOTICES.md。
