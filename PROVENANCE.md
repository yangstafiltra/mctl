# PROVENANCE · 协议常量与实现出处登记

本文件登记 mctl 里所有"来自外部世界的魔数/常量/端点"的出处口径，便于开源审查。

总口径：**mctl 实现的是网易客户端与平台之间公开可观察的互操作协议。**
分析第三方启动器只用于确认事实（端点、字段、时序、取值约束），代码一行不抄；
仓库内不含任何反编译产物，也不分发第三方/官方的二进制文件。

## 1. 加密与签名

| 常量 / 机制 | 位置 | 出处口径 |
| --- | --- | --- |
| HTTP 体 AES 密钥表、IV、keyIdx 位布局 | `src/netease/crypto.rs` | 网易客户端↔平台协议的公开 RE 事实与客户端抓包（多份独立实现一致） |
| user-token 签名盐 + MD5 流程 | `src/api/mod.rs`（`post_signed`） | 同上 |
| `Skip32` 角色 UUID 密钥 | `src/netease/crypto.rs`（`role_uuid`） | 同上；社区第三方实现亦公开同一事实 |
| AuthLib/RPC 帧加密与 CRC | `src/netease/crypto.rs`、`src/netease/rpc.rs` | 客户端 socket 协议事实（抓包 + 公开 RE） |

## 2. 平台接口常量

| 常量 / 机制 | 位置 | 出处口径 |
| --- | --- | --- |
| 网关/客户端主机名（x19 系列） | `src/api/netease.rs`（`GATEWAY`/`CLIENT`） | 官方客户端行为观察（DNS + 抓包） |
| `md5_pair(版本)`：`bootstrapMd5` / `datFileMd5` | `src/api/netease.rs` | 官方 runtime 文件的**内容指纹**（不是代码）：两个独立第三方实现逐值一致，已交叉验证 |
| `version_enum(版本)`：`game-patch-info` 的 `mc_version` 编码 | `src/launcher/netease.rs` | 官方接口的取值事实（`1.12.2 → 1012002` 这类，来自接口行为与公开资料） |
| `crc_salt` 候选表 | `src/netease/yggdrasil.rs` | 盐由游戏服务端 `GET /api/home` 下发；服务端可被 `--crc_salt` 覆盖，mctl 用候选探测 + 命中缓存 |
| X19 协议版本串 | 运行期刷新（`src/api/netease.rs`） | **不写死**：从官方更新接口获取，带本地缓存 |
| 进服硬约束（`gameid=[0,0]`、`filterkey` 32 位小写、`GameType:2` 等） | `src/launcher/mod.rs`（`netease_preflight`） | 官方客户端行为 + 社区踩坑事实（行为约束，非代码） |
| 联机/租赁服 `game_id` 语义 | 全为运行期参数 | `game_id` = **所选服务器的 item id**；不得硬编码任何具体服的 id |
| Java 版本矩阵（<1.16→8 / 1.16–1.20.5→17 / ≥1.20.6→21） | `src/launcher/java.rs` | 官方启动器按版本分发 runtime 的行为事实 |

## 3. 官方二进制的处理（重要）

| 物 | 处理方式 |
| --- | --- |
| 客户端包、natives、runtime 文件 | **不随仓库分发**：安装时从官方 CDN 下载并校验 md5；缺失的 runtime 文件按「已安装版本 → 本地缓存 → 官方包」就地补齐（`src/launcher/netease.rs` 的 `ensure_runtime_native*`） |
| 曾经内嵌的 runtime native | 已删除并清理历史：该文件曾以内嵌 base64 形式存在于源码中；现已从工作树与**全部提交历史**中移除（`refs/original` 已清理、仓库已 `gc`），改为运行期从官方包就地补齐 |

## 4. 明确不做的事

- 不实现/不引用任何第三方启动器的私有后端、账号池、插件生态；
- 不绕过反作弊、不改官方二进制、不伪装机器码；
- 不在仓库内出现反编译源码、抓包原文（含真实 uid/token/cookie/主机名/个人路径）。
