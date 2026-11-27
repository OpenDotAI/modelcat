# 用终端管理 ModelCat

Apple Silicon 版的 CLI 随 App 一起分发。在 Finder 中将 App 复制到一个可写目录后，把下面命令中的路径替换为实际位置。在同一个终端会话里运行以下命令即可，无需先打开窗口。

```sh
modelcat() { "/Applications/ModelCat.app/Contents/MacOS/modelcat" "$@"; }
modelcat login --key-file /path/to/platform-key
modelcat workspace start
modelcat workspace status
modelcat connections list
```

登录读取平台发放的 API Key，验证成功后保存与桌面共用的登录标记。也支持通过标准输入传入密钥。第一次启动会准备 Linux 虚拟机并下载镜像。默认选择桌面使用的工作区；使用独立目录时，在每条命令上指定相同的 `--data-dir /absolute/path/ModelCatVMData`。

## 工作区与工具

```sh
modelcat workspace resources
modelcat workspace resources --cpus 2 --memory-gib 2 --disk-gib 12
modelcat workspace monitor --seconds 300
modelcat workspace directory
modelcat workspace stop-on-exit false
modelcat tool network claude online
modelcat tool start claude
modelcat tool logs claude
modelcat tool run claude -- --version
```

监控支持最近一次采样、5 分钟、1 小时和 24 小时，对应 0、300、3600、86400 秒。先停止工具再更改外网权限。资源上限下次启动工作区时生效。用 `workspace stop` 停止虚拟机；已关闭窗口的情况下可用 `workspace directory /absolute/path/ModelCatVMData` 修改默认目录。用 `workspace logout` 停止工作区并退出平台。

## 连接与试调用

```sh
modelcat connections save connection.json --check-only
modelcat connections save connection.json
modelcat status
modelcat trial MODEL_ID "你好" --stream
modelcat requests
modelcat requests REQUEST_ID
```

连接文件包含 `label`、`base`、`protocol` 和可选的 `key`。协议可选 `messages`、`chat_completions`、`responses`、`ollama`。也可以把 JSON 通过标准输入交给 `connections save -`。用 `connections authorize 名称` 添加 Codex 授权连接，按终端提示在浏览器输入设备码；重新授权可加 `--id ID`。使用 `status` 返回的本地模型 ID 试调用。用 `connections --help` 查看改名、重新检测、刷新授权、更新和移除连接的命令。

桌面「账号」页的 Codex 资料可通过 `account profiles` 列出，或用 `account profiles --create 名称` 创建。用 `account --profile ID login` 发起授权，在浏览器确认后终端会显示结果。支持 `status`、`logout` 与登录代理 `login --proxy URL`。

## 卖家、网络与运行记录

```sh
modelcat bind --file binding.json
modelcat api /api/config > config.json
# 编辑 config.json 中的 offers、concurrency、upstream_proxy 等配置
modelcat config --file config.json
modelcat offer-list
modelcat offer-save MODEL_ID --input 0.15 --output 0.60
modelcat quotes --model MODEL_ID
modelcat earnings
modelcat api /api/activity
modelcat api /api/traffic/5m
modelcat api /api/token-usage/24h
modelcat network check claude
modelcat network check claude-oauth
```

`quotes` 不带参数时提交所有已启用报价，也可以重复传 `--model`。同一模型已有待审报价时，节点返回 `pending_models`；重复报价由节点检查。单个报价使用配置中的 `offers[].enabled`，整个节点使用 `pause` 和 `resume`。

`modelcat api /api/路径` 使用本地管理身份发送 GET；`--body 文件` 或 `--body -` 发送 POST JSON。输出直接写入标准输出，支持流式响应。例如 `api '/api/request-history?status=failed'` 查询失败请求。节点的管理路由见 [control.rs](apps/seller/src/control.rs)。

报价编辑器的未提交草稿保存在当前桌面浏览器。两端共用节点生成的候选模型、参考价和已保存报价。用 `offer-save MODEL_ID` 采用参考价，也可单独指定输入或输出价；缺少参考价时必须填写相应单价。保存后再用 `quotes --model MODEL_ID` 送审。
