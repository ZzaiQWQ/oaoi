pub mod fabric;
pub mod forge;
pub mod neoforge;
pub mod quilt;

// 只用于加载器版本列表的静态下限判断，不修改实例 JSON。
// 预发布/RC 版本要按前面的数字版本判断，避免新版本因为后缀被误判成不支持。
pub fn release_version_at_least(mc_version: &str, min_minor: u32, min_patch: u32) -> bool {
    if is_modern_snapshot(mc_version) {
        return true;
    }

    let mut parts = mc_version.split('.');
    let major = parts.next().and_then(parse_leading_number);
    let minor = parts.next().and_then(parse_leading_number);
    let patch = parts.next().and_then(parse_leading_number).unwrap_or(0);
    let (Some(major), Some(minor)) = (major, minor) else {
        return false;
    };
    if major != 1 {
        return major > 1;
    }
    minor > min_minor || (minor == min_minor && patch >= min_patch)
}

fn parse_leading_number(value: &str) -> Option<u32> {
    let digits: String = value
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse::<u32>().ok()
    }
}

fn is_modern_snapshot(mc_version: &str) -> bool {
    let chars: Vec<char> = mc_version.trim().chars().collect();
    chars.len() == 6
        && chars[0].is_ascii_digit()
        && chars[1].is_ascii_digit()
        && chars[2] == 'w'
        && chars[3].is_ascii_digit()
        && chars[4].is_ascii_digit()
        && chars[5].is_ascii_lowercase()
}
