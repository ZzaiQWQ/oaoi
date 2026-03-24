// ============ 下载页逻辑 ============
// loadInstalledVersions() 在 home.js 中定义

function initDownloadPage() {
  const dlList = document.getElementById('dlList');
  const dlSearch = document.getElementById('dlSearch');
  let allVersions = [];
  let dlFilter = 'release';
  let dlSource = localStorage.getItem('downloadSource') || 'official';

  // 下载源切换按钮
  const sourceFilters = document.getElementById('dlSourceFilters');
  if (sourceFilters) {
    const sourceBtns = sourceFilters.querySelectorAll('.dl-filter-btn');
    // 恢复保存的选择
    sourceBtns.forEach(btn => {
      btn.classList.toggle('active', btn.dataset.source === dlSource);
      btn.addEventListener('click', () => {
        sourceBtns.forEach(b => b.classList.remove('active'));
        btn.classList.add('active');
        dlSource = btn.dataset.source;
        localStorage.setItem('downloadSource', dlSource);
        fetchVersions(); // 切换时重新加载版本列表
      });
    });
  }

  async function fetchVersions() {
    try {
      const officialUrl = 'https://piston-meta.mojang.com/mc/game/version_manifest_v2.json';
      const mirrorUrl = 'https://bmclapi2.bangbang93.com/mc/game/version_manifest_v2.json';
      let resp;
      try {
        resp = await fetch(dlSource === 'bmcl' ? mirrorUrl : officialUrl);
      } catch (_) {
        resp = await fetch(dlSource === 'bmcl' ? officialUrl : mirrorUrl);
      }
      const data = await resp.json();
      allVersions = data.versions;
      renderVersions();
    } catch (e) {
      if (dlList) dlList.innerHTML = '<div class="dl-loading">❌ 加载失败，请检查网络</div>';
    }
  }

  // 愚人节版本检测（日期在3/31~4/2 且为 snapshot 类型）
  function isAprilFools(v) {
    if (v.type !== 'snapshot') return false;
    // rc / pre 是正式预发布版本，不是愚人节
    const id = (v.id || '').toLowerCase();
    if (/-rc|-pre|release/.test(id)) return false;
    const d = new Date(v.releaseTime);
    const m = d.getMonth() + 1;
    const day = d.getDate();
    return (m === 3 && day === 31) || (m === 4 && day <= 2);
  }

  let filteredVersions = [];
  let renderedCount = 0;
  const BATCH_SIZE = 50;

  function renderVersions() {
    if (!dlList) return;
    const query = (dlSearch?.value || '').toLowerCase();
    filteredVersions = allVersions.filter(v => {
      if (dlFilter !== 'all' && v.type !== dlFilter) return false;
      if (query && !v.id.toLowerCase().includes(query)) return false;
      return true;
    });

    if (filteredVersions.length === 0) {
      dlList.innerHTML = '<div class="dl-loading">没有找到匹配的版本</div>';
      return;
    }

    renderedCount = 0;
    dlList.innerHTML = '';
    appendVersionBatch();
    bindInstallButtons();
  }

  function buildVersionItem(v) {
    const date = new Date(v.releaseTime);
    const dateStr = `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`;
    const aprilFools = isAprilFools(v);
    let icon, typeName, typeClass;
    if (aprilFools) {
      icon = '🎃'; typeName = '愚人节'; typeClass = 'april-fools';
    } else if (v.type === 'release') {
      icon = '📦'; typeName = '正式版'; typeClass = 'release';
    } else if (v.type === 'old_alpha') {
      icon = '🏚️'; typeName = '远古版'; typeClass = 'old_alpha';
    } else if (v.type === 'old_beta') {
      icon = '🧱'; typeName = '远古Beta'; typeClass = 'old_beta';
    } else {
      icon = '🧪'; typeName = '快照'; typeClass = 'snapshot';
    }
    return `
      <div class="dl-item">
        <div class="dl-item-icon ${typeClass}">${icon}</div>
        <div class="dl-item-info">
          <div class="dl-item-name">${v.id}</div>
          <div class="dl-item-meta">
            <span class="dl-item-type ${typeClass}">${typeName}</span>
            ${dateStr}
          </div>
        </div>
        <button class="dl-install-btn" data-version="${v.id}" data-url="${v.url}">安装</button>
      </div>
    `;
  }

  function appendVersionBatch() {
    const batch = filteredVersions.slice(renderedCount, renderedCount + BATCH_SIZE);
    if (batch.length === 0) return;
    dlList.insertAdjacentHTML('beforeend', batch.map(buildVersionItem).join(''));
    renderedCount += batch.length;
  }

  // 无限滚动：滚到底部自动加载更多
  if (dlList) {
    dlList.addEventListener('scroll', () => {
      if (dlList.scrollTop + dlList.clientHeight >= dlList.scrollHeight - 50) {
        if (renderedCount < filteredVersions.length) {
          appendVersionBatch();
          bindInstallButtons();
        }
      }
    });
  }

  function bindInstallButtons() {

    // 打开 新建实例 对话框（只绑定未绑定的按钮）
    dlList.querySelectorAll('.dl-install-btn:not([data-bound])').forEach(btn => {
      btn.dataset.bound = '1';
      btn.addEventListener('click', () => {
        const ver = btn.dataset.version;
        const url = btn.dataset.url;

        document.getElementById('instMcVersion').value = ver;
        document.getElementById('instNameInput').value = `${ver}`;
        document.getElementById('instMetaUrl').value = url;

        document.querySelectorAll('input[name="loader"]').forEach(el => {
          if (el.value === 'vanilla') el.checked = true;
        });
        document.querySelectorAll('.loader-radio-btn').forEach(el => el.classList.remove('active'));
        document.querySelector('input[value="vanilla"]').parentElement.classList.add('active');
        document.getElementById('loaderVersionGroup').style.display = 'none';

        const createBtn = document.getElementById('createInstBtn');
        createBtn.disabled = false;
        createBtn.textContent = '确认创建';

        document.getElementById('newInstanceModal').classList.remove('hidden');
      });
    });
  }

  // 筛选按钮（仅版本类型，不影响源切换按钮）
  document.querySelectorAll('.dl-filter-btn[data-filter]').forEach(btn => {
    btn.addEventListener('click', () => {
      document.querySelectorAll('.dl-filter-btn[data-filter]').forEach(b => b.classList.remove('active'));
      btn.classList.add('active');
      dlFilter = btn.dataset.filter;
      renderVersions();
    });
  });

  // ===== 新建实例 Modal 逻辑 =====
  const modal = document.getElementById('newInstanceModal');
  if (modal) {
    const closeBtn = document.getElementById('closeModalBtn');
    const cancelBtn = document.getElementById('cancelInstBtn');
    const createBtn = document.getElementById('createInstBtn');
    const loaderVersionGroup = document.getElementById('loaderVersionGroup');
    const loaderSelect = document.getElementById('instLoaderVersion');
    const loaderTargetSpinner = document.getElementById('loaderTargetVersion');

    function closeModal() {
      modal.classList.add('hidden');
    }
    closeBtn.addEventListener('click', closeModal);
    cancelBtn.addEventListener('click', closeModal);

    // 监听加载器切换
    document.querySelectorAll('input[name="loader"]').forEach(radio => {
      radio.addEventListener('change', async (e) => {
        document.querySelectorAll('.loader-radio-btn').forEach(l => l.classList.remove('active'));
        e.target.parentElement.classList.add('active');

        const loader = e.target.value;
        const mcVer = document.getElementById('instMcVersion').value;
        const nameInput = document.getElementById('instNameInput');

        nameInput.value = loader === 'vanilla' ? mcVer : `${mcVer}-${loader}`;

        if (loader === 'vanilla') {
          loaderVersionGroup.style.display = 'none';
          return;
        }

        loaderVersionGroup.style.display = 'block';
        loaderSelect.innerHTML = '';
        loaderTargetSpinner.textContent = '加载中...';

        try {
          const tauri = await waitForTauri();
          let versions = [];
          if (loader === 'fabric') {
            versions = await tauri.core.invoke('get_fabric_versions', { mcVersion: mcVer });
          } else if (loader === 'forge') {
            versions = await tauri.core.invoke('get_forge_versions', { mcVersion: mcVer });
          } else if (loader === 'neoforge') {
            versions = await tauri.core.invoke('get_neoforge_versions', { mcVersion: mcVer });
          } else if (loader === 'quilt') {
            versions = await tauri.core.invoke('get_quilt_versions', { mcVersion: mcVer });
          }

          if (versions.length === 0) {
            loaderTargetSpinner.textContent = ' 无可用版本';
          } else {
            loaderTargetSpinner.textContent = '';
            versions.forEach(v => {
              const opt = document.createElement('option');
              opt.value = opt.textContent = v;
              loaderSelect.appendChild(opt);
            });
          }
        } catch (err) {
          loaderTargetSpinner.textContent = ' 获取失败';
          console.error(err);
        }
      });
    });

    // 确认创建实例
    createBtn.addEventListener('click', async () => {
      const name = document.getElementById('instNameInput').value.trim() || document.getElementById('instMcVersion').value;
      const mcVer = document.getElementById('instMcVersion').value;
      const metaUrl = document.getElementById('instMetaUrl').value;
      const loaderType = document.querySelector('input[name="loader"]:checked').value;
      const loaderVer = loaderSelect.value || '';

      createBtn.disabled = true;
      createBtn.textContent = '✨ 创建中...';

      let unlisten = null;
      try {
        const tauri = await waitForTauri();
        const dlActiveList = document.getElementById('dlActiveList');
        const dlId = 'dl-' + Date.now();

        closeModal();

        if (dlActiveList) {
          // 移除"暂无下载"提示
          const emptyMsg = dlActiveList.querySelector('.dl-active-empty');
          if (emptyMsg) emptyMsg.remove();
          // 确保有标题
          if (!dlActiveList.querySelector('.dl-section-title')) {
            dlActiveList.insertAdjacentHTML('afterbegin', '<h2 class="dl-section-title">⏳ 活跃下载</h2>');
          }
          // 紧凑进度卡片（与整合包安装一致）
          dlActiveList.insertAdjacentHTML('beforeend', `
            <div class="dl-progress-card" id="${dlId}">
              <div class="dl-progress-card-header">
                <div class="dl-progress-name">📦 ${name}</div>
                <button class="dl-cancel-btn" id="${dlId}-cancel" title="取消安装">✕</button>
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

          // 创建详情弹窗（共用同一个，如果已存在就复用）
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

          // 点击摘要行打开详情弹窗
          document.getElementById(`${dlId}-summary`)?.addEventListener('click', () => {
            const modal = document.getElementById('dlDetailModal');
            const titleEl = modal.querySelector('.dl-detail-title');
            if (titleEl) titleEl.textContent = `📦 ${name}`;
            const stagesContainer = document.getElementById(`${dlId}-stages`);
            const modalStages = document.getElementById('dlDetailStages');
            if (stagesContainer && modalStages) {
              modalStages.innerHTML = stagesContainer.innerHTML;
            }
            modal.classList.remove('hidden');
          });

          // 取消按钮
          document.getElementById(`${dlId}-cancel`)?.addEventListener('click', async () => {
            try {
              await tauri.core.invoke('cancel_modpack_install', { fileName: name });
            } catch (e) { console.warn('取消失败:', e); }
          });
        }

        // 使用全局 STAGE_LABELS (utils.js)

        unlisten = await tauri.event.listen('install-progress', (event) => {
          const { name: evtName, stage, current, total, detail } = event.payload;
          if (evtName !== name) return;

          let stagesContainer = document.getElementById(`${dlId}-stages`);
          let progressCard = document.getElementById(dlId);
          if (!stagesContainer) {
            const cards = document.querySelectorAll('.dl-progress-card');
            if (cards.length > 0) {
              progressCard = cards[cards.length - 1];
              stagesContainer = progressCard.querySelector('.dl-progress-stages');
            }
          }

          const summaryText = document.getElementById(`${dlId}-summary-text`);
          const mainBar = document.getElementById(`${dlId}-main-bar`);

          if (stage === 'done') {
            if (summaryText) summaryText.textContent = '✅ 安装完成！';
            if (mainBar) { mainBar.style.width = '100%'; mainBar.style.opacity = '0.5'; }
            if (stagesContainer) stagesContainer.innerHTML = '<div class="dl-stage-row"><span class="dl-stage-label">✅ 安装完成！</span></div>';
            const modalStages = document.getElementById('dlDetailStages');
            if (modalStages) modalStages.innerHTML = stagesContainer?.innerHTML || '';
            loadInstalledVersions();
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
            const label = STAGE_LABELS[stage] || stage;
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

        const gameDir = localStorage.getItem('gameDir') || '';
        const javaPath = localStorage.getItem('selectedJavaPath') || '';
        await tauri.core.invoke('create_instance', {
          name: name,
          mcVersion: mcVer,
          metaUrl: metaUrl,
          gameDir: gameDir,
          loaderType: loaderType,
          loaderVersion: loaderVer,
          javaPath: javaPath,
          useMirror: (localStorage.getItem('downloadSource') || 'official') === 'bmcl'
        });
      } catch (e) {
        console.error('创建失败:', e);
        // 清理事件监听器
        if (unlisten) unlisten();
        // 清理进度卡片
        const progressCard = document.getElementById(dlId);
        if (progressCard) progressCard.remove();
        if (dlActiveList && !dlActiveList.querySelector('.dl-progress-card')) {
          dlActiveList.innerHTML = `
            <h2 class="dl-section-title">⏳ 活跃下载</h2>
            <div class="dl-active-empty">暂无下载任务</div>
          `;
        }
        createBtn.textContent = '❌ 调用失败';
        setTimeout(() => { createBtn.textContent = '确认创建'; createBtn.disabled = false; }, 3000);
      }
    });
  }

  // 搜索
  if (dlSearch) {
    dlSearch.addEventListener('input', () => renderVersions());
  }

  // 加载版本列表
  if (dlList && allVersions.length === 0) fetchVersions();

  // ===== 整合包 Tab（逻辑在 modpack.js） =====
  initModpackTab();

  console.log('🌸 下载页已初始化');
}

