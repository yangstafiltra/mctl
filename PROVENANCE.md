# PROVENANCE · 协议常量与实现出处登记

本文件登记 mctl 里来自外部世界的端点、字段与协议事实的出处口径，便于开源审查。

总口径：**mctl 实现的是各平台公开可观察的互操作协议。** 代码不含任何反编译
产物，也不分发官方或第三方的二进制文件；分析闭源客户端只用于确认事实（端点、
字段、时序、取值约束），实现独立编写。

## 1. 认证与签名

| 常量 / 机制 | 位置 | 出处口径 |
| --- | --- | --- |
| user-token 签名流程 | `src/token.rs`、`src/api/mod.rs` | 网易资源接口的公开 RE 事实与客户端抓包，多份独立实现交叉验证 |
| 4399 登录的请求/响应字段、AES 变换 | `src/crypto.rs`、`src/api/auth.rs` | 公开网页登录流程观察 + 浏览器抓包 |
| LittleSkin OAuth / authlib-injector | `src/api/littleskin.rs` | authlib-injector 公开规范与 LittleSkin 文档 |
| 离线名 UUID 生成 | `src/launcher/mod.rs` | Minecraft 官方离线模式约定 |

## 2. 平台接口

| 常量 / 机制 | 位置 | 出处口径 |
| --- | --- | --- |
| 资源分类、列表、版本、下载字段 | `src/api/mod.rs` | 官方资源中心网页接口行为观察 |
| Java Server List Ping（握手/状态/favicon） | `src/api/mcserver.rs` | Minecraft 公开协议（wiki.vg / minecraft.wiki） |
| GitHub 仓库搜索与 topic 约定 | `src/api/github.rs` | GitHub REST API 公开文档；`mctl-plugin` 是本项目自定 topic |
| 版本清单、资源索引、库下载 | `src/launcher/meta.rs`、`src/launcher/version.rs` | Mojang piston-meta 公开元数据 |
| Modrinth 搜索 / 版本 / 下载 | `src/launcher/modpack.rs` | Modrinth 公开 API 文档 |

## 3. 官方二进制的处理

| 物 | 处理方式 |
| --- | --- |
| 游戏客户端、库、资产、Java runtime | **不随仓库分发**：安装时从官方源下载，并按官方元数据校验；失败可从镜像重试 |
| 网易资源包 | 运行时按接口返回的下载地址获取，不内嵌、不再分发 |

## 4. 明确不做的事

- 不绕过正版验证；正版验证服需要 Microsoft 登录，alpha 未实现。
- 不内置账号池、不代理他人凭据、不实现第三方启动器的私有后端。
- 不把官方客户端或反编译产物放进仓库。

## 5. vendored 的编译期依赖

`third_party/tui-wm-core` 与 `third_party/bedrock-launcher-core` 来自作者自己的
上游工程，均为 MIT，见 `third_party/README.md`。它们只提供窗口管理与基岩版
启动能力，不含平台协议常量。
