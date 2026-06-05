pub mod fabric;
pub mod forge;
pub mod neoforge;
pub mod quilt;

// 只用于加载器版本列表的静态下限判断，不修改实例 JSON。
pub fn release_version_at_least(mc_version: &str, min_minor: u32, min_patch: u32) -> bool {
    let mut parts = mc_version.split('.');
    let major = parts.next().and_then(|value| value.parse::<u32>().ok());
    let minor = parts.next().and_then(|value| value.parse::<u32>().ok());
    let patch = parts
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    let (Some(major), Some(minor)) = (major, minor) else {
        return false;
    };
    if major != 1 {
        return major > 1;
    }
    minor > min_minor || (minor == min_minor && patch >= min_patch)
}
