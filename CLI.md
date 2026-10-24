# mctl 命令行（层级上下文）

`mctl` 不带参数进入 TUI；带参数走 CLI。CLI 像 `cd` 一样有「当前位置」，
`list` / `info` 按位置解释。

```
/                 根：上下文索引
/account          账号（token / sead 只报长度）
/local            本地实例
/local/<实例>      某实例的本地地图
/server           联机服务器
/server/<id>      单个服务器
/rental           我的租赁服
/rental/<id>      单个租赁服
/resource         资源下载（先选下载源）
/resource/<源>    某个下载源的资源列表（20 条/页）
/plugin           插件
```

## 层级怎么看

```bash
mctl tree              # 完整命令树，▶ 标当前位置
mctl help              # 根：同 tree；其他层：本层命令 + 上级/下级
mctl pwd               # 当前位置
```

树里的每一行都能直接当命令前缀用，所以「层级」不只存在于 `cd` 状态里：

```bash
mctl local 1.21.1 list         # = mctl local/1.21.1 list
mctl local info 1.21.1         # = mctl local/1.21.1 info
mctl rental <id> start         # = cd rental/<id> 再 start
mctl resource/netease list     # 直接看网易资源第 1 页
```

## 交互式 CLI

```bash
mctl cli
```

进入后 prompt 形如：

```
mctl@yangsta020:/resource/netease>
```

一直停在 CLI 里，直到输入 `exit`（`quit` / `:q` 也可以）。支持单/双引号，
所以带空格的世界名不用转义：`info "我的 世界"`。也可以管道喂命令：

```bash
printf 'cd resource\nlist\nexit\n' | mctl cli
```

## 两种用法

有状态（像 shell）：

```bash
mctl cd local          # 进入 /local
mctl list              # 列实例
mctl cd 1.21.1         # 支持唯一前缀；进入 /local/1.21.1
mctl list              # 该实例的本地地图
mctl info 我的世界      # 单个世界详情
mctl cd ..             # 回 /local
mctl cd -              # 回上一个位置
mctl cd /              # 回根
```

在 `/local` 里还可以直接把子节点写在命令前面（不改状态）：

```bash
mctl 1.21.1 list       # 列该实例的地图
mctl 1.21.1 info
```

一次性（不改当前位置，适合脚本）：

```bash
mctl local list
mctl local/1.21.1 list
mctl server list
mctl server info <item_id>
mctl rental list
mctl rental/<id> info
mctl / install 1.21.1   # 在 /plugin 下强制走全局 install
```

## 资源下载

```bash
mctl cd resource          # /resource：下载源
mctl list                 # 表 1：netease / modrinth
mctl cd netease           # 选网易资源（cd download 也可以）
mctl list                 # 表 1：20 条，带序号/名称/作者/下载量/版本
mctl next                 # 下一页
mctl prev                 # 上一页
mctl page 3               # 跳到第 3 页
mctl category             # 表 1：分类（地图·小游戏 / 冒险 / 模组 / 光影 / 皮肤）
mctl category 2           # 切到冒险地图，回到第 1 页
mctl info 5               # 表 1 详情 + 表 2 可下载版本
mctl download 5           # 下载第 5 条（默认第 1 个版本）
mctl download 5 2         # 下载第 5 条的第 2 个版本
mctl 5                    # 直接输入序号也能下载（交互式 CLI 里同样）
```

- 每页固定 20 条；翻页用 `next` / `prev` / `page <n>`。
- `list 2` 等价于 `page 2`。
- 表格是纯文字，不输出任何图片；每张表有「表 1 / 表 2」表序和行序号。
- 网易资源需要登录（`mctl login`）；Modrinth 不需要。
- 下载保存到设置里的下载目录，默认 `~/.config/mctl/downloads/`。

## 各上下文的命令

| 位置 | 命令 |
|---|---|
| `/` | `list` `status` `cd ...` |
| `/account` | `list` `info` |
| `/local` | `list` `info <实例>` `worlds <实例>` `cd <实例>` |
| `/local/<实例>` | `list` `info [世界]` `worlds` |
| `/server` | `list [offset] [len] [分类]` `info <id>` `cd <id>` |
| `/server/<id>` | `info` `list` |
| `/rental` | `list|mine` `info <id>` `start <id>` `stop <id>` `address <id>` `roles <id>` `cd <id>` |
| `/rental/<id>` | `info` `start` `stop` `address` `roles`（id 可省略） |
| `/resource` | `list` `source <源>` `cd <源>` |
| `/resource/<源>` | `list [页码]` `next` `prev` `page <n>` `category [n|key]` `info <序号>` `download <序号> [版本]` `<数字>` |
| `/plugin` | `list` `info <名称>` `install <url|路径>` `update [名称]` `remove <名称>` |

全局命令任何位置都能用：`login`、`status`、`version`、`salt`、`proxy`、
`install`、`bedrock`、`dl`、`token`、`help`、`tree`、`cli`。

> `/plugin` 下的 `install` 指插件安装（层级优先）；要装游戏版本用
> `mctl / install <版本id>`，或先 `cd /`。其余同名情况同理。

## 约定

- 上下文状态存在 `~/.config/mctl/cli/context`，只有路径、页码、分类 key，
  没有凭据。
- 实例 id 支持唯一前缀；有歧义时会列出候选。
- 账号信息只输出长度，不打印 token / `sead` 原文。
- 旧的平铺命令保留：`mctl login`、`mctl salt`、`mctl proxy`、`mctl install`、
  `mctl plugin ...`、`mctl detail <id>`、`mctl online`。
- Nix 构建见 [`nix/README.md`](nix/README.md)。
