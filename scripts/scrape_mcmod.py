"""
MC百科 模组名称采集脚本
从列表页采集: 简写|中文名|英文名
HTML结构:
  div.modlist-block > .title > p.name > a (中文名含简写)
                              p.ename > a (英文名)
"""
import requests
from bs4 import BeautifulSoup
import re
import time
import os
import sys

OUTPUT_FILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "mcmod_data.txt")
PROGRESS_FILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "mcmod_progress.txt")
BASE_URL = "https://www.mcmod.cn/modlist.html?platform=1&page={}"
TOTAL_PAGES = 797

HEADERS = {
    "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
    "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    "Accept-Language": "zh-CN,zh;q=0.9,en;q=0.8",
    "Referer": "https://www.mcmod.cn/",
}

def get_start_page():
    if os.path.exists(PROGRESS_FILE):
        with open(PROGRESS_FILE, "r") as f:
            try:
                return int(f.read().strip())
            except:
                pass
    return 1

def save_progress(page):
    with open(PROGRESS_FILE, "w") as f:
        f.write(str(page))

def parse_page(html):
    """解析单页模组列表"""
    soup = BeautifulSoup(html, "html.parser")
    results = []

    for item in soup.select("div.modlist-block"):
        try:
            # 中文名(含简写): .title .name a
            cn_link = item.select_one(".title .name a")
            # 英文名: .title .ename a
            en_link = item.select_one(".title .ename a")

            cn_raw = cn_link.get_text(strip=True) if cn_link else ""
            en_text = en_link.get_text(strip=True) if en_link else ""

            # 提取简写和中文名
            abbr = ""
            cn_text = cn_raw
            abbr_match = re.match(r'\[([^\]]+)\]\s*', cn_raw)
            if abbr_match:
                abbr = abbr_match.group(1)
                cn_text = cn_raw[abbr_match.end():].strip()

            # 至少有一个名字就保存
            if cn_text or en_text:
                results.append((abbr, cn_text, en_text))
        except Exception:
            continue

    return results

def main():
    start_page = get_start_page()
    mode = "a" if start_page > 1 else "w"

    session = requests.Session()
    session.headers.update(HEADERS)

    total_mods = 0
    if mode == "a" and os.path.exists(OUTPUT_FILE):
        with open(OUTPUT_FILE, "r", encoding="utf-8") as f:
            total_mods = sum(1 for _ in f)

    print(f"MC百科模组数据采集")
    print(f"起始页: {start_page}/{TOTAL_PAGES}, 已有: {total_mods} 条")
    print(f"输出: {OUTPUT_FILE}")
    print("-" * 50)

    with open(OUTPUT_FILE, mode, encoding="utf-8") as f:
        for page in range(start_page, TOTAL_PAGES + 1):
            url = BASE_URL.format(page)

            for retry in range(3):
                try:
                    resp = session.get(url, timeout=15)
                    if resp.status_code == 429:
                        wait = 15 * (retry + 1)
                        print(f"\n  [429] 限流，等 {wait}s...")
                        time.sleep(wait)
                        continue
                    resp.raise_for_status()
                    break
                except Exception as e:
                    if retry == 2:
                        print(f"\n  [ERR] 页{page}失败: {e}")
                    time.sleep(5)
            else:
                save_progress(page)
                continue

            mods = parse_page(resp.text)
            for abbr, cn, en in mods:
                f.write(f"{abbr}|{cn}|{en}\n")
                total_mods += 1
            f.flush()
            save_progress(page + 1)

            pct = page / TOTAL_PAGES * 100
            sys.stdout.write(f"\r[{pct:5.1f}%] 页{page}/{TOTAL_PAGES} 本页{len(mods)}条 总{total_mods}条")
            sys.stdout.flush()

            # 每100页多休息
            if page % 100 == 0:
                print(f"\n  休息10s...")
                time.sleep(10)
            else:
                time.sleep(3)  # 3秒间隔

    print(f"\n\n完成！共 {total_mods} 条")
    if os.path.exists(PROGRESS_FILE):
        os.remove(PROGRESS_FILE)

if __name__ == "__main__":
    main()
