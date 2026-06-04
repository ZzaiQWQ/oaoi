/// 根据 fileId 构造 CurseForge 两个官方 CDN 地址。
pub fn cf_cdn_urls(file_id: u32, file_name: &str) -> Vec<String> {
    let id1 = file_id / 1000;
    let id2 = file_id % 1000;
    let encoded_name = urlencoding::encode(file_name);
    vec![
        format!(
            "https://mediafilez.forgecdn.net/files/{}/{}/{}",
            id1, id2, encoded_name
        ),
        format!(
            "https://edge.forgecdn.net/files/{}/{}/{}",
            id1, id2, encoded_name
        ),
    ]
}
