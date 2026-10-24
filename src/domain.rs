use serde::{Deserialize, Serialize};

/// 资源大厅分类
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    pub name: &'static str,
    pub item_type: i64,
    pub master_type_id: &'static str,
    pub secondary_type_id: &'static str,
}

pub const CATEGORY_SERVERS: Category = Category {
    name: "服务器",
    item_type: 1,
    master_type_id: "2",
    secondary_type_id: "",
};
pub const CATEGORY_MODS: Category = Category {
    name: "模组",
    item_type: 2,
    master_type_id: "3",
    secondary_type_id: "",
};
pub const CATEGORY_SHADERS: Category = Category {
    name: "光影材质",
    item_type: 2,
    master_type_id: "4",
    secondary_type_id: "",
};
pub const CATEGORY_MAPS_MINIGAME: Category = Category {
    name: "地图·小游戏",
    item_type: 2,
    master_type_id: "5",
    secondary_type_id: "",
};
pub const CATEGORY_MAPS_ADVENTURE: Category = Category {
    name: "地图·冒险",
    item_type: 2,
    master_type_id: "6",
    secondary_type_id: "",
};
pub const CATEGORY_SKINS: Category = Category {
    name: "皮肤",
    item_type: 2,
    master_type_id: "10",
    secondary_type_id: "",
};

pub const ALL_CATEGORIES: &[Category] = &[
    CATEGORY_SERVERS,
    CATEGORY_MODS,
    CATEGORY_SHADERS,
    CATEGORY_MAPS_MINIGAME,
    CATEGORY_MAPS_ADVENTURE,
    CATEGORY_SKINS,
];

/// 资源列表项（item/query/available）
#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
pub struct ShopItem {
    #[serde(default)]
    pub entity_id: String,
    #[serde(default)]
    pub item_type: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub item_version: String,
    #[serde(default)]
    pub developer_name: String,
    #[serde(default)]
    pub brief_summary: String,
    #[serde(default)]
    pub master_type_id: String,
    #[serde(default)]
    pub secondary_type_id: String,
    #[serde(default)]
    pub download_num: i64,
    #[serde(default)]
    pub like_num: i64,
    #[serde(default)]
    pub publish_time: i64,
    #[serde(default)]
    pub goods_state: i64,
    #[serde(default)]
    pub vip_only: bool,
    #[serde(default)]
    pub resource_version: i64,
    #[serde(default)]
    pub mod_id: i64,
}

/// 资源详情（item-details/get_v2）
#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
pub struct ItemDetail {
    #[serde(default)]
    pub entity_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub item_version: String,
    #[serde(default)]
    pub developer_name: String,
    #[serde(default)]
    pub brief_summary: String,
    #[serde(default)]
    pub detail_description: String,
    #[serde(default)]
    pub brief_image_urls: Vec<String>,
    #[serde(default)]
    pub download_num: i64,
    #[serde(default)]
    pub like_num: i64,
    #[serde(default)]
    pub network_tag: String,
}

/// 下载子资源（一个 MC 版本）
#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
pub struct DownloadSub {
    #[serde(default)]
    pub entity_id: String,
    #[serde(default)]
    pub mc_version_name: String,
    #[serde(default)]
    pub res_url: String,
    #[serde(default)]
    pub res_size: i64,
    #[serde(default)]
    pub res_md5: String,
    #[serde(default)]
    pub jar_md5: String,
    #[serde(default)]
    pub res_name: String,
    #[serde(default)]
    pub java_version: i64,
    #[serde(default)]
    pub res_version: i64,
}

/// 登录凭据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub user_id: String,
    pub user_token: String,
    pub aid: String,
    pub sdkuid: String,
    /// 登录响应里的 `sead`（服务端下发的 seed）。
    /// 先作为进服签名盐候选，待实网验证；旧凭据文件没有该字段。
    #[serde(default)]
    pub sead: Option<String>,
}
