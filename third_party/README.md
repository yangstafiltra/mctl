# third_party

这里的两个 crate 原先位于仓库外的 `[mctl]plugin/` 目录，通过
`../[mctl]plugin/...` 相对路径作为编译期依赖。为了让 mctl 本体可以
单独 clone / 打包（AUR、GitHub release），源码已 vendored 进来。

| crate | 上游 | 许可证 |
|---|---|---|
| `tui-wm-core` | `[mctl]tui-wm/tui-wm/tui-wm-core` | MIT |
| `bedrock-launcher-core` | `[mctl]bedrock-launcher/bedrock-launcher/bedrock-launcher-core` | MIT |

两份代码保持原样，仅把 Cargo workspace 继承改成显式版本号
（`version.workspace = true` → 具体值），以便独立构建。
MIT 与仓库整体的 Apache-2.0 兼容；各自保留原许可证。
