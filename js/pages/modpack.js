// ============ 整合包搜索/版本选择/一键安装 ============
// 由 download.js 中的 initDownloadPage() 调用

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
    modpackCurrentQuery = query;
    modpackOffset = 0;
    modpackNoMore = false;
    listEl.innerHTML = '<div class="dl-loading">搜索中...</div>';

    try {
      const tauri = await waitForTauri();
      const results = await tauri.core.invoke('search_modpacks', { query, offset: 0 });
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
      listEl.innerHTML = `<div class="dl-loading">❌ 搜索失败: ${err}</div>`;
    }
  }

  async function loadMoreModpacks() {
    if (modpackLoading || modpackNoMore) return;
    modpackLoading = true;
    const listEl = document.getElementById('modpackList');
    if (!listEl) return;

    // 加个 loading 提示
    listEl.insertAdjacentHTML('beforeend', '<div class="dl-loading modpack-load-more">加载更多...</div>');

    try {
      const tauri = await waitForTauri();
      const results = await tauri.core.invoke('search_modpacks', { query: modpackCurrentQuery, offset: modpackOffset });
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
      listEl.querySelector('.modpack-load-more')?.remove();
      console.error('加载更多失败:', err);
    } finally {
      modpackLoading = false;
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
      return `
        <div class="modpack-card">
          <img class="modpack-icon" src="${mp.icon_url || ''}" alt="" onerror="this.style.display='none'">
          <div class="modpack-info">
            <div class="modpack-title" title="${esc(mp.title)}">
              <span class="mod-source-tag ${sourceClass}">${sourceLabel}</span> ${esc(mp.title)}
            </div>
            <div class="modpack-desc" title="${esc(mp.description)}">${esc(mp.description)}</div>
            <div class="modpack-meta">
              <span>${esc(mp.author)}</span>
              <span>⬇ ${dlCount}</span>
            </div>
          </div>
          <div class="modpack-actions">
            <div class="modpack-link-row">
              ${hasMR ? `<button class="modpack-link-btn mr" data-url="${mp.mr_url}" title="Modrinth">MR</button>` : ''}
              ${hasCF ? `<button class="modpack-link-btn cf" data-url="${mp.cf_url}" title="CurseForge">CF</button>` : ''}
            </div>
            <button class="modpack-install-btn" data-project-id="${mp.project_id}" data-source="${mp.source}" data-title="${esc(mp.title)}">安装</button>
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
  if (modpackVersionCancel) {
    modpackVersionCancel.addEventListener('click', () => {
      modpackVersionModal?.classList.add('hidden');
    });
  }

  async function showModpackVersionModal(projectId, source, title) {
    const modal = document.getElementById('modpackVersionModal');
    const titleEl = document.getElementById('modpackVersionTitle');
    const listEl = document.getElementById('modpackVersionList');
    if (!modal || !listEl) return;

    titleEl.textContent = `${title} - 选择版本`;
    listEl.innerHTML = '<div class="dl-loading">正在加载版本列表...</div>';
    modal.classList.remove('hidden');

    try {
      const tauri = await waitForTauri();
      const versions = await tauri.core.invoke('get_modpack_versions', { projectId, source });
      if (versions.length === 0) {
        listEl.innerHTML = '<div class="dl-loading">暂无可用版本</div>';
        return;
      }
      renderModpackVersions(versions, title);
    } catch (err) {
      listEl.innerHTML = `<div class="dl-loading">❌ 加载失败: ${err}</div>`;
    }
  }

  function renderModpackVersions(versions, modpackTitle) {
    const listEl = document.getElementById('modpackVersionList');
    if (!listEl) return;
    const esc = escapeHtml;

    listEl.innerHTML = versions.map(v => {
      const size = formatFileSize(v.file_size);
      return `
        <div class="dl-item">
          <div class="dl-item-info">
            <div class="dl-item-name">${esc(v.version_name)}</div>
            <div class="dl-item-meta">
              <span class="dl-item-type release">🎮 ${esc(v.mc_versions)}</span>
              ${size ? `📦 ${size}` : ''} ${v.date ? `· ${v.date}` : ''}
            </div>
          </div>
          <button class="dl-install-btn" data-url="${esc(v.download_url)}" data-filename="${esc(v.file_name)}">安装</button>
        </div>
      `;
    }).join('');

    // 绑定安装按钮
    listEl.querySelectorAll('.dl-install-btn').forEach(btn => {
      btn.addEventListener('click', async () => {
        const url = btn.dataset.url;
        const fileName = btn.dataset.filename;
        btn.textContent = '下载中...';
        btn.disabled = true;

        try {
          const tauri = await waitForTauri();
          const gameDir = localStorage.getItem('gameDir') || '';
          const javaPath = localStorage.getItem('selectedJavaPath') || '';
          const useMirror = (localStorage.getItem('downloadSource') || 'official') === 'bmcl';

          // 关闭版本弹窗
          document.getElementById('modpackVersionModal')?.classList.add('hidden');

          // 创建进度卡片
          const dlActiveList = document.getElementById('dlActiveList');
          const dlId = 'dl-' + Date.now();
          if (dlActiveList) {
            const emptyMsg = dlActiveList.querySelector('.dl-active-empty');
            if (emptyMsg) emptyMsg.remove();
            if (!dlActiveList.querySelector('.dl-section-title')) {
              dlActiveList.insertAdjacentHTML('afterbegin', '<h2 class="dl-section-title">⏳ 活跃下载</h2>');
            }
            dlActiveList.insertAdjacentHTML('beforeend', `
              <div class="dl-progress-card" id="${dlId}">
                <div class="dl-progress-card-header">
                  <div class="dl-progress-name">📦 ${fileName}</div>
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

            // 创建详情弹窗（隐藏，点击卡片打开）
            let detailModal = document.getElementById('dlDetailModal');
            if (!detailModal) {
              document.body.insertAdjacentHTML('beforeend', `
                <div class="dl-detail-modal hidden" id="dlDetailModal">
                  <div class="dl-detail-content">
                    <div class="dl-detail-header">
                      <span class="dl-detail-title">安装详情</span>
                      <button class="dl-detail-close" id="dlDetailClose">✕</button>
                    </div>
                    <div class="dl-detail-stages" id="dlDetailStages"></div>
                  </div>
                </div>
              `);
              detailModal = document.getElementById('dlDetailModal');
              document.getElementById('dlDetailClose').addEventListener('click', () => {
                detailModal.classList.add('hidden');
              });
              detailModal.addEventListener('click', (e) => {
                if (e.target === detailModal) detailModal.classList.add('hidden');
              });
            }

            // 点击卡片打开弹窗
            document.getElementById(`${dlId}-summary`)?.addEventListener('click', () => {
              const modal = document.getElementById('dlDetailModal');
              const title = modal.querySelector('.dl-detail-title');
              if (title) title.textContent = `📦 ${fileName}`;
              // 复制 stages 到弹窗中
              const stagesContainer = document.getElementById(`${dlId}-stages`);
              const modalStages = document.getElementById('dlDetailStages');
              if (stagesContainer && modalStages) {
                modalStages.innerHTML = stagesContainer.innerHTML;
              }
              modal.classList.remove('hidden');
            });

            // 绑定取消按钮
            document.getElementById(`${dlId}-cancel`)?.addEventListener('click', async () => {
              try {
                await tauri.core.invoke('cancel_modpack_install', { fileName });
              } catch (e) { console.warn('取消失败:', e); }
            });
          }

          const STAGE_LABELS2 = STAGE_LABELS; // 使用全局 utils.js

          // 监听安装进度
          const unlisten = await tauri.event.listen('install-progress', (event) => {
            const { name: evtName, stage, current, total, detail } = event.payload;
            // 只处理属于本任务的进度事件
            if (evtName !== fileName) return;
            // 找到隐藏的 stages 容器
            let stagesContainer = document.getElementById(`${dlId}-stages`);
            let progressCard = document.getElementById(dlId);
            if (!stagesContainer) {
              const cards = document.querySelectorAll('.dl-progress-card');
              if (cards.length > 0) {
                progressCard = cards[cards.length - 1];
                stagesContainer = progressCard.querySelector('.dl-progress-stages');
              }
            }

            // 更新卡片上的总进度摘要
            const summaryText = document.getElementById(`${dlId}-summary-text`);
            const mainBar = document.getElementById(`${dlId}-main-bar`);

            if (stage === 'done') {
              if (summaryText) summaryText.textContent = '✅ 安装完成！';
              if (mainBar) { mainBar.style.width = '100%'; mainBar.style.opacity = '0.5'; }
              if (stagesContainer) stagesContainer.innerHTML = '<div class="dl-stage-row"><span class="dl-stage-label">✅ 安装完成！</span></div>';
              // 更新弹窗
              const modalStages = document.getElementById('dlDetailStages');
              if (modalStages) modalStages.innerHTML = stagesContainer?.innerHTML || '';
              if (typeof loadInstalledVersions === 'function') loadInstalledVersions();
              const cancelBtn = document.getElementById(`${dlId}-cancel`);
              if (cancelBtn) cancelBtn.style.display = 'none';
              setTimeout(() => {
                if (progressCard) progressCard.remove();
                if (dlActiveList && !dlActiveList.querySelector('.dl-progress-card')) {
                  dlActiveList.innerHTML = `
                    <h2 class="dl-section-title">⏳ 活跃下载</h2>
                    <div class="dl-active-empty">暂无下载任务</div>
                  `;
                }
                const modal = document.getElementById('dlDetailModal');
                if (modal) modal.classList.add('hidden');
              }, 2000);
              unlisten();
            } else if (stage === 'error' || stage === 'cancelled') {
              const icon = stage === 'cancelled' ? '🚫' : '❌';
              const msg = stage === 'cancelled' ? '已取消' : detail;
              if (summaryText) summaryText.textContent = `${icon} ${msg}`;
              if (mainBar) mainBar.style.width = '0%';
              if (stagesContainer) stagesContainer.innerHTML = `<div class="dl-stage-row"><span class="dl-stage-label" style="color:#ef4444">${icon} ${msg}</span></div>`;
              const modalStages = document.getElementById('dlDetailStages');
              if (modalStages) modalStages.innerHTML = stagesContainer?.innerHTML || '';
              const cancelBtn = document.getElementById(`${dlId}-cancel`);
              if (cancelBtn) cancelBtn.style.display = 'none';
              setTimeout(() => {
                if (progressCard) progressCard.remove();
                if (dlActiveList && !dlActiveList.querySelector('.dl-progress-card')) {
                  dlActiveList.innerHTML = `
                    <h2 class="dl-section-title">⏳ 活跃下载</h2>
                    <div class="dl-active-empty">暂无下载任务</div>
                  `;
                }
                const modal = document.getElementById('dlDetailModal');
                if (modal) modal.classList.add('hidden');
              }, 3000);
              unlisten();
            } else if (stagesContainer) {
              // 更新总进度摘要文字
              const label = STAGE_LABELS2[stage] || stage;
              if (total > 0) {
                let progressText;
                if (stage === 'downloading') {
                  progressText = `${(current / 1048576).toFixed(1)}MB / ${(total / 1048576).toFixed(1)}MB`;
                } else {
                  progressText = `${current}/${total}`;
                }
                if (summaryText) summaryText.textContent = `${label} ${progressText}`;
                if (mainBar) mainBar.style.width = Math.min(100, Math.round((current / total) * 100)) + '%';
              } else {
                if (summaryText) summaryText.textContent = label;
              }

              // 更新隐藏的 stages 容器
              const stageId = `${dlId}-stage-${stage}`;
              let row = document.getElementById(stageId);
              if (!row) {
                stagesContainer.insertAdjacentHTML('beforeend', `
                  <div class="dl-stage-row" id="${stageId}">
                    <div class="dl-stage-head">
                      <span class="dl-stage-label">${label}</span>
                      <span class="dl-stage-count" id="${stageId}-count"></span>
                    </div>
                    <div class="dl-progress-bar-wrap" style="height:4px;">
                      <div class="dl-progress-bar" id="${stageId}-bar" style="width:0%"></div>
                    </div>
                  </div>
                `);
                row = document.getElementById(stageId);
              }
              const countEl = document.getElementById(`${stageId}-count`);
              const barEl = document.getElementById(`${stageId}-bar`);
              if (total > 0) {
                const pct = Math.min(100, Math.round((current / total) * 100));
                if (countEl) countEl.textContent = `${current}/${total}`;
                if (barEl) barEl.style.width = pct + '%';
                if (pct >= 100 && barEl) barEl.style.opacity = '0.5';
              } else {
                if (countEl) countEl.textContent = detail;
              }

              // 如果弹窗打开着，实时同步内容
              const modal = document.getElementById('dlDetailModal');
              if (modal && !modal.classList.contains('hidden')) {
                const modalStages = document.getElementById('dlDetailStages');
                if (modalStages) modalStages.innerHTML = stagesContainer.innerHTML;
              }
            }
          });

          // 发起下载安装
          await tauri.core.invoke('install_modpack_direct', {
            downloadUrl: url, fileName, gameDir, javaPath, useMirror
          });
        } catch (err) {
          btn.textContent = '❌ 失败';
          setTimeout(() => { btn.textContent = '安装'; btn.disabled = false; }, 2000);
        }
      });
    });
  }
}
