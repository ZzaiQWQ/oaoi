import json, urllib.request, urllib.parse, time, os

BASE_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), '..'))

def load_dict():
    d = {}
    dict_path = os.path.join(BASE_DIR, 'scripts', 'mcmod_dict.txt')
    if os.path.exists(dict_path):
        with open(dict_path, 'r', encoding='utf-8') as f:
            for line in f:
                if '=' in line:
                    k, v = line.strip().split('=', 1)
                    d[k.strip().lower()] = v.strip()
    return d

MC_DICT = load_dict()

def exact_match(slug, title):
    slug_lower = slug.lower().strip()
    if slug_lower in MC_DICT:
        return MC_DICT[slug_lower]
    return None

def apply_post_translation_fixes(original_title, translated):
    # Convert obvious machine translation errors using common terms
    orig_lower = original_title.lower()
    
    if orig_lower.startswith('create ') or orig_lower.startswith('create:') or orig_lower.startswith('create -'):
        translated = translated.replace('创建', '机械动力').replace('创造', '机械动力')
        
    if 'tinkers' in orig_lower or "tinker's" in orig_lower:
        translated = translated.replace('修补匠', '匠魂').replace('工匠', '匠魂')
        
    if 'delight' in orig_lower:
        translated = translated.replace('喜悦', '乐事').replace('的喜悦', '乐事')
        
    if 'macaw' in orig_lower:
        translated = translated.replace('金刚鹦鹉', '金刚鹦鹉')
        
    if 'yung' in orig_lower:
        translated = translated.replace('荣格', 'YUNG')
        
    if 'cobblemon' in orig_lower:
        translated = translated.replace('鹅卵石怪物', '宝可梦').replace('鹅卵石兽', '宝可梦').replace('鹅卵石', '宝可梦')
        
    if 'compat' in orig_lower:
        translated = translated.replace('紧凑', '兼容').replace('的兼容', '兼容')

    if 'reforged' in orig_lower:
        translated = translated.replace('重铸', '重置版').replace('的重铸', '重置版')
        
    return translated

def translate_batch(slugs, titles):
    results = [exact_match(slugs[i], titles[i]) for i in range(len(slugs))]
    need_translation_indices = [i for i, r in enumerate(results) if r is None]
    
    if not need_translation_indices:
        return results
        
    texts_to_translate = [titles[i] for i in need_translation_indices]
    combined = '\n'.join(texts_to_translate)
    url = f"https://translate.googleapis.com/translate_a/single?client=gtx&sl=en&tl=zh-CN&dt=t&q={urllib.parse.quote(combined)}"
    req = urllib.request.Request(url, headers={
        'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36'
    })
    
    api_res = []
    for attempt in range(5):
        try:
            resp = urllib.request.urlopen(req, timeout=15)
            data = json.loads(resp.read().decode('utf-8'))
            result = ''.join(i[0] for i in data[0] if i[0])
            api_res = [s.strip() for s in result.split('\n')]
            break
        except Exception as e:
            print(f"    [警告] 翻译接口异常 ({e})，正在重试 {attempt+1}/5...")
            time.sleep(10)
    else:
        raise Exception("多次重试失败，已停止脚本防污染数据！")
        
    for idx, orig_index in enumerate(need_translation_indices):
        translated_text = api_res[idx] if idx < len(api_res) else titles[orig_index]
        fixed_text = apply_post_translation_fixes(titles[orig_index], translated_text)
        results[orig_index] = fixed_text
        
    return results

raw_file = os.path.join(BASE_DIR, 'src-tauri', 'mod_raw.json')
cn_file = os.path.join(BASE_DIR, 'src-tauri', 'mod_cn.json')

def auto_run():
    print("====== 自动化汉化进程 (带本地精准词典版) 已启动 ======")
    print(f"已加载 {len(MC_DICT)} 条人工精修词汇！")
    
    while True:
        with open(raw_file, 'r', encoding='utf-8') as f: raw = json.load(f)
        try:
            with open(cn_file, 'r', encoding='utf-8') as f: cn = json.load(f)
        except:
            cn = {}

        missing = [k for k in raw.keys() if k not in cn]
        if not missing:
            print("\n[完美收工] 65,000+ 个模组已经全部翻译完毕！")
            break
            
        print(f"\n[实时进度] 目前完成: {len(cn)}/{len(raw)}。剩余待跑: {len(missing)}")
        
        batch_keys = missing[:50]
        batch_titles = [raw[k] for k in batch_keys]
        
        print(f"  -> 正在飞速翻译下 50 条 (例如首个: {batch_titles[0]})...")
        translated = translate_batch(batch_keys, batch_titles)
        
        if len(translated) < len(batch_titles):
            translated.extend(batch_titles[len(translated):])
        elif len(translated) > len(batch_titles):
            translated = translated[:len(batch_titles)]
            
        for i, k in enumerate(batch_keys):
            cn[k] = translated[i]
            
        with open(cn_file, 'w', encoding='utf-8') as f:
            json.dump(cn, f, ensure_ascii=False, indent=2)
            
        print(f"  -> 本批次写入成功！安全休眠 6 秒避免封禁 IP...")
        time.sleep(6)

if __name__ == '__main__':
    auto_run()
