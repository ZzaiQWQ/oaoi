// ============ 实例详情页逻辑 ============

let currentDetailInstance = null;
let currentDetailInfo = null; // { mc_version, loader_type }
let currentModList = [];
const modUrlCache = {}; // 缓存已查过的 mod 链接

// 打开实例详情页
function showInstanceDetail(instanceName) {
  const instance = instancesCache.find(v => v.name === instanceName);
  if (!instance) return;

  currentDetailInstance = instanceName;
  currentDetailInfo = instance;

  // 填充信息
  document.getElementById('instanceDetailName').textContent = instance.name;
  document.getElementById('instanceDetailMcVer').textContent = instance.mc_version;
  document.getElementById('instanceDetailLoader').textContent =
    instance.loader_type === 'vanilla' ? '原版' :
    instance.loader_type.charAt(0).toUpperCase() + instance.loader_type.slice(1);
  document.getElementById('instanceDetailLoaderVer').textContent =
    instance.loader_version || '-';

  // 切换到详情页
  const pages = document.querySelectorAll('.page');
  const navItems = document.querySelectorAll('.nav-item');
  pages.forEach(p => p.classList.remove('active'));
  navItems.forEach(n => n.classList.remove('active'));
  document.getElementById('pageInstanceDetail').classList.add('active');

  // 重置 tab 到已安装
  switchModTab('installed');

  // 清空搜索
  const search = document.getElementById('modSearchInput');
  if (search) search.value = '';
  const onlineSearch = document.getElementById('onlineModSearch');
  if (onlineSearch) onlineSearch.value = '';

  // 切换实例后刷新在线列表（用缓存或清空）
  const onlineList = document.getElementById('onlineModList');
  if (onlineList) {
    const cacheKey = `${instance.mc_version||''}:${instance.loader_type||''}:${currentOnlineCategory}:`;
    if (onlineSearchCache[cacheKey]) {
      renderOnlineResults(onlineSearchCache[cacheKey], '');
    } else {
      const typeLabel = { mod: 'Mod', resourcepack: '材质包', shader: '光影包' }[currentOnlineCategory] || 'Mod';
      onlineList.innerHTML = `<div class="mod-list-empty">输入关键词搜索 Modrinth + CurseForge 上的 ${typeLabel}</div>`;
    }
  }

  loadModList();
}

// Tab 切换
function switchModTab(tab) {
  document.querySelectorAll('.mod-tab').forEach(t => t.classList.toggle('active', t.dataset.tab === tab));
  document.getElementById('modTabInstalledContent')?.classList.toggle('active', tab === 'installed');
  document.getElementById('modTabOnlineContent')?.classList.toggle('active', tab === 'online');
  // 显示/隐藏类别按钮
  document.getElementById('onlineCategoryTabs')?.classList.toggle('visible', tab === 'online');
  // 切到在线搜索时自动加载热门
  if (tab === 'online') {
    const listEl = document.getElementById('onlineModList');
    const cacheKey = `${currentDetailInfo?.mc_version||''}:${currentDetailInfo?.loader_type||''}:${currentOnlineCategory}:`;
    if (listEl && listEl.querySelector('.mod-list-empty') && !onlineSearchCache[cacheKey]) {
      searchOnlineMods();
    }
  }
}

// 加载已安装 mod 列表
async function loadModList() {
  if (!currentDetailInstance) return;
  const listEl = document.getElementById('modList');
  const countEl = document.getElementById('modCount');
  try {
    const tauri = await waitForTauri();
    const gameDir = localStorage.getItem('gameDir') || '';
    const mods = await tauri.core.invoke('list_mods', { gameDir, name: currentDetailInstance });
    currentModList = mods;
    if (countEl) countEl.textContent = mods.length;
    renderModList(mods);
  } catch (err) {
    console.warn('加载 Mod 列表失败:', err);
    if (listEl) listEl.innerHTML = '<div class="mod-list-empty">加载失败</div>';
  }
}

// 渲染已安装 mod 列表
function renderModList(mods) {
  const listEl = document.getElementById('modList');
  if (!listEl) return;

  const searchVal = (document.getElementById('modSearchInput')?.value || '').toLowerCase();
  const filtered = searchVal
    ? mods.filter(m => m.file_name.toLowerCase().includes(searchVal) || (m.cn_name && m.cn_name.toLowerCase().includes(searchVal)))
    : mods;

  if (filtered.length === 0) {
    listEl.innerHTML = `<div class="mod-list-empty">${mods.length === 0 ? '暂无 Mod' : '无匹配结果'}</div>`;
    return;
  }

  listEl.innerHTML = filtered.map(mod => {
    const baseName = mod.file_name.replace(/\.jar\.disabled$/i, '').replace(/\.jar$/i, '');
    const displayName = mod.cn_name ? `${mod.cn_name} (${baseName})` : baseName;
    return `
      <div class="mod-item ${mod.enabled ? '' : 'disabled'}" data-file="${mod.file_name}">
        <button class="mod-toggle ${mod.enabled ? 'active' : ''}" data-file="${mod.file_name}" title="${mod.enabled ? '点击禁用' : '点击启用'}"></button>
        <span class="mod-name" title="${mod.file_name}">${displayName}</span>
        <span class="mod-actions" id="mod-actions-${mod.file_name.replace(/[^a-zA-Z0-9]/g, '_')}">
          <button class="mod-delete-btn" data-file="${mod.file_name}" title="删除">🗑</button>
        </span>
        <span class="mod-size">${mod.size_kb > 1024 ? (mod.size_kb / 1024).toFixed(1) + ' MB' : mod.size_kb + ' KB'}</span>
      </div>
    `;
  }).join('');

  // 开关事件
  listEl.querySelectorAll('.mod-toggle').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      try {
        const tauri = await waitForTauri();
        const gameDir = localStorage.getItem('gameDir') || '';
        await tauri.core.invoke('toggle_mod', { gameDir, name: currentDetailInstance, fileName: btn.dataset.file });
        await loadModList();
      } catch (err) {
        console.warn('切换 Mod 状态失败:', err);
      }
    });
  });

  // 删除事件（二次点击确认）
  listEl.querySelectorAll('.mod-delete-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      // 第一次点击：变成确认状态
      if (!btn.dataset.confirming) {
        btn.dataset.confirming = 'true';
        btn.textContent = '确认?';
        btn.classList.add('confirming');
        // 2秒后自动恢复
        btn.dataset.timerId = String(setTimeout(() => {
          delete btn.dataset.confirming;
          btn.textContent = '🗑';
          btn.classList.remove('confirming');
        }, 2000));
        return;
      }
      // 第二次点击：真正删除
      clearTimeout(parseInt(btn.dataset.timerId));
      try {
        const tauri = await waitForTauri();
        const gameDir = localStorage.getItem('gameDir') || '';
        await tauri.core.invoke('delete_mod', { gameDir, name: currentDetailInstance, fileName: btn.dataset.file });
        await loadModList();
      } catch (err) {
        console.warn('删除 Mod 失败:', err);
      }
    });
  });

  // 异步查询真实链接（带缓存）
  const allFileNames = filtered.map(m => m.file_name);
  // 先用缓存填充已有的
  for (const fn of allFileNames) {
    if (modUrlCache[fn]) {
      _applyModUrls(modUrlCache[fn]);
    }
  }
  const uncached = allFileNames.filter(fn => !modUrlCache[fn]);
  if (uncached.length > 0) {
    (async () => {
      try {
        const tauri = await waitForTauri();
        const urls = await tauri.core.invoke('lookup_mod_urls', { fileNames: uncached });
        for (const info of urls) {
          modUrlCache[info.file_name] = info;
          _applyModUrls(info);
        }
      } catch (err) {
        console.warn('查询 Mod 链接失败:', err);
      }
    })();
  }
}

function _applyModUrls(info) {
  const safeId = info.file_name.replace(/[^a-zA-Z0-9]/g, '_');
  const actionsEl = document.getElementById('mod-actions-' + safeId);
  if (!actionsEl || actionsEl.querySelector('.mod-link')) return; // 已有链接则跳过

  const deleteBtn = actionsEl.querySelector('.mod-delete-btn');
  let linksHtml = '';
  if (info.mr_url) linksHtml += `<a href="#" class="mod-link mr" data-url="${info.mr_url}" title="Modrinth">MR</a>`;
  if (info.cf_url) linksHtml += `<a href="#" class="mod-link cf" data-url="${info.cf_url}" title="CurseForge">CF</a>`;

  if (linksHtml && deleteBtn) {
    deleteBtn.insertAdjacentHTML('beforebegin', linksHtml);
  }

  actionsEl.querySelectorAll('.mod-link').forEach(link => {
    link.addEventListener('click', async (e) => {
      e.preventDefault();
      e.stopPropagation();
      await openExternalUrl(link.dataset.url);
    });
  });
}

let currentOnlineCategory = 'mod';

// 持久化缓存（localStorage，3天过期，新的覆盖旧的）
const CACHE_KEY = 'onlineSearchCache';
const CACHE_TTL = 3 * 24 * 60 * 60 * 1000; // 3天

function _loadCache() {
  try {
    const raw = localStorage.getItem(CACHE_KEY);
    if (!raw) return {};
    const data = JSON.parse(raw);
    // 清理过期条目
    const now = Date.now();
    for (const k of Object.keys(data)) {
      if (data[k]?._ts && now - data[k]._ts > CACHE_TTL) {
        delete data[k];
      }
    }
    return data || {};
  } catch { return {}; }
}

function _saveCache(cache) {
  try {
    // 给每条记录打上时间戳
    const now = Date.now();
    for (const k of Object.keys(cache)) {
      if (!cache[k]?._ts) cache[k] = { ...cache[k], _ts: now };
    }
    // 超 150 条 → 按时间排序，删最旧的
    const keys = Object.keys(cache);
    if (keys.length > 150) {
      keys.sort((a, b) => (cache[a]?._ts || 0) - (cache[b]?._ts || 0));
      keys.slice(0, keys.length - 150).forEach(k => delete cache[k]);
    }
    const json = JSON.stringify(cache);
    // 超 3MB → 再删一半最旧的
    if (json.length > 3 * 1024 * 1024) {
      const sorted = Object.keys(cache).sort((a, b) => (cache[a]?._ts || 0) - (cache[b]?._ts || 0));
      sorted.slice(0, Math.floor(sorted.length / 2)).forEach(k => delete cache[k]);
    }
    localStorage.setItem(CACHE_KEY, JSON.stringify(cache));
  } catch (e) {
    console.warn('[cache] 缓存写入失败:', e);
  }
}

function clearInstanceCache(mcVersion, loader) {
  const prefix = `${mcVersion || ''}:${loader || ''}:`;
  let changed = false;
  for (const key of Object.keys(onlineSearchCache)) {
    if (key.startsWith(prefix)) {
      delete onlineSearchCache[key];
      changed = true;
    }
  }
  if (changed) _saveCache(onlineSearchCache);
}

const onlineSearchCache = _loadCache();

// 类别切换（作用域委托，避免全局捕获）
const onlineCatContainer = document.getElementById('onlineCategoryTabs');
if (onlineCatContainer) {
  onlineCatContainer.addEventListener('click', (e) => {
    const btn = e.target.closest('.online-cat-btn');
    if (!btn) return;
    onlineCatContainer.querySelectorAll('.online-cat-btn').forEach(b => b.classList.remove('active'));
    btn.classList.add('active');
    currentOnlineCategory = btn.dataset.type;
    const searchInput = document.getElementById('onlineModSearch');
    const typeLabel = { mod: 'Mod', resourcepack: '材质包', shader: '光影包' }[currentOnlineCategory];
    if (searchInput) {
      searchInput.placeholder = `搜索 ${typeLabel}...`;
      searchInput.value = '';
    }
    // 自动加载热门列表
    searchOnlineMods();
  });
}

// 在线搜索
async function searchOnlineMods() {
  const query = document.getElementById('onlineModSearch')?.value?.trim() || '';

  const listEl = document.getElementById('onlineModList');
  if (!listEl) return;

  // 检查缓存
  const cacheKey = `${currentDetailInfo?.mc_version||''}:${currentDetailInfo?.loader_type||''}:${currentOnlineCategory}:${query}`;
  if (onlineSearchCache[cacheKey]) {
    renderOnlineResults(onlineSearchCache[cacheKey], query);
    return;
  }

  listEl.innerHTML = '<div class="mod-list-empty">搜索中...</div>';

  try {
    const tauri = await waitForTauri();
    const mcVersion = currentDetailInfo?.mc_version || '';
    const loader = currentDetailInfo?.loader_type || '';
    const projectType = currentOnlineCategory;
    const results = await tauri.core.invoke('search_online_mods', { query, mcVersion, loader, projectType });
    onlineSearchCache[cacheKey] = results;
    _saveCache(onlineSearchCache);
    renderOnlineResults(results, query);
  } catch (err) {
    listEl.innerHTML = `<div class="mod-list-empty">搜索失败: ${err}</div>`;
  }
}

function renderOnlineResults(results, query) {
  const listEl = document.getElementById('onlineModList');
  if (!listEl) return;

  if (results.length === 0) {
    const isChinese = /[\u4e00-\u9fa5]/.test(query);
    listEl.innerHTML = `<div class="mod-list-empty">
      未找到匹配的结果${isChinese ? '<br><span style="font-size:11px;margin-top:4px;display:inline-block;">中文搜索推荐 <a href="#" class="mcmod-search-link" style="color:var(--pink-700);font-weight:600;">在MC百科搜索「' + query + '」</a></span>' : ''}
    </div>`;
    if (isChinese) {
      listEl.querySelector('.mcmod-search-link')?.addEventListener('click', (e) => {
        e.preventDefault();
        window.open(`https://search.mcmod.cn/s?key=${encodeURIComponent(query)}&filter=1`, '_blank');
      });
    }
    return;
  }

  // escapeHtml 使用全局 utils.js

  listEl.innerHTML = results.map(mod => {
    const dlCount = formatDownloads(mod.downloads);
    const src = getSourceInfo(mod);
    const sourceLabel = src.label;
    const sourceClass = src.cssClass;
    const hasMR = src.hasMR;
    const hasCF = src.hasCF;
    const displayTitle = mod.cn_title ? `${mod.cn_title} (${mod.title})` : mod.title;
    return `
      <div class="online-mod-card">
        <img class="online-mod-icon" src="${mod.icon_url || ''}" alt="" onerror="this.style.display='none'">
        <div class="online-mod-info">
          <div class="online-mod-title" title="${escapeHtml(displayTitle)}">
            <span class="mod-source-tag ${sourceClass}">${sourceLabel}</span> ${escapeHtml(displayTitle)}
          </div>
          <div class="online-mod-desc" title="${escapeHtml(mod.description)}">${escapeHtml(mod.description)}</div>
          <div class="online-mod-meta">${escapeHtml(mod.author || '')} · ${dlCount} 下载</div>
        </div>
        <div class="online-mod-actions">
          <div class="mod-link-row">
            ${hasMR ? `<button class="mod-link-btn mr" data-url="${mod.mr_url}" title="在 Modrinth 查看">MR</button>` : ''}
            ${hasCF ? `<button class="mod-link-btn cf" data-url="${mod.cf_url}" title="在 CurseForge 查看">CF</button>` : ''}
          </div>
          <button class="online-mod-dl-btn" data-project="${mod.project_id}" data-title="${mod.title}">下载</button>
        </div>
      </div>
    `;
  }).join('');

  // 绑定打开链接事件
  listEl.querySelectorAll('.mod-link-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      e.preventDefault();
      await openExternalUrl(btn.dataset.url);
    });
  });

  // 绑定下载事件
  listEl.querySelectorAll('.online-mod-dl-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      if (btn.disabled) return;
      btn.disabled = true;
      btn.textContent = '下载中...';

      try {
        const tauri = await waitForTauri();
        const gameDir = localStorage.getItem('gameDir') || '';
        const result = await tauri.core.invoke('download_online_mod', {
          gameDir,
          name: currentDetailInstance,
          projectId: btn.dataset.project,
          mcVersion: currentDetailInfo?.mc_version || '',
          loader: currentDetailInfo?.loader_type || '',
          projectType: currentOnlineCategory,
        });
        btn.textContent = '已下载';
        btn.classList.add('done');
        // 刷新已安装列表
        loadModList();
      } catch (err) {
        const errMsg = String(err);
        if (errMsg.includes('没有找到') || errMsg.includes('无可用')) {
          btn.textContent = '无此版本';
        } else {
          btn.textContent = '失败';
        }
        btn.disabled = false;
        setTimeout(() => { btn.textContent = '下载'; }, 3000);
        console.warn('下载 Mod 失败:', err);
      }
    });
  });
}

// 初始化
function initInstanceDetailPage() {
  const backBtn = document.getElementById('instanceBackBtn');
  if (backBtn) {
    backBtn.addEventListener('click', () => {
      const pages = document.querySelectorAll('.page');
      const navItems = document.querySelectorAll('.nav-item');
      pages.forEach(p => p.classList.remove('active'));
      navItems.forEach(n => n.classList.remove('active'));
      document.getElementById('pageDownload').classList.add('active');
      const dlNav = document.querySelector('[data-page="download"]');
      if (dlNav) dlNav.classList.add('active');
      currentDetailInstance = null;
      currentDetailInfo = null;
      currentModList = [];
    });
  }

  // 文件夹按钮
  document.querySelectorAll('.instance-folder-btn').forEach(btn => {
    btn.addEventListener('click', async () => {
      if (!currentDetailInstance) return;
      try {
        const tauri = await waitForTauri();
        const gameDir = localStorage.getItem('gameDir') || '';
        await tauri.core.invoke('open_folder', { gameDir, name: currentDetailInstance, subDir: btn.dataset.sub });
      } catch (err) {
        console.warn('打开目录失败:', err);
      }
    });
  });

  // Tab 切换
  document.querySelectorAll('.mod-tab').forEach(tab => {
    tab.addEventListener('click', () => switchModTab(tab.dataset.tab));
  });

  // 已安装搜索
  document.getElementById('modSearchInput')?.addEventListener('input', () => renderModList(currentModList));
  document.getElementById('modRefreshBtn')?.addEventListener('click', () => loadModList());

  // 在线搜索
  document.getElementById('onlineModSearchBtn')?.addEventListener('click', searchOnlineMods);
  document.getElementById('onlineModSearch')?.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') searchOnlineMods();
  });

  // 启动按钮
  document.getElementById('instanceLaunchBtn')?.addEventListener('click', () => {
    if (!currentDetailInstance) return;
    const sel = document.getElementById('versionSelector');
    if (sel) {
      sel.value = currentDetailInstance;
      localStorage.setItem('selectedVersion', currentDetailInstance);
      syncVersionDropdown(instancesCache, currentDetailInstance);
    }
    const pages = document.querySelectorAll('.page');
    const navItems = document.querySelectorAll('.nav-item');
    pages.forEach(p => p.classList.remove('active'));
    navItems.forEach(n => n.classList.remove('active'));
    document.getElementById('pageHome').classList.add('active');
    document.querySelector('[data-page="home"]')?.classList.add('active');
    setTimeout(() => document.getElementById('launchBtn')?.click(), 300);
  });

  // 删除按钮
  document.getElementById('instanceDeleteBtn')?.addEventListener('click', async () => {
    if (!currentDetailInstance) return;
    try {
      const gameDir = localStorage.getItem('gameDir') || '';
      const confirmed = await showConfirm(
        `确定删除实例 ${currentDetailInstance} 吗？此操作不可恢复。`,
        { title: '删除确认', kind: 'danger' }
      );
      if (!confirmed) return;
      const tauri = await waitForTauri();
      await tauri.core.invoke('delete_version', { gameDir, name: currentDetailInstance });
      // 只有同版本同loader的最后一个实例删除时才清缓存
      const mcV = currentDetailInfo?.mc_version;
      const ldr = currentDetailInfo?.loader_type;
      const othersSame = (typeof instancesCache !== 'undefined' ? instancesCache : [])
        .filter(v => v.name !== currentDetailInstance && v.mc_version === mcV && v.loader_type === ldr);
      if (othersSame.length === 0) {
        clearInstanceCache(mcV, ldr);
      }
      document.getElementById('instanceBackBtn')?.click();
      loadInstalledVersions();
    } catch (err) {
      showToast('删除失败: ' + err, 'error');
    }
  });
}
