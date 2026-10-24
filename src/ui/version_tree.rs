/// Modrinth 版本列表的树状分组行。
/// 473 个版本平铺看不出「1.21.1 下有哪些变体」，这里按 MC 版本号分父行、
/// 组内再按 loader 分子行。**行号 ≠ 版本索引**，所以选中/渲染都走 `TreeRow`，
/// 别再用 `mr_detail_sel` 直接索引 `mr_versions`（改了结构就跳错行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeRow {
    /// MC 版本分组头（例：1.21.1）
    Group {
        mc: String,
        count: usize,
        expanded: bool,
    },
    /// 组内 loader 分组头（例：fabric）
    Loader {
        mc: String,
        loader: String,
        count: usize,
        expanded: bool,
    },
    /// 实际可选中的版本
    Leaf {
        /// 索引进 `mr_versions`
        idx: usize,
        depth: usize,
    },
}

/// 稳定版判定：版本号里含 -beta/-alpha/-rc/-snapshot/-pre 就是预发布。
pub fn is_prerelease(version_number: &str) -> bool {
    let v = version_number.to_ascii_lowercase();
    ["beta", "alpha", "rc", "snapshot", "pre", "dev"]
        .iter()
        .any(|tag| v.contains(&format!("-{tag}")) || v.contains(&format!(".{tag}")))
}

/// 组内排序：稳定版在前，然后版本号倒序（新的在前）。
fn sort_key(v: &crate::launcher::modpack::MrVersion) -> (u8, String) {
    let pre = if is_prerelease(&v.version_number) {
        1
    } else {
        0
    };
    // 反转靠版本号本身，实现不了就地反序——交给下游按字符串倒序比较
    (pre, v.version_number.clone())
}

/// 构建分组行。`collapsed` 里放已折叠的 key（`"mc"` 或 `"mc/loader"`）。
/// ponytail: 不做懒加载，一次构建 473 个版本的全树（几 KB 内存，<1ms）。
/// 真卡了再改成按需展开。
pub fn build_tree(
    versions: &[crate::launcher::modpack::MrVersion],
    collapsed: &std::collections::HashSet<String>,
) -> Vec<TreeRow> {
    // MC 号 → 该 MC 下的版本
    let mut by_mc: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
    for (i, v) in versions.iter().enumerate() {
        let mc = v
            .game_versions
            .iter()
            .find(|g| g.chars().next().is_some_and(|c| c.is_ascii_digit()))
            .cloned()
            .unwrap_or_else(|| "其他".into());
        by_mc.entry(mc).or_default().push(i);
    }
    // MC 号按语义序倒序（26.3 > 1.21.11），不是字典序
    let mut mcs: Vec<String> = by_mc.keys().cloned().collect();
    mcs.sort_by(|a, b| mc_version_key(b).cmp(&mc_version_key(a)));

    let mut rows = Vec::new();
    for mc in mcs {
        let idxs = &by_mc[&mc];
        // 组内：稳定优先 + 版本号倒序
        let mut idxs = idxs.clone();
        idxs.sort_by(|&a, &b| {
            let (pa, va) = sort_key(&versions[a]);
            let (pb, vb) = sort_key(&versions[b]);
            pa.cmp(&pb).then_with(|| vb.cmp(&va))
        });

        let mc_expanded = !collapsed.contains(&mc);
        rows.push(TreeRow::Group {
            mc: mc.clone(),
            count: idxs.len(),
            expanded: mc_expanded,
        });
        if !mc_expanded {
            continue;
        }

        // 组内按 loader 再分一层
        let mut by_loader: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
        for &i in &idxs {
            let l = versions[i]
                .loaders
                .first()
                .cloned()
                .unwrap_or_else(|| "any".into());
            by_loader.entry(l).or_default().push(i);
        }
        // 单 loader 时不必再套一层（Sodium 全是 fabric，多一层纯属噪音）
        if by_loader.len() <= 1 {
            for i in idxs {
                rows.push(TreeRow::Leaf { idx: i, depth: 1 });
            }
            continue;
        }
        let mut loaders: Vec<String> = by_loader.keys().cloned().collect();
        loaders.sort();
        for ld in loaders {
            let key = format!("{mc}/{ld}");
            let ldx = &by_loader[&ld];
            let ld_expanded = !collapsed.contains(&key);
            rows.push(TreeRow::Loader {
                mc: mc.clone(),
                loader: ld.clone(),
                count: ldx.len(),
                expanded: ld_expanded,
            });
            if ld_expanded {
                for &i in ldx {
                    rows.push(TreeRow::Leaf { idx: i, depth: 2 });
                }
            }
        }
    }
    rows
}

/// MC 版本号的语义排序键：主版本大者优先，`26.3` 要排在 `1.21.11` 之后。
fn mc_version_key(mc: &str) -> (u64, Vec<u64>) {
    let mut segs: Vec<u64> = mc
        .split(['.', '-'])
        .map(|s| s.parse::<u64>().unwrap_or(0))
        .collect();
    // 补齐长度，避免 [1,21] 和 [1,21,1] 比较时错位
    while segs.len() < 3 {
        segs.push(0);
    }
    (segs.first().copied().unwrap_or(0), segs)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::modpack::MrVersion;

    fn v(num: &str, mc: &str, loaders: &[&str]) -> MrVersion {
        MrVersion {
            id: num.into(),
            version_number: num.into(),
            game_versions: vec![mc.into()],
            loaders: loaders.iter().map(|s| (*s).into()).collect(),
            files: vec![],
            dependencies: vec![],
        }
    }

    /// MC 版本必须语义排序：26.3 > 1.21.11，不能按字典序（字典序里 26 排在 1.21 后面恰好对，
    /// 但 1.9 < 1.21 字典序会错）
    #[test]
    fn mc_ordering_is_semantic_not_lexicographic() {
        let mut keys = vec!["1.9", "1.21.11", "26.3", "1.21.4", "1.20.1"];
        keys.sort_by(|a, b| mc_version_key(b).cmp(&mc_version_key(a)));
        assert_eq!(keys, vec!["26.3", "1.21.11", "1.21.4", "1.20.1", "1.9"]);
    }

    #[test]
    fn stable_versions_sort_before_prerelease() {
        assert!(!is_prerelease("1.21.1"));
        assert!(!is_prerelease("15.0.0"));
        assert!(is_prerelease("1.21.1-beta.1"));
        assert!(is_prerelease("15.0.0-alpha.4"));
        assert!(is_prerelease("1.21-rc-1"));
    }

    /// 折叠 MC 组后，它的叶子不能出现在树里
    #[test]
    fn collapsed_group_hides_its_leaves() {
        let versions = vec![
            v("1.21.1", "1.21.1", &["fabric"]),
            v("1.21.1-beta.1", "1.21.1", &["fabric"]),
            v("1.20.4", "1.20.4", &["fabric"]),
        ];
        let mut c = std::collections::HashSet::new();
        c.insert("1.21.1".to_string());
        let rows = build_tree(&versions, &c);
        let leaves: Vec<usize> = rows
            .iter()
            .filter_map(|r| match r {
                TreeRow::Leaf { idx, .. } => Some(*idx),
                _ => None,
            })
            .collect();
        assert_eq!(leaves, vec![2], "折叠后只应剩 1.20.4 那条叶子");
        assert!(matches!(
            rows[0],
            TreeRow::Group {
                expanded: false,
                ..
            }
        ));
    }

    /// 展开时稳定版必须在同组的第一条叶子位置（用户明确要求）
    #[test]
    fn stable_release_is_first_leaf_in_group() {
        let versions = vec![
            v("1.21.1-beta.1", "1.21.1", &["fabric"]),
            v("1.21.1-rc-1", "1.21.1", &["fabric"]),
            v("1.21.1", "1.21.1", &["fabric"]),
        ];
        let rows = build_tree(&versions, &std::collections::HashSet::new());
        let first_leaf = rows
            .iter()
            .find_map(|r| match r {
                TreeRow::Leaf { idx, .. } => Some(*idx),
                _ => None,
            })
            .expect("应有叶子");
        assert_eq!(
            versions[first_leaf].version_number, "1.21.1",
            "稳定版应在最前"
        );
    }

    /// 单 loader 不该多套一层（Sodium 全 fabric，多一层是噪音）
    #[test]
    fn single_loader_group_has_no_nested_level() {
        let versions = vec![v("1.0.0", "1.21.1", &["fabric"])];
        let rows = build_tree(&versions, &std::collections::HashSet::new());
        assert_eq!(rows.len(), 2, "应是 1 组 + 1 叶子，实际 {:?}", rows);
        assert!(!rows.iter().any(|r| matches!(r, TreeRow::Loader { .. })));
    }

    /// 多 loader 时才分第二层
    #[test]
    fn multi_loader_group_nests() {
        let versions = vec![
            v("1.0.0-fabric", "1.21.1", &["fabric"]),
            v("1.0.0-neo", "1.21.1", &["neoforge"]),
        ];
        let rows = build_tree(&versions, &std::collections::HashSet::new());
        assert_eq!(rows.len(), 5, "1 组 + 2 loader 组 + 2 叶子");
        let loaders: Vec<String> = rows
            .iter()
            .filter_map(|r| match r {
                TreeRow::Loader { loader, .. } => Some(loader.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(loaders, vec!["fabric", "neoforge"]);
    }
}
