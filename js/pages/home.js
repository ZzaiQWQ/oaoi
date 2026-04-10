// ============ 主页逻辑 ============

// ===== Minecraft 官方资讯拉取 =====
const MC_NEWS_API = 'https://launchercontent.mojang.com/v2/javaPatchNotes.json';
const MC_IMG_BASE = 'https://launchercontent.mojang.com';

(async function loadNews() {
  const container = document.getElementById('newsCards');
  if (!container) return;

  // 内置默认资讯（加载失败时显示）
  const DEFAULT_NEWS = [
    { title: '欢迎使用 oaoi 启动器！', desc: '全新樱花主题，畅享 Minecraft 之旅。', img: 'assets/news1.png', link: '' },
    { title: '支持一键安装整合包', desc: 'Modrinth / CurseForge 整合包拖拽即装。', img: 'assets/news2.png', link: '' },
  ];

  function renderNewsCards(items) {
    container.innerHTML = items.map(n => `
      <div class="news-card" ${n.link ? `data-link="${escapeHtml(n.link)}"` : ''} style="cursor:pointer;">
        <div class="news-card-img"><img src="${escapeHtml(n.img)}" alt="${escapeHtml(n.title)}" onerror="this.src='assets/news1.png'"></div>
        <div class="news-card-content">
          <h3>${escapeHtml(n.title)}</h3>
          <p>${escapeHtml(n.desc)}</p>
        </div>
      </div>`).join('');
    // 安全绑定点击事件（替代内联 onclick，防止 XSS）
    container.querySelectorAll('.news-card[data-link]').forEach(card => {
      card.addEventListener('click', () => openExternalUrl(card.dataset.link));
    });
  }

  try {
    const resp = await fetch(MC_NEWS_API, { signal: AbortSignal.timeout(8000) });
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const data = await resp.json();

    // 取前 4 条（默认已按日期排序，最新在前）
    const posts = (data.entries || []).slice(0, 4);

    if (!posts.length) {
      renderNewsCards(DEFAULT_NEWS);
      return;
    }

    const items = posts.map(post => {
      // 从标题生成 minecraft.net 文章 slug
      const slug = (post.title || '')
        .toLowerCase()
        .replace(/[:]/g, '')
        .replace(/[.\s]+/g, '-')
        .replace(/-+/g, '-')
        .replace(/^-|-$/g, '');
      return {
        title: post.title || '无标题',
        desc: (post.shortText || '').slice(0, 80),
        img: post.image?.url ? MC_IMG_BASE + post.image.url : 'assets/news1.png',
        link: `https://www.minecraft.net/en-us/article/${slug}`,
      };
    });

    renderNewsCards(items);
  } catch (e) {
    console.warn('[news] Minecraft 官方资讯加载失败:', e);
    renderNewsCards(DEFAULT_NEWS);
  }
})();


// MC 版本 → 所需 Java 版本映射（全局，settings.js 也可用）
function getRequiredJavaMajor(mcVersion) {
  if (!mcVersion) return 21;
  const parts = mcVersion.split('.');
  const major = parseInt(parts[0]) || 1;
  const minor = parseInt(parts[1]) || 0;
  const patch = parseInt(parts[2]) || 0;
  // MC 26.x+ → Java 25
  if (major >= 26) return 25;
  // MC 1.20.5+ → Java 21
  if (minor >= 21 || (minor === 20 && patch >= 5)) return 21;
  // MC 1.17+ → Java 17
  if (minor >= 17) return 17;
  // MC 1.16 及以下 → Java 8
  return 8;
}

// 实例缓存（供启动时查 mc_version 用）
let instancesCache = [];
let isLaunching = false;

// 显示崩溃分析弹窗
function showCrashModal(version, content, loading = false) {
  const modal = document.getElementById('crashModal');
  const body  = document.getElementById('crashModalBody');
  const ver   = document.getElementById('crashModalVersion');
  const closeBtn = document.getElementById('crashModalClose');
  if (!modal || !body) return;
  ver.textContent = loading
    ? `${version} · AI 分析中...`
    : `${version} · 退出码异常`;
  // 简单 markdown → HTML
  let html = content
    .replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
    .replace(/`([^`]+)`/g, '<code>$1</code>')
    .replace(/^### (.+)$/gm, '<h4>$1</h4>')
    .replace(/^## (.+)$/gm, '<h4>$1</h4>')
    .replace(/^\d+\.\s*(.+)$/gm, '<h4>$1</h4>')
    .replace(/^[\-\*]\s+(.+)$/gm, '<li>$1</li>')
    .replace(/\n{2,}/g, '<br>')
    .replace(/\n/g, '<br>');
  // 给连续的 <li> 块包裹 <ul>（避免重复嵌套）
  html = html.replace(/(<li>.*?<\/li>)(?:<br>)*(<li>)/g, '$1$2');
  html = html.replace(/((?:<li>.*?<\/li>)+)/g, '<ul>$1</ul>');
  if (loading) {
    html += '<div style="margin-top:16px;text-align:center"><div class="crash-loading-dot"></div></div>';
  }
  body.innerHTML = html;
  modal.style.display = '';
  // 加载中隐藏关闭按钮，分析完成后显示
  if (closeBtn) closeBtn.style.display = loading ? 'none' : '';
  if (!loading) {
    closeBtn.textContent = '我知道了';
    closeBtn.onclick = () => modal.style.display = 'none';
    modal.onclick = (e) => { if (e.target === modal) modal.style.display = 'none'; };
  }
}

// 监听后台游戏崩溃事件
(async () => {
  try {
    const tauri = await waitForTauri();
    await tauri.event.listen('game-crashed', async (event) => {
      const { version, exit_code, diagnosis, log_tail, crash_report } = event.payload;
      console.log(`游戏崩溃: ${version}, 退出码: ${exit_code}`);

      // 立即恢复启动按钮
      const btn = document.getElementById('launchBtn');
      if (btn) { resetLaunchBtn(btn); isLaunching = false; }

      // 检查是否配置了 AI
      const aiKey      = localStorage.getItem('ai_api_key') || '';
      const aiUrl      = localStorage.getItem('ai_api_url') || '';
      const aiModel    = localStorage.getItem('ai_model') || '';

      if (aiKey && aiUrl) {
        const logForAi = [
          crash_report ? `=== Crash Report ===\n${crash_report}` : '',
          log_tail ? `=== Game Log (last 150 lines) ===\n${log_tail}` : '',
        ].filter(Boolean).join('\n\n');

        if (!logForAi) {
          showCrashModal(version, diagnosis);
          return;
        }

        // 先弹窗显示"AI 分析中"加载状态，关闭按钮可取消
        const aiAbort = new AbortController();
        showCrashModal(version, '⏳ **AI 正在分析崩溃日志，请稍候...**\n\n这可能需要几秒钟时间。', true);
        // 绑定取消按钮
        const closeBtn = document.getElementById('crashModalClose');
        if (closeBtn) {
          closeBtn.style.display = '';
          closeBtn.textContent = '取消分析';
          closeBtn.onclick = () => {
            aiAbort.abort();
            document.getElementById('crashModal').style.display = 'none';
            isLaunching = false;
          };
        }

        try {
          const aiResult = await callAiApi(aiKey, aiUrl, aiModel,
            `Minecraft ${version} 崩溃了，退出码: ${exit_code}。请分析以下日志：\n\n${logForAi}`,
            aiAbort.signal
          );
          if (!aiAbort.signal.aborted) {
            showCrashModal(version, aiResult || diagnosis);
          }
        } catch (e) {
          if (aiAbort.signal.aborted) return; // 用户取消，不再显示
          console.warn('AI 分析失败，降级到本地分析:', e);
          showCrashModal(version, `${diagnosis}\n\n**AI 分析失败:** ${e.message}`);
        }
      } else {
        showCrashModal(version, diagnosis);
      }
    });

    // 监听游戏正常退出，恢复启动按钮
    await tauri.event.listen('game-exited', async () => {
      const btn = document.getElementById('launchBtn');
      if (btn) { resetLaunchBtn(btn); isLaunching = false; }
    });
  } catch (e) { console.warn('[crash] 监听崩溃事件失败:', e); }
})();

function resetLaunchBtn(btn) {
  btn.style.pointerEvents = '';
  btn.style.background = '';
  btn.innerHTML = `<span>启动游戏</span>`;
}
async function loadInstalledVersions() {
  const sel = document.getElementById('versionSelector');
  const installedList = document.getElementById('installedList');
  const installedCount = document.getElementById('installedCount');
  try {
    const tauri = await waitForTauri();
    const gameDir = localStorage.getItem('gameDir') || '';
    if (!gameDir) {
      if (installedList) installedList.innerHTML = '<div class="installed-empty">请先在设置页选择游戏目录</div>';
      if (installedCount) installedCount.textContent = '0';
      return;
    }
    const instances = await tauri.core.invoke('list_installed_versions', { gameDir });
    instancesCache = instances; // 缓存供启动时查 mc_version

    // 更新主页版本选择器（隐藏的 select + 自定义下拉）
    if (sel) {
      const prev = sel.value;
      sel.innerHTML = '<option value="">-- 选择实例 --</option>';
      instances.forEach(v => {
        const opt = document.createElement('option');
        opt.value = opt.textContent = v.name;
        sel.appendChild(opt);
      });
      const saved = localStorage.getItem('selectedVersion');
      if (saved && instances.find(v => v.name === saved)) sel.value = saved;
      else if (prev && instances.find(v => v.name === prev)) sel.value = prev;

      // 同步自定义下拉组件
      syncVersionDropdown(instances, sel.value);
    }

    // 更新下载页已安装版本列表
    if (installedList) {
      if (installedCount) installedCount.textContent = instances.length;
      if (instances.length === 0) {
        installedList.innerHTML = '<div class="installed-empty">暂无已安装实例，请在下方下载</div>';
      } else {
        installedList.innerHTML = instances.map(v => `
          <div class="installed-card" data-ver="${v.name}">
            <div style="flex: 1; min-width:0; display:flex; flex-direction:column; gap:1px;">
              <span class="ver-name">${v.name}</span>
              <span style="font-size:9px; color:#b0506e; white-space:nowrap; overflow:hidden; text-overflow:ellipsis;">MC ${v.mc_version} <span style="text-transform:capitalize;">${v.loader_type !== 'vanilla' ? '| ' + v.loader_type : ''}</span></span>
            </div>
            <button class="ver-delete" title="删除此实例" data-ver="${v.name}">🗑️</button>
          </div>
        `).join('');
        // 绑定删除事件
        installedList.querySelectorAll('.ver-delete').forEach(btn => {
          btn.addEventListener('click', async (e) => {
            e.stopPropagation();
            const ver = btn.dataset.ver;
            const confirmed = await showConfirm(`确定删除实例 ${ver} 吗？`, { title: '删除确认', kind: 'danger' });
            if (!confirmed) return;
            try {
              const tauri = await waitForTauri();
              await tauri.core.invoke('delete_version', { gameDir, name: ver });
              loadInstalledVersions();
            } catch (err) {
              showToast('删除失败: ' + err, 'error');
            }
          });
        });
        // 点击卡片 → 打开实例详情页
        installedList.querySelectorAll('.installed-card').forEach(card => {
          card.style.cursor = 'pointer';
          card.addEventListener('click', () => {
            const ver = card.dataset.ver;
            if (ver && typeof showInstanceDetail === 'function') {
              showInstanceDetail(ver);
            }
          });
        });
      }
    }
  } catch (e) {
    console.warn('加载实例列表失败:', e);
  }
}

function initLaunchButton() {
  const btn = document.getElementById('launchBtn');
  const sel = document.getElementById('versionSelector');
  if (!btn) return;

  // 保存选择
  if (sel) sel.addEventListener('change', () => localStorage.setItem('selectedVersion', sel.value));

  // 初始加载版本列表
  loadInstalledVersions();

  btn.addEventListener('click', async () => {
    if (isLaunching) return;
    isLaunching = true;
    // 读取设置
    const gameDir = localStorage.getItem('gameDir');
    let memAlloc = parseInt(localStorage.getItem('memAlloc') || '4096');
    const selectedVersion = sel ? sel.value : '';
    const loginMode = localStorage.getItem('loginMode') || 'offline';

    // 检查实例是否有推荐内存（整合包安装时自动计算的）
    if (selectedVersion) {
      const inst = instancesCache.find(i => i.name === selectedVersion);
      if (inst && inst.recommendedMemory) {
        // 使用整合包推荐内存（除非用户在实例设置里手动覆盖了）
        const instMemOverride = localStorage.getItem(`mem_${selectedVersion}`);
        if (!instMemOverride) {
          memAlloc = inst.recommendedMemory;
          console.log(`[launch] 使用整合包推荐内存: ${memAlloc}MB`);
        }
      }
    }
    // 根据登录模式决定玩家信息
    let playerName, accessToken = null, playerUuid = null;
    if (loginMode === 'online') {
      let accounts = [];
      try { accounts = JSON.parse(localStorage.getItem('msAccounts') || '[]'); } catch (e) { console.warn('[auth] 解析账号列表失败:', e); }
      const activeIdx = parseInt(localStorage.getItem('activeAccountIdx') || '0') || 0;
      const msProfile = accounts[activeIdx] || null;
      if (msProfile && msProfile.access_token) {
        playerName = msProfile.name;
        accessToken = msProfile.access_token;
        playerUuid = msProfile.uuid;
      } else {
        showToast('正版模式下未找到登录信息，请先在设置页进行微软登录', 'warn');
        isLaunching = false;
        return;
      }
    } else {
      playerName = localStorage.getItem('playerName');
    }

    // 验证
    if (!selectedVersion) { showToast('请先选择一个已安装的版本', 'warn'); isLaunching = false; return; }
    if (!gameDir) { showToast('请先在设置页选择游戏目录', 'warn'); isLaunching = false; return; }
    if (!playerName) { showToast('请先在设置页输入玩家名称或微软登录', 'warn'); isLaunching = false; return; }

    // 自动/手动 Java 选择
    const javaMode = localStorage.getItem('javaMode') || 'auto';
    let javaPath;

    if (javaMode === 'auto') {
      // 从实例缓存查真实 mc_version，防止整合包实例名无法解析
      const instInfo = instancesCache.find(v => v.name === selectedVersion);
      const mcVer = instInfo?.mc_version || selectedVersion.split('-')[0];
      const requiredMajor = getRequiredJavaMajor(mcVer);

      btn.style.pointerEvents = 'none';
      btn.innerHTML = `<span class="launch-icon">⏳</span><span>查找 Java ${requiredMajor}...</span>`;
      btn.style.background = 'linear-gradient(135deg, #f59e0b 0%, #d97706 100%)';

      try {
        const tauri = await waitForTauri();

        // 1. 先扫描系统（包括 gameDir/java/）
        const javas = await tauri.core.invoke('find_java', { gameDir });
        console.log(`[java] 扫描到 ${javas.length} 个 Java`);
        const matched = javas.find(j => j.major === requiredMajor);

        if (matched) {
          javaPath = matched.path;
          console.log(`[java] 找到匹配: Java ${requiredMajor} → ${javaPath}`);
        } else {
          // 2. 没找到 → 自动下载
          console.log(`[java] 未找到 Java ${requiredMajor}，自动下载...`);
          btn.innerHTML = `<span class="launch-icon">⏳</span><span>下载 Java ${requiredMajor}...</span>`;
          const result = await tauri.core.invoke('download_java', { major: requiredMajor, gameDir });
          if (result && result !== 'downloading') {
            javaPath = result;
            console.log(`[java] 已存在: ${javaPath}`);
          } else {
            javaPath = await new Promise((resolve, reject) => {
              let unlisten = null;
              tauri.event.listen('java-download-done', (event) => {
                const d = event.payload;
                if (d.major === requiredMajor) {
                  if (unlisten) unlisten();
                  if (d.success) resolve(d.path);
                  else reject(d.error || '下载失败');
                }
              }).then(fn => { unlisten = fn; }).catch(reject);
            });
            console.log(`[java] 下载完成: ${javaPath}`);
          }
        }
      } catch (e) {
        showToast(`Java ${requiredMajor} 获取失败: ${e}`, 'error');
        resetLaunchBtn(btn);
        isLaunching = false;
        return;
      }
    } else {
      javaPath = localStorage.getItem('selectedJavaPath');
      if (!javaPath) { showToast('请先在设置页选择 Java 路径', 'warn'); isLaunching = false; return; }
    }

    btn.style.pointerEvents = 'none';
    btn.innerHTML = `
      <span class="launch-icon">⏳</span>
      <span>正在启动...</span>
    `;
    btn.style.background = 'linear-gradient(135deg, #c084fc 0%, #a855f7 50%, #9333ea 100%)';
    createPetalBurst(btn);

    try {
      const tauri = await waitForTauri();
      const result = await tauri.core.invoke('launch_minecraft', {
        options: {
          java_path: javaPath,
          game_dir: gameDir,
          version_name: selectedVersion,
          player_name: playerName,
          memory_mb: memAlloc,
          server_ip: null,
          server_port: null,
          access_token: accessToken,
          uuid: playerUuid,
          custom_jvm_args: localStorage.getItem('customJvmArgs') || null,
        }
      });

      console.log('🎮 ' + result);
      btn.innerHTML = `
        <span class="launch-icon">✅</span>
        <span>启动成功！</span>
      `;
      btn.style.background = 'linear-gradient(135deg, #86efac 0%, #4ade80 50%, #22c55e 100%)';
    } catch (e) {
      console.log('❌ 启动失败:', e);
      const errMsg = typeof e === 'string' ? e : (e.message || '未知错误');
      // 按钮显示简短错误
      const shortMsg = errMsg.split('\n')[0].substring(0, 30);
      btn.innerHTML = `
        <span class="launch-icon">❌</span>
        <span>${shortMsg}</span>
      `;
      btn.style.background = 'linear-gradient(135deg, #fca5a5 0%, #f87171 50%, #ef4444 100%)';
      // 弹窗显示完整错误
      showToast(errMsg, 'error', 8000);
    }

    setTimeout(() => {
      resetLaunchBtn(btn);
      isLaunching = false;
    }, 8000);
  });
}



// ============ 新闻卡片悬停效果 ============
function initNewsHoverEffects() {
  const cards = document.querySelectorAll('.news-card');

  cards.forEach(card => {
    card.addEventListener('mouseenter', () => {
      // 轻微倾斜效果
      card.style.transition = 'all 0.3s ease';
    });

    card.addEventListener('mousemove', (e) => {
      const rect = card.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const y = e.clientY - rect.top;
      const centerX = rect.width / 2;
      const centerY = rect.height / 2;
      const rotateX = (y - centerY) / centerY * -3;
      const rotateY = (x - centerX) / centerX * 3;

      card.style.transform = `translateY(-3px) perspective(500px) rotateX(${rotateX}deg) rotateY(${rotateY}deg)`;
    });

    card.addEventListener('mouseleave', () => {
      card.style.transform = '';
    });
  });
}

// ============ 自定义版本下拉框 ============
function syncVersionDropdown(instances, selectedValue) {
  const list = document.getElementById('versionDropdownList');
  const text = document.getElementById('versionDropdownText');
  if (!list || !text) return;

  list.innerHTML = '';
  // 默认项
  const defItem = document.createElement('button');
  defItem.type = 'button';
  defItem.className = 'vd-item' + (!selectedValue ? ' active' : '');
  defItem.textContent = '-- 选择实例 --';
  defItem.dataset.value = '';
  list.appendChild(defItem);

  instances.forEach(v => {
    const item = document.createElement('button');
    item.type = 'button';
    item.className = 'vd-item' + (v.name === selectedValue ? ' active' : '');
    item.textContent = v.name;
    item.dataset.value = v.name;
    list.appendChild(item);
  });

  text.textContent = selectedValue || '-- 选择实例 --';
}

function initVersionDropdown() {
  const dropdown = document.getElementById('versionDropdown');
  const btn = document.getElementById('versionDropdownBtn');
  const list = document.getElementById('versionDropdownList');
  const text = document.getElementById('versionDropdownText');
  const sel = document.getElementById('versionSelector');
  if (!dropdown || !btn || !list) return;

  // 打开/关闭列表
  btn.addEventListener('click', () => {
    list.classList.toggle('hidden');
  });

  // 选择某一项
  list.addEventListener('click', (e) => {
    const item = e.target.closest('.vd-item');
    if (!item) return;
    const value = item.dataset.value;
    list.querySelectorAll('.vd-item').forEach(i => i.classList.remove('active'));
    item.classList.add('active');
    if (sel) sel.value = value;
    if (text) text.textContent = item.textContent;
    if (value) localStorage.setItem('selectedVersion', value);
    list.classList.add('hidden');
  });

  // 点外部关闭
  document.addEventListener('click', (e) => {
    if (!dropdown.contains(e.target)) {
      list.classList.add('hidden');
    }
  });
}