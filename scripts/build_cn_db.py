"""
Mod 数据采集工具（纯采集，不翻译）
多策略采集: 排序 + 版本 + 加载器 + 分类，实时保存
生成 mod_raw.json: { slug: title, ... }
用法: python scripts/build_cn_db.py
"""

import json, time, os, urllib.request, urllib.parse

OUTPUT = os.path.join(os.path.dirname(__file__), "..", "src-tauri", "mod_raw.json")
PAGE_SIZE = 100
MAX_OFFSET = 10000

print("=== Mod 数据采集工具 ===")
print(f"输出: {OUTPUT}\n")

# 加载已有数据（增量）
all_mods = {}
if os.path.exists(OUTPUT):
    with open(OUTPUT, "r", encoding="utf-8") as f:
        all_mods = json.load(f)
    print(f"已有 {len(all_mods)} 条, 增量模式\n")

def save():
    with open(OUTPUT, "w", encoding="utf-8") as f:
        json.dump(all_mods, f, ensure_ascii=False, indent=2)

def fetch(facets_str, sort_by, offset):
    facets = urllib.parse.quote(facets_str)
    url = f"https://api.modrinth.com/v2/search?facets={facets}&limit={PAGE_SIZE}&offset={offset}&index={sort_by}"
    req = urllib.request.Request(url, headers={"User-Agent": "OaoiLauncher/1.0"})
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read().decode("utf-8"))

def crawl(label, facets_str, sort_by):
    """采集一个维度，每页实时保存，连续3页无新增自动跳过"""
    print(f"\n--- {label} (排序:{sort_by}) ---")
    offset = 0
    total_new = 0
    no_new_count = 0  # 连续无新增计数
    while offset < MAX_OFFSET:
        try:
            data = fetch(facets_str, sort_by, offset)
        except Exception as e:
            print(f"  错误 offset={offset}: {e}")
            break
        hits = data.get("hits", [])
        if not hits:
            break
        new = 0
        for h in hits:
            slug = h["slug"]
            if slug not in all_mods:
                all_mods[slug] = h["title"]
                new += 1
        total_new += new
        # 有新数据就实时保存
        if new > 0:
            save()
            no_new_count = 0
        else:
            no_new_count += 1
        print(f"  offset={offset} | +{new} | 总计 {len(all_mods)}")
        # 连续3页无新增，跳过剩余页
        if no_new_count >= 5:
            print(f"  连续{no_new_count}页无新增，跳过")
            break
        offset += PAGE_SIZE
        time.sleep(0.15)
    print(f"  本轮新增 {total_new} | 总计 {len(all_mods)}")

# ===== 1. 按排序方式采集（最新发布的优先，下载量少的先采） =====
print("\n========== 阶段 1: 按排序采集 ==========")
for sort in ["newest", "updated", "follows", "downloads"]:
    crawl("全部mod", '[["project_type:mod"]]', sort)

# ===== 2. 按加载器采集 =====
print("\n========== 阶段 2: 按加载器采集 ==========")
for loader in ["forge", "fabric", "neoforge", "quilt"]:
    facets = f'[["project_type:mod"],["categories:{loader}"]]'
    crawl(f"加载器 {loader}", facets, "downloads")

# ===== 3. 按分类采集 =====
print("\n========== 阶段 3: 按分类采集 ==========")
categories = [
    "adventure", "cursed", "decoration", "economy", "equipment",
    "food", "game-mechanics", "library", "magic", "management",
    "minigame", "mobs", "optimization", "social", "storage",
    "technology", "transportation", "utility", "worldgen",
]
for cat in categories:
    facets = f'[["project_type:mod"],["categories:{cat}"]]'
    crawl(f"分类 {cat}", facets, "downloads")

# ===== 4. 按版本采集（最全面，放最后）=====
print("\n========== 阶段 4: 按版本采集 ==========")
versions = [
    "1.21.5", "1.21.4", "1.21.3", "1.21.2", "1.21.1", "1.21",
    "1.20.6", "1.20.4", "1.20.2", "1.20.1", "1.20",
    "1.19.4", "1.19.3", "1.19.2", "1.19.1", "1.19",
    "1.18.2", "1.18.1", "1.18",
    "1.17.1", "1.17",
    "1.16.5", "1.16.4", "1.16.3", "1.16.2", "1.16.1",
    "1.15.2", "1.15.1",
    "1.14.4", "1.14.3", "1.14.2",
    "1.13.2",
    "1.12.2", "1.12.1", "1.12",
    "1.11.2", "1.10.2",
    "1.9.4", "1.8.9", "1.7.10",
]
for ver in versions:
    facets = f'[["project_type:mod"],["versions:{ver}"]]'
    crawl(f"版本 {ver}", facets, "downloads")

save()
print(f"\n=== 采集完成! 共 {len(all_mods)} 个 mod ===")
print(f"文件: {OUTPUT}")
