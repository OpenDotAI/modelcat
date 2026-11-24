# ModelCat

Share token and receive coin.

ModelCat 是模型提供者（MSP）的本地客户端。macOS 隔离工作区支持 Claude API Key 和 Ollama，接入的模型可在本地调用，也可绑定 CAT 网关、提交报价后提供服务。

[下载最新版](https://github.com/OpenDotAI/modelcat/releases/latest) · [全部版本与更新说明](https://github.com/OpenDotAI/modelcat/releases)

## Apple Silicon 桌面开发预览

在仓库根目录运行 `./scripts/dev-desktop.sh`。脚本用相邻
`toolchain` 中的 Rust 工具链构建 Tauri 和 VM 节点，并直接打开 macOS 窗口。
首次验证平台 API Key 后，默认工作区位于相邻的 `ModelCatVMData`。修改代码后关闭窗口，再运行同一条命令。

关闭主窗口或退出 ModelCat 时，默认会停止工作区虚拟机。可在 **ModelCat → 设置…（⌘,）** 关闭「退出时关闭虚拟机」，让工作区继续运行。
也可以通过 **工作区 → 停止工作区** 手动关闭虚拟机，或在应用退出后运行 `./scripts/dev-desktop.sh --stop`。

开发预览打开工作区后，可以运行 `./scripts/dev-desktop.sh status` 查看节点状态，或用
`./scripts/dev-desktop.sh pause` 暂停接单。相同入口也支持 `bind`、`quotes` 和 `resume`。

如需验收 Claude 模拟账号授权，先安装 Google Chrome，再运行 `./scripts/dev-desktop.sh --oauth-preview`。
默认工作区就绪后，另开一个终端运行 `./scripts/dev-desktop.sh --oauth-preview-proxy`。该命令通过 SSH 把本机的 `127.0.0.1:18779` 转发到 VM 内的测试代理。
在「模型服务 → Claude 账号模拟授权」发起授权并在系统浏览器确认。
授权成功后可在「本地调用」选择 `claude-oauth-preview`。该模型通过测试代理校验模拟令牌并返回固定结果。
返回客户端可刷新模拟令牌。停止测试代理可以验证授权、刷新和模型调用失败。
模拟凭据只保留在节点进程内存中。

## macOS 隔离实验版

1. 打开 Apple Silicon DMG，把 **ModelCat Experiment.app** 拖到有写入权限的目录，再打开 App。
2. 输入平台 API Key。验证通过后，App 在专用 Linux VM 中启动 Docker 和 ModelCat 节点，并显示工作区概览。
3. 在「模型服务」连接 Claude API Key，或填写 Ollama 地址并登记模型。在「本地调用」读取连接参数。
4. 如需对外提供模型，从 MSP 工作台下载节点绑定文件，在「绑定与设置」导入。选择模型、填写报价并提交审核，获批后恢复接单。

Claude 兼容中转站可在「模型服务 → Claude API Key → 连接选项」填写 HTTPS API 地址和对应密钥。中转模型在「本地调用」中可用，仅向该地址发送服务商密钥。

「工作区 → 工具管理 → Codex」提供本机 Codex 的 ChatGPT 登录。需要预先安装支持 `app-server` 的官方 Codex CLI。代理留空时使用系统默认浏览器。如果填写 Mac 登录代理，ModelCat 用独立的 Chrome 窗口打开授权页，并将同一代理传给 Codex 登录服务。登录保存在当前工作区的 `codex-account` 目录，可在同一面板退出。VM 内的 Codex 工具和「模型服务」各有自己的配置。

包内已设置执行权限，不需要用户手动 chmod。当前未配置 Apple Developer ID 时发布为 ad-hoc 签名，仍需按 macOS 的提示，在“系统设置 → 隐私与安全性”批准首次运行。没有声称已公证，不要求关闭 Gatekeeper 或给整个磁盘授权。导入其他 App 的钥匙串或受保护文件时，系统仍可能要求用户授权。

### 在本机 Claude Code 中调用

在「本地调用」选择已登记的模型。下面用 `ollama/qwen2.5:0.5b` 做一次纯文本试调用。把地址和模型 ID 换成界面显示的值，运行到密钥提示时，粘贴同一页面的工作区密钥并按回车。

```sh
printf '工作区密钥: '
IFS= read -rs MODELCAT_LOCAL_KEY
printf '\n'
env -u ANTHROPIC_API_KEY \
  ANTHROPIC_BASE_URL=http://127.0.0.1:19701 \
  ANTHROPIC_AUTH_TOKEN="$MODELCAT_LOCAL_KEY" \
  claude --model 'ollama/qwen2.5:0.5b' --tools '' \
  --system-prompt '请直接回答用户的问题。' -p '请用一句话解释什么是哈希。'
unset MODELCAT_LOCAL_KEY
```

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
