// ============ 整合包搜索/版本选择/一键安装 ============
// 由 download.js 中的 initDownloadPage() 调用

function normalizeModpackIconUrls(iconUrl, iconUrls = []) {
  const urls = Array.isArray(iconUrls) ? iconUrls : [];
  const out = [];
  [iconUrl, ...urls].forEach(url => {
    const text = String(url || '').trim();
    if (text && !out.includes(text)) out.push(text);
  });
  return out;
}

function handleModpackIconError(img) {
  if (!img) return;
  let urls = [];
  try {
    urls = JSON.parse(img.dataset.iconUrls || '[]');
  } catch (_) {
    urls = [];
  }
  const nextIndex = Number(img.dataset.iconIndex || '0') + 1;
  if (nextIndex < urls.length) {
    img.dataset.iconIndex = String(nextIndex);
    img.src = urls[nextIndex];
    return;
  }
  const placeholder = document.createElement('span');
  placeholder.className = img.className || 'modpack-icon';
  img.replaceWith(placeholder);
}

function renderModpackIcon(modpack) {
  const urls = normalizeModpackIconUrls(modpack?.icon_url || '', modpack?.icon_urls || []);
  if (!urls.length) return '<span class="modpack-icon"></span>';
  const encodedUrls = escapeHtml(JSON.stringify(urls));
  // 图标地址按后端候选顺序尝试，避免单个 CDN 失败时整张卡片没图。
  return `<img class="modpack-icon" src="${escapeHtml(urls[0])}" data-icon-index="0" data-icon-urls="${encodedUrls}" alt="" onerror="handleModpackIconError(this)">`;
}

function initModpackTab() {
  // ===== Tab 切换逻辑 =====
  let modpacksLoaded = false;
  const dlPage = document.getElementById('pageDownload');
  const dlTabs = dlPage ? dlPage.querySelectorAll('.dl-top-tab') : [];
  dlTabs.forEach(tab => {
    tab.addEventListener('click', () => {
      dlTabs.forEach(t => t.classList.remove('active'));
      tab.classList.add('active');
      (dlPage || document).querySelectorAll('.dl-tab-content').forEach(c => c.classList.remove('active'));
      const target = tab.dataset.dlTab === 'versions' ? 'dlTabVersions' : 'dlTabModpacks';
      document.getElementById(target)?.classList.add('active');
      // 首次切到整合包 tab 时自动加载热门
      if (tab.dataset.dlTab === 'modpacks' && !modpacksLoaded) {
        modpacksLoaded = true;
        doModpackSearch('');
      }
    });
  });

  // ===== 整合包搜索 =====
  const modpackSearchBtn = document.getElementById('modpackSearchBtn');
  const modpackSearchInput = document.getElementById('modpackSearch');
  let modpackCurrentQuery = '';
  let modpackOffset = 0;
  let modpackLoading = false;
  let modpackNoMore = false;
  let modpackSearchSeq = 0;
  const MODPACK_PAGE_SIZE = 20;

  if (modpackSearchBtn) {
    modpackSearchBtn.addEventListener('click', () => {
      doModpackSearch(modpackSearchInput?.value?.trim() || '');
    });
  }
  if (modpackSearchInput) {
    modpackSearchInput.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') doModpackSearch(modpackSearchInput.value.trim());
    });
  }

  // 无限滚动：滑到底部加载下一页
  const modpackListEl = document.getElementById('modpackList');
  if (modpackListEl) {
    modpackListEl.addEventListener('scroll', () => {
      if (modpackLoading || modpackNoMore) return;
      if (modpackListEl.scrollTop + modpackListEl.clientHeight >= modpackListEl.scrollHeight - 80) {
        loadMoreModpacks();
      }
    });
  }

  async function doModpackSearch(query) {
    const listEl = document.getElementById('modpackList');
    if (!listEl) return;
    const searchSeq = ++modpackSearchSeq;
    modpackCurrentQuery = query;
    modpackOffset = 0;
    modpackNoMore = false;
    modpackLoading = true;
    listEl.innerHTML = '<div class="dl-loading">搜索中...</div>';

    try {
      const tauri = await waitForTauri();
      const results = await tauri.core.invoke('search_modpacks', { query, offset: 0 });
      if (searchSeq !== modpackSearchSeq || query !== modpackCurrentQuery) return;
      if (results.length === 0) {
        listEl.innerHTML = '<div class="dl-loading">未找到整合包</div>';
        modpackNoMore = true;
        return;
      }
      listEl.innerHTML = '';
      modpackOffset = MODPACK_PAGE_SIZE;
      appendModpackCards(results);
      if (results.length < MODPACK_PAGE_SIZE) modpackNoMore = true;
    } catch (err) {
      if (searchSeq !== modpackSearchSeq) return;
      listEl.innerHTML = `<div class="dl-loading">❌ 搜索失败: ${escapeHtml(err)}</div>`;
    } finally {
      if (searchSeq === modpackSearchSeq) modpackLoading = false;
    }
  }

  async function loadMoreModpacks() {
    if (modpackLoading || modpackNoMore) return;
    const listEl = document.getElementById('modpackList');
    if (!listEl) return;
    const searchSeq = modpackSearchSeq;
    const query = modpackCurrentQuery;
    const offset = modpackOffset;
    modpackLoading = true;

    // 加个 loading 提示
    listEl.insertAdjacentHTML('beforeend', '<div class="dl-loading modpack-load-more">加载更多...</div>');

    try {
      const tauri = await waitForTauri();
      const results = await tauri.core.invoke('search_modpacks', { query, offset });
      if (searchSeq !== modpackSearchSeq || query !== modpackCurrentQuery) return;
      // 删掉 loading 提示
      listEl.querySelector('.modpack-load-more')?.remove();
      if (results.length === 0) {
        modpackNoMore = true;
      } else {
        modpackOffset += MODPACK_PAGE_SIZE;
        appendModpackCards(results);
        if (results.length < MODPACK_PAGE_SIZE) modpackNoMore = true;
      }
    } catch (err) {
      if (searchSeq !== modpackSearchSeq) return;
      listEl.querySelector('.modpack-load-more')?.remove();
      console.error('加载更多失败:', err);
    } finally {
      if (searchSeq === modpackSearchSeq) modpackLoading = false;
    }
  }

  function appendModpackCards(results) {
    const listEl = document.getElementById('modpackList');
    if (!listEl || results.length === 0) return;

    const esc = escapeHtml;
    const html = results.map(mp => {
      const dlCount = formatDownloads(mp.downloads);
      const src = getSourceInfo(mp);
      const sourceLabel = src.label;
      const sourceClass = src.cssClass;
      const hasMR = src.hasMR;
      const hasCF = src.hasCF;
      const iconHtml = renderModpackIcon(mp);
      const mrUrl = esc(mp.mr_url || '');
      const cfUrl = esc(mp.cf_url || '');
      const projectId = esc(mp.project_id || '');
      const source = esc(mp.source || '');
      const title = esc(mp.title || '');
      return `
        <div class="modpack-card">
          ${iconHtml}
          <div class="modpack-info">
            <div class="modpack-title" title="${title}">
              <span class="mod-source-tag ${sourceClass}">${esc(sourceLabel)}</span> ${title}
            </div>
            <div class="modpack-desc" title="${esc(mp.description)}">${esc(mp.description)}</div>
            <div class="modpack-meta">
              <span>${esc(mp.author)}</span>
              <span>⬇ ${esc(dlCount)}</span>
            </div>
          </div>
          <div class="modpack-actions">
            <div class="modpack-link-row">
              ${hasMR ? `<button class="modpack-link-btn mr" data-url="${mrUrl}" title="Modrinth">MR</button>` : ''}
              ${hasCF ? `<button class="modpack-link-btn cf" data-url="${cfUrl}" title="CurseForge">CF</button>` : ''}
            </div>
            <button class="modpack-install-btn" data-project-id="${projectId}" data-source="${source}" data-title="${title}">安装</button>
          </div>
        </div>
      `;
    }).join('');

    listEl.insertAdjacentHTML('beforeend', html);
    bindModpackButtons();
  }

  function bindModpackButtons() {
    const listEl = document.getElementById('modpackList');
    if (!listEl) return;

    // 绑定链接按钮
    listEl.querySelectorAll('.modpack-link-btn:not([data-bound])').forEach(btn => {
      btn.dataset.bound = '1';
      btn.addEventListener('click', async (e) => {
        e.stopPropagation();
        await openExternalUrl(btn.dataset.url);
      });
    });

    // 绑定安装按钮 → 打开版本选择弹窗
    listEl.querySelectorAll('.modpack-install-btn:not([data-bound])').forEach(btn => {
      btn.dataset.bound = '1';
      btn.addEventListener('click', async (e) => {
        e.stopPropagation();
        const projectId = btn.dataset.projectId;
        const source = btn.dataset.source;
        const title = btn.dataset.title;
        await showModpackVersionModal(projectId, source, title);
      });
    });
  }

  // ===== 版本选择弹窗 =====
  const modpackVersionModal = document.getElementById('modpackVersionModal');
  const modpackVersionCancel = document.getElementById('modpackVersionCancel');
  const MODPACK_VERSION_PAGE_SIZE = 20;
  const modpackVersionState = {
    projectId: '',
    source: '',
    title: '',
    offset: 0,
    loading: false,
    noMore: false,
    requestId: 0,
    versions: [],
  };
  if (modpackVersionCancel) {
    modpackVersionCancel.addEventListener('click', () => {
      modpackVersionModal?.classList.add('hidden');
    });
  }
  const modpackVersionListEl = document.getElementById('modpackVersionList');
  const modpackVersionScrollRoot = modpackVersionListEl?.closest('.modal-body') || modpackVersionListEl;
  const handleModpackVersionScroll = () => {
    if (modpackVersionState.loading || modpackVersionState.noMore) return;
    const root = modpackVersionScrollRoot;
    if (!root) return;
    if (root.scrollTop + root.clientHeight >= root.scrollHeight - 80) {
      loadMoreModpackVersions();
    }
  };
  modpackVersionListEl?.addEventListener('scroll', handleModpackVersionScroll);
  if (modpackVersionScrollRoot && modpackVersionScrollRoot !== modpackVersionListEl) {
    modpackVersionScrollRoot.addEventListener('scroll', handleModpackVersionScroll);
  }

  async function showModpackVersionModal(projectId, source, title) {
    const modal = document.getElementById('modpackVersionModal');
    const titleEl = document.getElementById('modpackVersionTitle');
    const listEl = document.getElementById('modpackVersionList');
    if (!modal || !listEl) return;

    titleEl.textContent = `${title} - 选择版本`;
    listEl.innerHTML = '<div class="dl-loading">正在加载版本列表...</div>';
    Object.assign(modpackVersionState, {
      projectId,
      source,
      title,
      offset: 0,
      loading: false,
      noMore: false,
      requestId: modpackVersionState.requestId + 1,
      versions: [],
    });
    modal.classList.remove('hidden');

    await loadMoreModpackVersions({ initial: true });
  }

  async function loadMoreModpackVersions(options = {}) {
    const { initial = false } = options;
    const listEl = document.getElementById('modpackVersionList');
    if (!listEl || modpackVersionState.loading || modpackVersionState.noMore) return;
    const requestId = modpackVersionState.requestId;
    modpackVersionState.loading = true;
    if (!initial) {
      listEl.insertAdjacentHTML('beforeend', '<div class="dl-loading modpack-version-load-more">加载更多...</div>');
    }
    try {
      const tauri = await waitForTauri();
      const versions = await tauri.core.invoke('get_modpack_versions', {
        projectId: modpackVersionState.projectId,
        source: modpackVersionState.source,
        offset: modpackVersionState.offset,
      });
      if (requestId !== modpackVersionState.requestId) return;
      listEl.querySelector('.modpack-version-load-more')?.remove();
      if (versions.length === 0) {
        if (initial) listEl.innerHTML = '<div class="dl-loading">暂无可用版本</div>';
        modpackVersionState.noMore = true;
        return;
      }
      if (initial) listEl.innerHTML = '';
      appendModpackVersions(versions);
      modpackVersionState.offset += MODPACK_VERSION_PAGE_SIZE;
      if (versions.length < MODPACK_VERSION_PAGE_SIZE) modpackVersionState.noMore = true;
    } catch (err) {
      if (requestId !== modpackVersionState.requestId) return;
      listEl.querySelector('.modpack-version-load-more')?.remove();
      if (initial) {
        listEl.innerHTML = `<div class="dl-loading">❌ 加载失败: ${escapeHtml(err)}</div>`;
      } else {
        console.error('加载更多整合包版本失败:', err);
      }
    } finally {
      if (requestId === modpackVersionState.requestId) {
        modpackVersionState.loading = false;
      }
    }
  }

  function appendModpackVersions(versions) {
    const listEl = document.getElementById('modpackVersionList');
    if (!listEl) return;
    const esc = escapeHtml;
    const startIndex = modpackVersionState.versions.length;
    modpackVersionState.versions.push(...versions);

    const html = versions.map((v, idx) => {
      const realIndex = startIndex + idx;
      const size = formatFileSize(v.file_size);
      return `
        <div class="dl-item">
          <div class="dl-item-info">
            <div class="dl-item-name">${esc(v.version_name)}</div>
            <div class="dl-item-meta">
              <span class="dl-item-type release">🎮 ${esc(v.mc_versions)}</span>
              ${size ? `📦 ${escapeHtml(size)}` : ''} ${v.date ? `· ${escapeHtml(v.date)}` : ''}
            </div>
          </div>
          <button class="dl-install-btn" data-index="${realIndex}" data-url="${esc(v.download_url)}" data-filename="${esc(v.file_name)}">安装</button>
        </div>
      `;
    }).join('');
    listEl.insertAdjacentHTML('beforeend', html);

    // 绑定安装按钮
    listEl.querySelectorAll('.dl-install-btn:not([data-bound])').forEach(btn => {
      btn.dataset.bound = '1';
      btn.addEventListener('click', async () => {
        const version = modpackVersionState.versions[Number(btn.dataset.index)] || {};
        const downloadUrls = Array.isArray(version.download_urls)
          ? version.download_urls.filter(Boolean)
          : [];
        const url = downloadUrls[0] || btn.dataset.url;
        const fileName = btn.dataset.filename;
        btn.textContent = '下载中...';
        btn.disabled = true;

        let unlisten = null;
        let dlId = '';
        let dlActiveList = null;
        let cancelController = null;
        if (!acquireInstallEventName(fileName)) {
          showToast(`同名安装任务正在进行: ${fileName}`, 'warn');
          btn.textContent = '安装';
          btn.disabled = false;
          return;
        }
        let eventNameAcquired = true;
        const releaseEventName = () => {
          if (!eventNameAcquired) return;
          eventNameAcquired = false;
          releaseInstallEventName(fileName);
        };
        try {
          const tauri = await waitForTauri();
          const gameDir = localStorage.getItem('gameDir') || '';
          const javaPath = localStorage.getItem('selectedJavaPath') || '';
          const useMirror = (localStorage.getItem('downloadSource') || 'official') === 'bmcl';

          // 关闭版本弹窗
          document.getElementById('modpackVersionModal')?.classList.add('hidden');

          // 创建进度卡片
          dlActiveList = document.getElementById('dlActiveList');
          dlId = createUiTaskId('dl');
          cancelController = createInstallCancelController({
            tauri,
            fileName,
            gameDir,
            getCancelButton: () => document.getElementById(`${dlId}-cancel`),
            getStatusElement: () => document.getElementById(`${dlId}-summary-text`),
            logPrefix: 'modpack',
          });
          if (dlActiveList) {
            const emptyMsg = dlActiveList.querySelector('.dl-active-empty');
            if (emptyMsg) emptyMsg.remove();
            if (!dlActiveList.querySelector('.dl-section-title')) {
              dlActiveList.insertAdjacentHTML('afterbegin', '<h2 class="dl-section-title">⏳ 活跃下载</h2>');
            }
            dlActiveList.insertAdjacentHTML('beforeend', `
              <div class="dl-progress-card" id="${dlId}">
                <div class="dl-progress-card-header">
                  <div class="dl-progress-name">📦 ${escapeHtml(fileName)}</div>
                  <button class="dl-cancel-btn" id="${dlId}-cancel" title="取消下载">✕</button>
                </div>
                <div class="dl-progress-summary" id="${dlId}-summary">
                  <span class="dl-summary-text" id="${dlId}-summary-text">准备中...</span>
                </div>
                <div class="dl-progress-bar-wrap" style="height:4px;">
                  <div class="dl-progress-bar" id="${dlId}-main-bar" style="width:0%"></div>
                </div>
                <div class="dl-progress-stages" id="${dlId}-stages" style="display:none"></div>
              </div>
            `);

            // 点击卡片打开弹窗
            document.getElementById(`${dlId}-summary`)?.addEventListener('click', () => {
              const stagesContainer = document.getElementById(`${dlId}-stages`);
              openDownloadDetail(dlId, fileName, stagesContainer);
            });

            // 绑定取消按钮
            document.getElementById(`${dlId}-cancel`)?.addEventListener('click', async (event) => {
              event.preventDefault();
              event.stopPropagation();
              await cancelController?.requestCancel();
            });
          }

          // 监听安装进度
          unlisten = await tauri.event.listen('install-progress', (event) => {
            const { name: evtName, stage, current, total, detail } = event.payload;
            // 只处理属于本任务的进度事件
            if (evtName !== fileName) return;
            // 找到隐藏的 stages 容器
            const stagesContainer = document.getElementById(`${dlId}-stages`);
            const progressCard = document.getElementById(dlId);

            // 更新卡片上的总进度摘要
            const summaryText = document.getElementById(`${dlId}-summary-text`);
            const mainBar = document.getElementById(`${dlId}-main-bar`);
            const cancelBtn = document.getElementById(`${dlId}-cancel`);
            cancelController?.markBackendReady();

            if (stage === 'done') {
              cancelController?.settle();
              releaseEventName();
              renderInstallTerminal({ taskId: dlId, stage, detail, statusElement: summaryText, mainBar, stagesContainer, cancelButton: cancelBtn });
              if (typeof loadInstalledVersions === 'function') loadInstalledVersions();
              setTimeout(() => {
                if (progressCard) progressCard.remove();
                if (dlActiveList && !dlActiveList.querySelector('.dl-progress-card')) {
                  dlActiveList.innerHTML = `
                    <h2 class="dl-section-title">⏳ 活跃下载</h2>
                    <div class="dl-active-empty">暂无下载任务</div>
                  `;
                }
                closeDownloadDetail(dlId);
              }, 2000);
              unlisten();
            } else if (stage === 'error' || stage === 'cancelled') {
              cancelController?.settle();
              releaseEventName();
              renderInstallTerminal({ taskId: dlId, stage, detail, statusElement: summaryText, mainBar, stagesContainer, cancelButton: cancelBtn });
              setTimeout(() => {
                if (progressCard) progressCard.remove();
                if (dlActiveList && !dlActiveList.querySelector('.dl-progress-card')) {
                  dlActiveList.innerHTML = `
                    <h2 class="dl-section-title">⏳ 活跃下载</h2>
                    <div class="dl-active-empty">暂无下载任务</div>
                  `;
                }
                closeDownloadDetail(dlId);
              }, 3000);
              unlisten();
            } else if (stagesContainer) {
              renderInstallProgress({ taskId: dlId, stage, current, total, detail, statusElement: summaryText, mainBar, stagesContainer });
            }
          });

          // 发起下载安装
          await tauri.core.invoke('install_modpack_direct', {
            downloadUrl: url,
            downloadUrls,
            fileName,
            gameDir,
            javaPath,
            useMirror,
            expectedSize: Number(version.file_size) || null,
            expectedSha1: version.sha1 || null,
          });
        } catch (err) {
          cancelController?.settle();
          releaseEventName();
          try {
            if (unlisten) unlisten();
          } catch (cleanupError) {
            console.warn('清理整合包监听器失败:', cleanupError);
          }
          const errorMessage = String(err || '未知错误');
          const progressCard = dlId ? document.getElementById(dlId) : null;
          const summaryText = dlId ? document.getElementById(`${dlId}-summary-text`) : null;
          const stagesContainer = dlId ? document.getElementById(`${dlId}-stages`) : null;
          const cancelBtn = dlId ? document.getElementById(`${dlId}-cancel`) : null;
          const mainBar = dlId ? document.getElementById(`${dlId}-main-bar`) : null;
          renderInstallTerminal({ taskId: dlId, stage: 'error', detail: errorMessage, statusElement: summaryText, mainBar, stagesContainer, cancelButton: cancelBtn });
          showToast(`整合包安装失败: ${errorMessage}`, 'error', 8000);
          setTimeout(() => {
            progressCard?.remove();
            if (dlId) closeDownloadDetail(dlId);
            if (dlActiveList && !dlActiveList.querySelector('.dl-progress-card')) {
              dlActiveList.innerHTML = `
                <h2 class="dl-section-title">⏳ 活跃下载</h2>
                <div class="dl-active-empty">暂无下载任务</div>
              `;
            }
          }, 8000);
          btn.textContent = '❌ 失败';
          setTimeout(() => { btn.textContent = '安装'; btn.disabled = false; }, 2000);
        }
      });
    });
  }
}
