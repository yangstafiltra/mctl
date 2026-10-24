#[derive(Debug, Clone)]
pub struct Account {
    pub provider: String,
    pub name: String,
    pub logged_in: bool,
}

#[derive(Debug, Clone)]
pub struct WorldInfo {
    /// 显示名（levelname.txt 优先，回退目录名）
    pub name: String,
    /// 世界目录名（saves 下的目录）。
    /// 图标路径和 `--quickPlaySingleplayer` 都必须用它——显示名可能和目录名不同。
    pub dir: String,
    /// 最近游玩（目录修改）时间，格式 MM-DD HH:mm
    pub last_played: String,
    /// 世界目录近似大小（人类可读，如 "12.3 MB"；扫描失败 = "—"）
    pub size: String,
}
