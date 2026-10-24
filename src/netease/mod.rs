/// 探测当前网络运营商线路（租赁服三网线路选择）
pub fn detect_isp() -> String {
    // 尝试读取 /sys/class/net/*/address 前缀或路由表判断，简化：默认 "cmcc"
    "cmcc".to_string()
}

