# ModelCat updates

## 本轮修复

- 修复 DSH 自定义模型默认发送 128000 预算而被网关拒绝的问题；按具体型号及网关有效能力生成参数。
- 明确区分官方能力、平台限制与旧节点兼容上限，支持逐模型上下文、输入类型和 reasoning。
- DSH App 改为缓存启动、独立端口、就绪后打开及重复点击复用；启动失败有可见反馈。

## 0.2.0

- 客户端命名为 modelcat，保留旧 profile 与命令入口兼容。
- macOS App + DMG 安装，归档保留执行权限；签名与公证可接入发布流程。
- MSP 节点绑定、模型发现、多选报价、Kiro Session 导入与收款/收益工作台。
- Claude 风格本地管理界面。
- 为 Codex、Claude Desktop、DSH 创建独立网关配置与 App 入口，重复执行时刷新可用模型。
- 节点自动识别并上报公网 IP，保留网关实际观察到的连接地址与来源。
- 安装包内保留 ModelCat 与 Asale 的许可证和第三方声明。
- Release 包含源码版本、平台信息与 SHA-256 校验和。

发布说明会明确签名状态。无 Developer ID 的包依然受 Gatekeeper 首次运行检查。
