"""
CurseForge Mod 数据采集工具
采集所有 Minecraft mod 的 slug -> name 映射
合并到 mod_raw.json，支持断点续采
用法: python scripts/build_cf_db.py
"""

import json, time, os, urllib.request, urllib.parse, traceback
from datetime import datetime

OUTPUT = os.path.join(os.path.dirname(__file__), "..", "src-tauri", "mod_raw.json")
PROGRESS_FILE = os.path.join(os.path.dirname(__file__), "..", "src-tauri", "cf_progress.json")
ERROR_LOG = os.path.join(os.path.dirname(__file__), "..", "src-tauri", "cf_errors.log")
CF_API_KEY = "$2a$10$4COp2ijFg3ZaE8BdSt6NrOfdFmlN8XagJlHePxIGEAj2Oq1GLE9q2"
PAGE_SIZE = 50
DELAY = 1.0  # 1秒一次，更安全
COOLDOWN_EVERY = 100   # 每100页(约5000个mod)休息
COOLDOWN_SECS = 120    # 休息2分钟
request_count = 0      # 全局请求计数

print("=== CurseForge Mod 采集工具 ===")
print(f"输出: {OUTPUT}")
print(f"进度: {PROGRESS_FILE}")
print(f"错误日志: {ERROR_LOG}\n")

# 加载已有数据（增量）
all_mods = {}
if os.path.exists(OUTPUT):
    with open(OUTPUT, "r", encoding="utf-8") as f:
        all_mods = json.load(f)
    print(f"已有 {len(all_mods)} 条, 增量模式")

# 加载已完成的采集任务（断点续采）
done_tasks = set()
if os.path.exists(PROGRESS_FILE):
    with open(PROGRESS_FILE, "r", encoding="utf-8") as f:
        done_tasks = set(json.load(f))
    print(f"已完成 {len(done_tasks)} 个采集任务, 跳过已完成的")

print()

def save():
    with open(OUTPUT, "w", encoding="utf-8") as f:
        json.dump(all_mods, f, ensure_ascii=False, indent=2)

def save_progress():
    with open(PROGRESS_FILE, "w", encoding="utf-8") as f:
        json.dump(list(done_tasks), f)

def log_error(task_name, msg):
    with open(ERROR_LOG, "a", encoding="utf-8") as f:
        f.write(f"[{datetime.now().strftime('%Y-%m-%d %H:%M:%S')}] {task_name}: {msg}\n")

def fetch_cf(index, sort_field=2, sort_order="desc", game_version="", category_id=0):
    url = f"https://api.curseforge.com/v1/mods/search?gameId=432&classId=6&pageSize={PAGE_SIZE}&index={index}&sortField={sort_field}&sortOrder={sort_order}"
    if game_version:
        url += f"&gameVersion={urllib.parse.quote(game_version)}"
    if category_id > 0:
        url += f"&categoryId={category_id}"
    req = urllib.request.Request(url, headers={
        "User-Agent": "OaoiLauncher/1.0",
        "x-api-key": CF_API_KEY,
        "Accept": "application/json"
    })
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read().decode("utf-8"))

def crawl(task_id, label, sort_field=2, game_version="", category_id=0):
    """采集一个维度，实时保存，断点续采"""
    # 已完成的跳过
    if task_id in done_tasks:
        print(f"\n--- {label} --- [已完成，跳过]")
        return
    
    print(f"\n--- {label} ---")
    index = 0
    total_new = 0
    error_count = 0
    no_new_count = 0
    
    while True:
        try:
            data = fetch_cf(index, sort_field, game_version=game_version, category_id=category_id)
            error_count = 0  # 成功后重置错误计数
        except urllib.error.HTTPError as e:
            error_count += 1
            msg = f"HTTP {e.code} at index={index}"
            print(f"  错误: {msg}")
            log_error(label, msg)
            if e.code == 403:
                print("  API key 无效或被封! 停止采集")
                log_error(label, "API key 被封，停止")
                save()
                return
            if error_count >= 3:
                print(f"  连续{error_count}次错误，跳过此维度")
                log_error(label, f"连续{error_count}次错误，跳过")
                break
            time.sleep(3)  # 出错后等久一点
            continue
        except Exception as e:
            error_count += 1
            msg = f"{type(e).__name__}: {e} at index={index}"
            print(f"  错误: {msg}")
            log_error(label, msg)
            if error_count >= 3:
                print(f"  连续{error_count}次错误，跳过此维度")
                log_error(label, f"连续{error_count}次错误，跳过")
                break
            time.sleep(3)
            continue
        
        mods = data.get("data", [])
        if not mods:
            break
        
        new = 0
        for m in mods:
            slug = m.get("slug", "")
            name = m.get("name", "")
            if slug and slug not in all_mods:
                all_mods[slug] = name
                new += 1
        total_new += new
        if new > 0:
            save()
            no_new_count = 0
        else:
            no_new_count += 1
        
        print(f"  index={index} | +{new} | 总计 {len(all_mods)}")
        
        if no_new_count >= 10:
            print(f"  连续{no_new_count}页无新增，跳过")
            break
        
        pagination = data.get("pagination", {})
        total_count = pagination.get("totalCount", 0)
        if index + PAGE_SIZE >= total_count:
            break
        
        index += PAGE_SIZE
        
        # 请求计数 + 定期休息
        global request_count
        request_count += 1
        if request_count % COOLDOWN_EVERY == 0:
            print(f"\n  === 已请求 {request_count} 次，休息 {COOLDOWN_SECS} 秒 ===\n")
            save()
            time.sleep(COOLDOWN_SECS)
        else:
            time.sleep(DELAY)
    
    print(f"  本轮新增 {total_new} | 总计 {len(all_mods)}")
    # 标记此任务完成
    done_tasks.add(task_id)
    save_progress()

# ===== 1. 按排序采集 =====
print("\n========== 阶段 1: 按排序采集 ==========")
for sort_name, sort_id in [("热度", 2), ("总下载", 5), ("最后更新", 3), ("名称", 1)]:
    crawl(f"sort_{sort_id}", f"排序: {sort_name}", sort_field=sort_id)

# ===== 2. 按分类采集 =====
print("\n========== 阶段 2: 按分类采集 ==========")
cf_categories = [
    (406, "世界生成"), (407, "生物"), (408, "API和库"),
    (409, "烹饪"), (410, "杂项"), (411, "盔甲武器工具"),
    (412, "魔法"), (414, "科技"), (415, "冒险RPG"),
    (417, "能源"), (419, "矿石资源"), (420, "服务器工具"),
    (421, "装饰"), (422, "信息显示"), (423, "红石"),
    (424, "基因"), (425, "存储"), (426, "农业"),
    (429, "地图信息"), (434, "食物"),
    (435, "建筑"), (436, "传送"),
]
for cat_id, cat_name in cf_categories:
    crawl(f"cat_{cat_id}", f"分类: {cat_name}", sort_field=2, category_id=cat_id)

# ===== 3. 按版本采集 =====
print("\n========== 阶段 3: 按版本采集 ==========")
versions = [
    "1.21.5", "1.21.4", "1.21.1", "1.21",
    "1.20.6", "1.20.4", "1.20.2", "1.20.1",
    "1.19.4", "1.19.2",
    "1.18.2", "1.17.1", "1.16.5",
    "1.15.2", "1.14.4", "1.12.2",
    "1.11.2", "1.10.2", "1.8.9", "1.7.10",
]
for ver in versions:
    crawl(f"ver_{ver}", f"版本: {ver}", sort_field=2, game_version=ver)

save()
print(f"\n=== 采集完成! 共 {len(all_mods)} 个 mod ===")
print(f"文件: {OUTPUT}")
if os.path.exists(ERROR_LOG):
    print(f"错误日志: {ERROR_LOG}")
