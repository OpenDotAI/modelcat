# ModelCat 原生虚拟机实验版

本版使用 Tauri 窗口与内置 Lima 2.3.0 的 Apple Virtualization.framework 后端，在专用 Ubuntu Linux VM 中运行现有 ModelCat 服务。不是 Kata／KVM，不需要安装 Docker、Homebrew 或修改 shell 配置。

## 安装与存储

1. 适用于 Apple Silicon Mac，macOS 13.5 或更高版本；本次仅在构建机当前 macOS 版本验证。
2. 打开 DMG，将 `ModelCat Experiment.app` 拖入 Applications，替换之前的实验 App。包未公证，首次打开可能需要在“系统设置 → 隐私与安全性”批准。
3. 打开后先显示工作区准备页，默认存储位置为 App 旁边的 `ModelCatVMData`（标准安装即 `/Applications/ModelCatVMData`），不在 App 包内，也不在 DMG 内。
4. 可以选择其他存放位置，程序会在其下创建 `ModelCatVMData`。目录必须可写、位于用户 HOME 之外，路径过长时会提示选择更短位置。默认目录无写权限时选择其他磁盘目录，无需提权。
5. 点击“启动工作区”，首次联网下载并校验约 214 MB Ubuntu Minimal 系统镜像，创建上限 8 GiB 的虚拟磁盘，分配 2 CPU、2 GiB 内存。镜像缓存和管理文件会额外占用磁盘；启动后在同一窗口显示原有管理页。
6. 再次打开 App 时选择同一存储位置继续使用；本版不在 HOME 写入用于记忆位置的业务配置。正常关闭窗口或退出 App 会停止 VM，保留数据。准备期间需等待完成或错误提示后再退出。
7. 删除 App 只删除程序。确认 VM 已停止且实验数据不再需要后，再删除所选 `ModelCatVMData` 文件夹，可集中清理虚拟磁盘、缓存、SSH 管理密钥和日志。旧版本在 `~/.modelcat-desktop-experiment` 的数据不会被自动迁移或删除。

## 隔离边界

- 业务服务、依赖、业务密钥、配置、日志位于 Linux VM 内；宿主不挂载 HOME 或其他文件夹到 VM，也不转发 SSH agent，不导入宿主 SSH 公钥。
- 宿主保留窗口、VZ 管理进程、SSH 通道及虚拟磁盘。内置运行时不安装到系统 PATH；管理进程的 HOME、LIMA_HOME、缓存与临时目录均重定向到所选目录。
- 仅显式转发业务管理 TCP 端口：Mac `127.0.0.1:19701` → guest `127.0.0.1:19700`，其余自动业务端口转发被禁用；VM 管理通道另由 Lima 管理。该版不是断网沙箱，VM 可联网。
- 管理窗口使用 WebView 临时会话，避免把登录会话写入持久浏览器数据存储。macOS／WebView 仍可能在用户 Library 中生成系统缓存、偏好文件，不承诺宿主完全零写入。
- 原管理页的节点绑定、模型配置和记录页面继续使用；“本机导入”现在扫描的是 Linux VM，不会读取 Mac 上现成的 Codex／Claude 登录。文件选择导入只读取用户明确选择的文件。OAuth 网页仍在宿主浏览器打开，真实登录、回调及接单尚未验收。

## 构建

需要 Mac、Xcode Command Line Tools、近期 stable Rust 与 Python 3。只构建 Apple Silicon 包：

```sh
python3 scripts/prepare-vm-runtime.py
cargo build --release --locked --features desktop --manifest-path apps/seller/Cargo.toml
cargo test --release --locked --features desktop --manifest-path apps/seller/Cargo.toml
python3 scripts/package-modelcat.py --binary apps/seller/target/release/modelcat --target aarch64-apple-darwin --version 0.2.0-vm.1 --output release/vm --desktop
```

准备脚本校验固定 SHA-256，下载 Lima 2.3.0（Apache-2.0）与上游 ModelCat v0.2.0-build.2 的 Linux aarch64 服务。guest 服务未改变业务代码；Mac 源码中的 `desktop` feature 只负责窗口和 VM 管理。依赖许可随包保留，Ubuntu 镜像地址和固定校验和见 `apps/seller/vm.yaml`。不启用 desktop feature 时仍可构建原 CLI，但它不提供本版 VM 隔离启动流程。

## 故障恢复

启动失败会在页面显示错误，管理日志保存在所选目录的 `vm-error.log` 及 `lima/modelcat` 下。强制退出后，可使用内置管理程序显式停止本工作区；把第一行路径改为实际存储位置：

```sh
VM_DATA=/Applications/ModelCatVMData
HOME="$VM_DATA/host-home" LIMA_HOME="$VM_DATA/lima" TMPDIR="$VM_DATA/tmp" "/Applications/ModelCat Experiment.app/Contents/Resources/vm-runtime/lima/bin/limactl" stop modelcat
```

## 验证范围

验证包括源代码测试、HOME／App 包／路径跳转拒绝检查，以及真实 Apple VZ Linux VM 的服务启动、本地转发认证 API、guest 配置文件存在、内核挂载记录无 virtiofs／9p 宿主目录、VM 停止。真实账号导入、OAuth 回调、生产网关接单与收益不包含在已通过结果中；首次安装与目录选择交互仍需用户试装确认。
