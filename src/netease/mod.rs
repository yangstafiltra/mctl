pub mod authlib;
pub mod crypto;
pub mod proxy;
pub mod rpc;
pub mod yggdrasil;

use std::path::Path;

/// 探测当前网络运营商线路（租赁服三网线路选择）
pub fn detect_isp() -> String {
    // 尝试读取 /sys/class/net/*/address 前缀或路由表判断，简化：默认 "cmcc"
    "cmcc".to_string()
}

/// 扫描 `<root>/mods` 构造 Yggdrasil ModList JSON。
///
/// 官方（InstallerService.InstallGameMods）两类 jar 的上报格式不同：
/// - 核心 mod：落盘 `{res_name 去后缀}@{MTypeId}@{EntityId}.jar`，上报 `{ItemId}@{MTypeId}@0.jar`
///   （两者实测同值），`iid = ItemId` → 安装时记进 `<root>/.mods-names.json`，这里按映射取；
/// - 游戏组件 / 用户 mod：`modPath/id = 文件名`，`iid = 文件名 @ 前段`，md5 现算。
///
/// md5 一律**大写**（官方 `Md5 = jar_md5.Upper()` / `ComputeMd5FromFile` 同款）。
pub fn mods_json(root: &Path) -> String {
    use md5::Digest;
    // 落盘名 → (上报名, iid)
    let names: std::collections::HashMap<String, (String, String)> =
        std::fs::read_to_string(crate::launcher::netease::mod_names_path(root))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v.as_object().cloned())
            .map(|m| {
                m.into_iter()
                    .filter_map(|(k, v)| {
                        let mp = v.get("modPath")?.as_str()?.to_string();
                        let iid = v.get("iid")?.as_str()?.to_string();
                        Some((k, (mp, iid)))
                    })
                    .collect()
            })
            .unwrap_or_default();
    let mut mods = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root.join("mods")) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "jar") {
                continue;
            }
            let Ok(data) = std::fs::read(&p) else {
                continue;
            };
            let hash = md5::Md5::digest(&data);
            let md5hex: String = hash.iter().map(|b| format!("{b:02X}")).collect();
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let (mod_path, iid) = match names.get(&name) {
                Some((mp, iid)) => (mp.clone(), iid.clone()),
                None => {
                    let iid = name.split('@').next().unwrap_or(&name).to_string();
                    (name.clone(), iid)
                }
            };
            mods.push(serde_json::json!({
                "modPath": mod_path, "name": "", "id": mod_path,
                "iid": iid, "md5": md5hex, "version": "",
            }));
        }
    }
    mods.sort_by(|a, b| a["modPath"].as_str().cmp(&b["modPath"].as_str()));
    serde_json::to_string(&serde_json::json!({ "mods": mods }))
        .unwrap_or_else(|_| "{\"mods\":[]}".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use md5::Digest;

    #[test]
    fn mods_json_scans_jars_with_md5() {
        let root = std::env::temp_dir().join(format!("mctl_mods_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("mods");
        std::fs::create_dir_all(&dir).unwrap();
        // 核心 mod：落盘名与上报名不同 → 按 .mods-names.json 映射上报
        std::fs::write(
            dir.join("4681704866889354274@3@13105.jar"),
            b"core-mod-bytes",
        )
        .unwrap();
        std::fs::write(
            crate::launcher::netease::mod_names_path(&root),
            r#"{"4681704866889354274@3@13105.jar":{"modPath":"4681704866889354274@3@0.jar","id":"4681704866889354274@3@0.jar","iid":"4681704866889354274"}}"#,
        )
        .unwrap();
        // 组件 mod：没有映射 → 原样文件名、iid = @ 前段
        std::fs::write(dir.join("component@3@0.jar"), b"comp").unwrap();
        std::fs::write(dir.join("readme.txt"), b"skip me").unwrap();
        let v: serde_json::Value = serde_json::from_str(&mods_json(&root)).unwrap();
        let list = v["mods"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        let core = list
            .iter()
            .find(|m| m["iid"] == "4681704866889354274")
            .unwrap();
        assert_eq!(core["modPath"], "4681704866889354274@3@0.jar");
        assert_eq!(core["id"], "4681704866889354274@3@0.jar");
        // md5 必须大写（官方 Md5 = jar_md5.Upper()）
        assert_eq!(
            core["md5"],
            format!("{:X}", md5::Md5::digest(b"core-mod-bytes"))
        );
        let comp = list.iter().find(|m| m["iid"] == "component").unwrap();
        assert_eq!(comp["modPath"], "component@3@0.jar");
        assert_eq!(comp["md5"], format!("{:X}", md5::Md5::digest(b"comp")));
        // 空目录 → {"mods":[]}
        let empty = std::env::temp_dir().join(format!("mctl_mods_empty_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&empty);
        assert_eq!(mods_json(&empty), "{\"mods\":[]}");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&empty);
    }
}
