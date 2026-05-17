// ============ 主页逻辑 ============

// ===== Minecraft 官方资讯拉取 =====
const MC_NEWS_API = 'https://launchercontent.mojang.com/v2/javaPatchNotes.json';
const MC_IMG_BASE = 'https://launchercontent.mojang.com';
const MC_NEWS_CACHE_KEY = 'mcOfficialNewsCache';

(async function loadNews() {
  const container = document.getElementById('newsCards');
  if (!container) return;

  function readCachedNews() {
    try {
      const cached = JSON.parse(localStorage.getItem(MC_NEWS_CACHE_KEY) || '[]');
      return Array.isArray(cached) ? cached : [];
    } catch {
      return [];
    }
  }

  async function fetchWithTimeout(url, timeoutMs) {
    if (typeof AbortController === 'undefined') {
      return fetch(url);
    }
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    try {
      return await fetch(url, { signal: controller.signal });
    } finally {
      clearTimeout(timer);
    }
  }

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
    const resp = await fetchWithTimeout(MC_NEWS_API, 8000);
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const data = await resp.json();

    // 按日期降序排列，取前 4 条最新
    const posts = (data.entries || [])
      .sort((a, b) => new Date(b.date) - new Date(a.date))
      .slice(0, 4);

    if (!posts.length) {
      const cached = readCachedNews();
      if (cached.length) renderNewsCards(cached);
      else container.innerHTML = '<div class="news-empty"><p>官方资讯暂时没有内容</p></div>';
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

    localStorage.setItem(MC_NEWS_CACHE_KEY, JSON.stringify(items));
    renderNewsCards(items);
  } catch (e) {
    console.warn('[news] Minecraft 官方资讯加载失败:', e);
    const cached = readCachedNews();
    if (cached.length) {
      renderNewsCards(cached);
    } else {
      container.innerHTML = '<div class="news-empty"><p>官方资讯加载失败，请稍后重试</p></div>';
    }
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
let javaDownloadModalTimer = null;
let launchRepairModalTimer = null;

function ensureJavaDownloadModal() {
  let modal = document.getElementById('javaDownloadModal');
  if (modal) return modal;

  document.body.insertAdjacentHTML('beforeend', `
    <div class="modal-overlay hidden java-download-modal" id="javaDownloadModal" data-no-drag>
      <div class="java-download-card">
        <div class="java-download-header">
          <div>
            <div class="java-download-title" id="javaDownloadTitle">下载 Java</div>
            <div class="java-download-subtitle" id="javaDownloadSubtitle">准备中...</div>
          </div>
          <div class="java-download-actions">
            <div class="java-download-percent" id="javaDownloadPercent">0%</div>
            <button class="java-download-cancel" id="javaDownloadCancel" type="button">取消</button>
          </div>
        </div>
        <div class="java-download-bar-wrap">
          <div class="java-download-bar" id="javaDownloadBar"></div>
        </div>
        <div class="java-download-detail" id="javaDownloadDetail">正在连接下载源...</div>
      </div>
    </div>
  `);
  modal = document.getElementById('javaDownloadModal');
  document.getElementById('javaDownloadCancel')?.addEventListener('click', async () => {
    const btn = document.getElementById('javaDownloadCancel');
    const detailEl = document.getElementById('javaDownloadDetail');
    const major = Number(modal?.dataset.major || 0);
    if (!major) return;
    if (btn) {
      btn.disabled = true;
      btn.textContent = '取消中';
    }
    if (detailEl) detailEl.textContent = '正在取消下载...';
    try {
      const tauri = await waitForTauri();
      await tauri.core.invoke('cancel_java_download', { major });
    } catch (err) {
      if (btn) {
        btn.disabled = false;
        btn.textContent = '取消';
      }
      if (detailEl) detailEl.textContent = `取消失败: ${err}`;
    }
  });
  return modal;
}

function showJavaDownloadModal(major) {
  if (javaDownloadModalTimer) {
    clearTimeout(javaDownloadModalTimer);
    javaDownloadModalTimer = null;
  }
  const modal = ensureJavaDownloadModal();
  modal.dataset.major = String(major);
  modal.classList.remove('hidden');
  document.getElementById('javaDownloadTitle').textContent = `下载 Java ${major}`;
  document.getElementById('javaDownloadSubtitle').textContent = '准备下载运行环境';
  document.getElementById('javaDownloadPercent').textContent = '0%';
  document.getElementById('javaDownloadBar').style.width = '0%';
  document.getElementById('javaDownloadDetail').textContent = '正在连接下载源...';
  const cancelBtn = document.getElementById('javaDownloadCancel');
  if (cancelBtn) {
    cancelBtn.disabled = false;
    cancelBtn.textContent = '取消';
    cancelBtn.style.display = '';
  }
}

function updateJavaDownloadModal(payload) {
  const modal = ensureJavaDownloadModal();
  if (modal.classList.contains('hidden')) modal.classList.remove('hidden');

  const major = payload.major || '';
  const titleEl = document.getElementById('javaDownloadTitle');
  const subtitleEl = document.getElementById('javaDownloadSubtitle');
  const percentEl = document.getElementById('javaDownloadPercent');
  const barEl = document.getElementById('javaDownloadBar');
  const detailEl = document.getElementById('javaDownloadDetail');

  if (titleEl) titleEl.textContent = `下载 Java ${major}`;

  if (payload.stage === 'extracting') {
    if (subtitleEl) subtitleEl.textContent = '正在解压';
    if (percentEl) percentEl.textContent = '100%';
    if (barEl) barEl.style.width = '100%';
    if (detailEl) detailEl.textContent = '下载完成，正在解压 Java 文件...';
    return;
  }

  if (payload.stage === 'done') {
    if (subtitleEl) subtitleEl.textContent = '安装完成';
    if (percentEl) percentEl.textContent = '100%';
    if (barEl) barEl.style.width = '100%';
    if (detailEl) detailEl.textContent = 'Java 已安装完成，准备启动游戏。';
    return;
  }

  const downloaded = Number(payload.downloaded || 0);
  const total = Number(payload.total || 0);
  const source = payload.source ? `${payload.source}源` : '下载源';
  if (subtitleEl) subtitleEl.textContent = `正在从${source}下载`;

  if (downloaded > 0 && total > 0) {
    const percent = Math.min(100, Math.round(downloaded / total * 100));
    if (percentEl) percentEl.textContent = `${percent}%`;
    if (barEl) barEl.style.width = `${percent}%`;
    if (detailEl) detailEl.textContent = `${formatFileSize(downloaded)} / ${formatFileSize(total)}`;
  } else if (downloaded > 0) {
    if (percentEl) percentEl.textContent = '--';
    if (barEl) barEl.style.width = '12%';
    if (detailEl) detailEl.textContent = `已下载 ${formatFileSize(downloaded)}`;
  } else if (detailEl && payload.detail) {
    detailEl.textContent = String(payload.detail);
  }
}

function finishJavaDownloadModal(success, message) {
  const modal = ensureJavaDownloadModal();
  const cancelled = !success && String(message || '').includes('取消');
  modal.classList.remove('hidden');
  document.getElementById('javaDownloadSubtitle').textContent = success ? '安装完成' : (cancelled ? '已取消' : '下载失败');
  document.getElementById('javaDownloadPercent').textContent = success ? '100%' : (cancelled ? '已取消' : '失败');
  document.getElementById('javaDownloadBar').style.width = success ? '100%' : '100%';
  document.getElementById('javaDownloadBar').classList.toggle('error', !success);
  document.getElementById('javaDownloadDetail').textContent = message;
  const cancelBtn = document.getElementById('javaDownloadCancel');
  if (cancelBtn) cancelBtn.style.display = 'none';

  javaDownloadModalTimer = setTimeout(() => {
    modal.classList.add('hidden');
    document.getElementById('javaDownloadBar')?.classList.remove('error');
  }, success ? 900 : 3500);
}

function ensureLaunchRepairModal() {
  let modal = document.getElementById('launchRepairModal');
  if (modal) return modal;

  document.body.insertAdjacentHTML('beforeend', `
    <div class="modal-overlay hidden java-download-modal" id="launchRepairModal" data-no-drag>
      <div class="java-download-card">
        <div class="java-download-header">
          <div>
            <div class="java-download-title" id="launchRepairTitle">启动前修复</div>
            <div class="java-download-subtitle" id="launchRepairSubtitle">准备检查文件...</div>
          </div>
          <div class="java-download-percent" id="launchRepairPercent">0%</div>
        </div>
        <div class="java-download-bar-wrap">
          <div class="java-download-bar" id="launchRepairBar"></div>
        </div>
        <div class="java-download-detail" id="launchRepairDetail">正在检查缺失文件...</div>
      </div>
    </div>
  `);
  return document.getElementById('launchRepairModal');
}

function updateLaunchRepairModal(version, payload) {
  if (launchRepairModalTimer) {
    clearTimeout(launchRepairModalTimer);
    launchRepairModalTimer = null;
  }
  const modal = ensureLaunchRepairModal();
  modal.classList.remove('hidden');
  const stage = payload.stage || 'repair';
  const label = (window.STAGE_LABELS && window.STAGE_LABELS[stage]) || stage || '修复文件';
  const current = Number(payload.current || 0);
  const total = Number(payload.total || 0);
  const titleEl = document.getElementById('launchRepairTitle');
  const subtitleEl = document.getElementById('launchRepairSubtitle');
  const percentEl = document.getElementById('launchRepairPercent');
  const barEl = document.getElementById('launchRepairBar');
  const detailEl = document.getElementById('launchRepairDetail');

  if (titleEl) titleEl.textContent = '启动前修复';
  if (subtitleEl) subtitleEl.textContent = version || '当前版本';
  if (barEl) barEl.classList.remove('error');

  if (total > 0) {
    const percent = Math.min(100, Math.round((current / Math.max(total, 1)) * 100));
    if (percentEl) percentEl.textContent = `${percent}%`;
    if (barEl) barEl.style.width = `${percent}%`;
    if (detailEl) detailEl.textContent = `${label} ${formatStageProgress(current, total, stage)}`;
  } else {
    if (percentEl) percentEl.textContent = '--';
    if (barEl) barEl.style.width = '12%';
    if (detailEl) detailEl.textContent = String(payload.detail || label);
  }
}

function finishLaunchRepairModal(success, message) {
  const modal = document.getElementById('launchRepairModal');
  if (!modal || modal.classList.contains('hidden')) return;
  const subtitleEl = document.getElementById('launchRepairSubtitle');
  const percentEl = document.getElementById('launchRepairPercent');
  const barEl = document.getElementById('launchRepairBar');
  const detailEl = document.getElementById('launchRepairDetail');
  if (subtitleEl) subtitleEl.textContent = success ? '修复完成' : '修复失败';
  if (percentEl) percentEl.textContent = success ? '100%' : '失败';
  if (barEl) {
    barEl.style.width = '100%';
    barEl.classList.toggle('error', !success);
  }
  if (detailEl) detailEl.textContent = message || (success ? '文件检查完成，正在启动游戏。' : '启动前修复失败。');
  launchRepairModalTimer = setTimeout(() => {
    modal.classList.add('hidden');
    document.getElementById('launchRepairBar')?.classList.remove('error');
  }, success ? 700 : 3500);
}

function parseMemoryMb(value) {
  const memory = parseInt(value, 10);
  return Number.isFinite(memory) && memory > 0 ? memory : null;
}

function estimateMemoryByModCount(modCount) {
  const count = parseInt(modCount, 10) || 0;
  if (count === 0) return 2048;
  if (count <= 50) return 4096;
  if (count <= 150) return 6144;
  if (count <= 250) return 8192;
  return 10240;
}

function getInstanceAutoMemory(instance) {
  if (!instance) return null;
  const packMemory = parseMemoryMb(instance.packRecommendedMemory);
  if (packMemory) {
    return { memory: packMemory, source: '整合包内部' };
  }

  const estimatedMemory = parseMemoryMb(instance.estimatedMemory) || estimateMemoryByModCount(instance.modCount);
  if (estimatedMemory) {
    return { memory: estimatedMemory, source: '按 Mod 数估算' };
  }
  return null;
}

// 显示崩溃分析弹窗
function showCrashModal(version, content, loading = false, isLocal = false) {
  const modal = document.getElementById('crashModal');
  const body = document.getElementById('crashModalBody');
  const ver = document.getElementById('crashModalVersion');
  const closeBtn = document.getElementById('crashModalClose');
  const titleEl = modal ? modal.querySelector('.crash-modal-title') : null;
  if (!modal || !body) return;
  // 标题区分 AI 和本地
  if (titleEl) titleEl.textContent = isLocal ? '本地崩溃检测' : 'AI 崩溃分析';
  ver.textContent = loading
    ? `${version} · AI 分析中...`
    : isLocal ? `${version} · 本地检测` : `${version} · 退出码异常`;
  // 简单 markdown → HTML
  let html = escapeHtml(String(content || ''))
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
  // 本地检测提示配置 AI
  if (isLocal) {
    html += '<div style="margin-top:12px;padding:8px 12px;background:rgba(232,69,116,0.06);border-radius:8px;font-size:11px;color:#be185d;">💡 当前为本地检测，前往 <strong>设置 → AI 崩溃分析</strong> 配置 API 可获得更精准的分析结果。</div>';
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
      console.log(`[CRASH DEBUG] 游戏崩溃: ${version}, 退出码: ${exit_code}, diagnosis长度: ${(diagnosis || '').length}`);

      // 立即恢复启动按钮
      const btn = document.getElementById('launchBtn');
      if (btn) { resetLaunchBtn(btn); isLaunching = false; }

      // 检查是否配置了 AI 且已启用
      const aiKey = localStorage.getItem('ai_api_key') || '';
      const aiUrl = localStorage.getItem('ai_api_url') || '';
      const aiModel = localStorage.getItem('ai_model') || '';
      const aiEnabled = localStorage.getItem('ai_enabled') !== 'false';

      if (aiKey && aiUrl && aiEnabled) {
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
            `你是 Minecraft 崩溃分析助手。请注意以下 Java 版本要求：\n` +
            `- Minecraft 26.1+ 需要 Java 25\n` +
            `- Minecraft 1.21~1.21.11 需要 Java 21\n` +
            `- Minecraft 1.17~1.20 需要 Java 17\n` +
            `- Minecraft 1.16 及以下需要 Java 8\n\n` +
            `Minecraft ${version} 崩溃了，退出码: ${exit_code}。请分析以下日志并给出解决方案：\n\n${logForAi}`,
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
        showCrashModal(version, diagnosis || `Minecraft ${version} 异常退出，退出码: ${exit_code}`, false, true);
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

function getSelectedInstanceName() {
  const sel = document.getElementById('versionSelector');
  return (sel?.value || '').trim();
}

function updateOpenInstanceButton() {
  const btn = document.getElementById('openSelectedInstanceBtn');
  if (!btn) return;
  btn.disabled = !getSelectedInstanceName();
}

function initOpenSelectedInstanceButton() {
  const btn = document.getElementById('openSelectedInstanceBtn');
  if (!btn || btn.dataset.bound === '1') return;
  btn.dataset.bound = '1';
  btn.addEventListener('click', () => {
    const selectedVersion = getSelectedInstanceName();
    if (!selectedVersion) {
      showToast('先选一个版本，再打开设置', 'warn');
      return;
    }
    if (typeof showInstanceDetail === 'function') {
      showInstanceDetail(selectedVersion);
    }
  });
  updateOpenInstanceButton();
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
      sel.innerHTML = '<option value="">-- 选择版本 --</option>';
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
        installedList.innerHTML = '<div class="installed-empty">暂无已安装版本，请在下方下载</div>';
      } else {
        installedList.innerHTML = instances.map(v => {
          const name = escapeHtml(v.name || '');
          const mcVersion = escapeHtml(v.mc_version || '');
          const rawLoader = v.loader_type || 'vanilla';
          const loader = rawLoader !== 'vanilla' ? `| ${escapeHtml(rawLoader)}` : '';
          return `
            <div class="installed-card" data-ver="${name}">
              <div style="flex: 1; min-width:0; display:flex; flex-direction:column; gap:1px;">
                <span class="ver-name">${name}</span>
                <span style="font-size:9px; color:#b0506e; white-space:nowrap; overflow:hidden; text-overflow:ellipsis;">MC ${mcVersion} <span style="text-transform:capitalize;">${loader}</span></span>
              </div>
              <button class="ver-delete" title="删除此版本" data-ver="${name}">🗑️</button>
            </div>
          `;
        }).join('');
        // 绑定删除事件
        installedList.querySelectorAll('.ver-delete').forEach(btn => {
          btn.addEventListener('click', async (e) => {
            e.stopPropagation();
            const ver = btn.dataset.ver;
            const confirmed = await showConfirm(`确定删除版本 ${ver} 吗？`, { title: '删除确认', kind: 'danger' });
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
    console.warn('加载版本列表失败:', e);
  }
}

async function refreshInstanceForLaunch(gameDir, selectedVersion) {
  if (!gameDir || !selectedVersion) {
    return instancesCache.find(i => i.name === selectedVersion) || null;
  }
  try {
    const tauri = await waitForTauri();
    const instances = await tauri.core.invoke('list_installed_versions', { gameDir });
    if (Array.isArray(instances)) {
      instancesCache = instances;
      return instances.find(i => i.name === selectedVersion) || null;
    }
  } catch (err) {
    console.warn('[launch] 启动前刷新实例信息失败，使用页面缓存:', err);
  }
  return instancesCache.find(i => i.name === selectedVersion) || null;
}

function initLaunchButton() {
  const btn = document.getElementById('launchBtn');
  const sel = document.getElementById('versionSelector');
  if (!btn) return;

  // 保存选择
  if (sel) sel.addEventListener('change', () => {
    localStorage.setItem('selectedVersion', sel.value);
    updateOpenInstanceButton();
  });
  initOpenSelectedInstanceButton();

  // 初始加载版本列表
  loadInstalledVersions();

  btn.addEventListener('click', async () => {
    if (isLaunching) return;
    isLaunching = true;
    // 读取设置
    const gameDir = localStorage.getItem('gameDir');
    let memAlloc = parseInt(localStorage.getItem('memAlloc') || '4096');
    const memoryMode = localStorage.getItem('memoryMode') || 'manual';
    const selectedVersion = sel ? sel.value : '';
    const loginMode = localStorage.getItem('loginMode') || 'offline';

    // 实例单独设置优先；全局选择自动时，才使用整合包推荐内存。
    if (selectedVersion) {
      const inst = await refreshInstanceForLaunch(gameDir, selectedVersion);
      const instMemOverride = localStorage.getItem(`mem_${selectedVersion}`);
      if (instMemOverride) {
        memAlloc = parseInt(instMemOverride) || memAlloc;
        console.log(`[launch] 使用版本内存: ${memAlloc}MB`);
      } else if (memoryMode === 'auto') {
        const autoMemory = getInstanceAutoMemory(inst);
        if (autoMemory) {
          memAlloc = autoMemory.memory;
          console.log(`[launch] 使用自动内存(${autoMemory.source}): ${memAlloc}MB`);
        } else {
          console.log(`[launch] 自动内存无可用值，使用全局内存: ${memAlloc}MB`);
        }
      } else {
        console.log(`[launch] 使用全局手动内存: ${memAlloc}MB`);
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
    const instanceJavaMode = localStorage.getItem(`javaMode_${selectedVersion}`) || 'global';
    const javaMode = instanceJavaMode === 'global'
      ? (localStorage.getItem('javaMode') || 'auto')
      : instanceJavaMode;
    let javaPath;

    if (javaMode === 'auto') {
      // 从实例缓存查真实 mc_version，防止整合包实例名无法解析
      const instInfo = instancesCache.find(v => v.name === selectedVersion);
      const mcVer = instInfo?.mc_version || selectedVersion.split('-')[0];
      const requiredMajor = getRequiredJavaMajor(mcVer);

      btn.style.pointerEvents = 'none';
      btn.innerHTML = `<span>查找 Java ${requiredMajor}...</span>`;
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
          btn.innerHTML = `<span>下载 Java ${requiredMajor}...</span>`;
          showJavaDownloadModal(requiredMajor);
          let progressUnlisten = null;
          let doneUnlisten = null;

          let resolveJavaDownload;
          let rejectJavaDownload;
          const donePromise = new Promise((resolve, reject) => {
            resolveJavaDownload = resolve;
            rejectJavaDownload = reject;
          });
          doneUnlisten = await tauri.event.listen('java-download-done', (event) => {
            const d = event.payload;
            if (d.major === requiredMajor) {
              if (doneUnlisten) doneUnlisten();
              if (progressUnlisten) progressUnlisten();
              if (d.success) {
                finishJavaDownloadModal(true, 'Java 已安装完成，正在继续启动。');
                resolveJavaDownload(d.path);
              } else if (d.cancelled) {
                finishJavaDownloadModal(false, '已取消下载');
                rejectJavaDownload('已取消下载');
              } else {
                finishJavaDownloadModal(false, d.error || '下载失败');
                rejectJavaDownload(d.error || '下载失败');
              }
            }
          });

          progressUnlisten = await tauri.event.listen('java-download-progress', (event) => {
            const d = event.payload;
            if (d.major !== requiredMajor) return;
            updateJavaDownloadModal(d);
            if (d.stage === 'extracting') {
              btn.innerHTML = `<span>解压 Java ${requiredMajor}...</span>`;
              return;
            }
            if (d.stage === 'done') {
              btn.innerHTML = `<span>准备启动...</span>`;
              return;
            }
            const downloaded = Number(d.downloaded || 0);
            const total = Number(d.total || 0);
            if (downloaded > 0 && total > 0) {
              const percent = Math.min(100, Math.round(downloaded / total * 100));
              btn.innerHTML = `<span>下载 Java ${requiredMajor} ${percent}%</span>`;
            } else if (d.detail) {
              btn.innerHTML = `<span>${escapeHtml(String(d.detail)).slice(0, 24)}</span>`;
            }
          });

          let result;
          try {
            result = await tauri.core.invoke('download_java', { major: requiredMajor, gameDir });
          } catch (e) {
            if (doneUnlisten) doneUnlisten();
            if (progressUnlisten) progressUnlisten();
            finishJavaDownloadModal(false, String(e));
            throw e;
          }
          if (result && result !== 'downloading') {
            if (doneUnlisten) doneUnlisten();
            if (progressUnlisten) progressUnlisten();
            finishJavaDownloadModal(true, 'Java 已存在，正在继续启动。');
            javaPath = result;
            console.log(`[java] 已存在: ${javaPath}`);
          } else {
            javaPath = await donePromise;
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
      javaPath = localStorage.getItem(`javaPath_${selectedVersion}`) || localStorage.getItem('selectedJavaPath');
      if (!javaPath) { showToast('请先在版本设置或设置页选择 Java 路径', 'warn'); isLaunching = false; return; }
    }

    btn.style.pointerEvents = 'none';
    btn.innerHTML = `
      <span>正在启动...</span>
    `;
    btn.style.background = 'linear-gradient(135deg, #c084fc 0%, #a855f7 50%, #9333ea 100%)';
    createPetalBurst(btn);

    try {
      const tauri = await waitForTauri();
      let repairUnlisten = null;
      repairUnlisten = await tauri.event.listen('install-progress', (event) => {
        const d = event.payload || {};
        if (d.name !== selectedVersion) return;
        updateLaunchRepairModal(selectedVersion, d);
      });
      const instanceJvmArgs = localStorage.getItem(`jvmArgs_${selectedVersion}`);
      const customJvmArgs = instanceJvmArgs !== null
        ? instanceJvmArgs
        : (localStorage.getItem('customJvmArgs') || null);
      let result;
      try {
        result = await tauri.core.invoke('launch_minecraft', {
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
            custom_jvm_args: customJvmArgs,
          }
        });
      } finally {
        if (repairUnlisten) repairUnlisten();
      }

      console.log('🎮 ' + result);
      finishLaunchRepairModal(true, '文件检查完成，正在启动游戏。');
      btn.innerHTML = `
        <span class="launch-icon">✅</span>
        <span>启动成功！</span>
      `;
      btn.style.background = 'linear-gradient(135deg, #86efac 0%, #4ade80 50%, #22c55e 100%)';
    } catch (e) {
      console.log('❌ 启动失败:', e);
      const errMsg = typeof e === 'string' ? e : (e.message || '未知错误');
      finishLaunchRepairModal(false, errMsg.split('\n')[0]);
      // 按钮显示简短错误
      const shortMsg = errMsg.split('\n')[0].substring(0, 30);
      btn.innerHTML = `
        <span class="launch-icon">❌</span>
        <span>${escapeHtml(shortMsg)}</span>
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
  defItem.textContent = '-- 选择版本 --';
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

  text.textContent = selectedValue || '-- 选择版本 --';
  updateOpenInstanceButton();
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
    updateOpenInstanceButton();
    list.classList.add('hidden');
  });

  // 点外部关闭
  document.addEventListener('click', (e) => {
    if (!dropdown.contains(e.target)) {
      list.classList.add('hidden');
    }
  });
}
