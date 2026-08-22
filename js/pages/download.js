// ============ 下载页逻辑 ============
// loadInstalledVersions() 在 home.js 中定义

function initDownloadPage() {
  const dlList = document.getElementById('dlList');
  const dlSearch = document.getElementById('dlSearch');
  let allVersions = [];
  let dlFilter = 'release';
  let dlSource = localStorage.getItem('downloadSource') || 'official';
  if (!['official', 'bmcl'].includes(dlSource)) dlSource = 'official';
  let versionFetchToken = 0;
  let loaderFetchToken = 0;
  let nameCheckToken = 0;
  let validateInstanceName = async () => true;

  // 所有版本都显示 Loader 入口，实际可用版本由对应接口返回。
  function refreshLoaderChoices() {
    document.querySelectorAll('input[name="loader"]').forEach(radio => {
      radio.disabled = false;
      radio.closest('.loader-radio-btn')?.removeAttribute('hidden');
    });
  }

  async function getInstalledNameSet() {
    const tauri = await waitForTauri();
    const gameDir = localStorage.getItem('gameDir') || '';
    if (!gameDir) return new Set();
    const instances = await tauri.core.invoke('list_installed_versions', { gameDir });
    return new Set((instances || []).map(item => item.name));
  }

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
        renderVersions(); // 只刷新安装按钮携带的下载源，不重新拉版本列表
      });
    });
  }

  async function fetchVersions() {
    const fetchToken = ++versionFetchToken;
    try {
      const result = await loadVersionManifest();
      if (fetchToken !== versionFetchToken) return;
      allVersions = Array.isArray(result.data.versions) ? result.data.versions : [];
      renderVersions();
    } catch (e) {
      if (fetchToken !== versionFetchToken) return;
      if (dlList) dlList.innerHTML = '<div class="dl-loading">❌ 加载失败，请检查网络</div>';
    }
  }

  async function loadVersionManifest() {
    const resp = await fetch('https://piston-meta.mojang.com/mc/game/version_manifest_v2.json');
    if (!resp.ok) throw new Error(`版本清单请求失败: ${resp.status}`);
    return { source: 'official', data: await resp.json() };
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
    const versionId = escapeHtml(v.id || '');
    const metaUrl = escapeHtml(v.url || '');
    const source = escapeHtml(dlSource);
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
          <div class="dl-item-name">${versionId}</div>
          <div class="dl-item-meta">
            <span class="dl-item-type ${typeClass}">${typeName}</span>
            ${dateStr}
          </div>
        </div>
        <button class="dl-install-btn" data-version="${versionId}" data-url="${metaUrl}" data-source="${source}">安装</button>
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

        const modal = document.getElementById('newInstanceModal');
        if (modal) {
          modal.dataset.mcVersion = ver;
          modal.dataset.downloadSource = dlSource;
          modal.dataset.loaderReady = '1';
          modal.dataset.nameValid = '0';
        }
        const nameInput = document.getElementById('instNameInput');
        nameInput.value = `${ver}`;
        document.getElementById('instMetaUrl').value = url;
        refreshLoaderChoices(ver);
        modal
          ?.querySelector('.modal-header')
          ?.setAttribute('data-subtitle', '取个名，选个 Loader，就可以开整。');

        document.querySelectorAll('input[name="loader"]').forEach(el => {
          if (el.value === 'vanilla') el.checked = true;
        });
        document.querySelectorAll('.loader-radio-btn').forEach(el => el.classList.remove('active'));
        document.querySelector('input[value="vanilla"]').parentElement.classList.add('active');
        document.getElementById('loaderVersionGroup').style.display = 'none';
        loaderFetchToken++;

        const createBtn = document.getElementById('createInstBtn');
        createBtn.disabled = true;
        createBtn.textContent = '确认创建';
        validateInstanceName();

        modal?.classList.remove('hidden');
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
    const nameInput = document.getElementById('instNameInput');
    const nameHint = document.getElementById('instNameHint');

    function updateCreateButtonState() {
      const loaderReady = modal.dataset.loaderReady !== '0';
      const nameValid = modal.dataset.nameValid === '1';
      createBtn.disabled = !loaderReady || !nameValid;
    }

    validateInstanceName = async function() {
      const token = ++nameCheckToken;
      const name = (nameInput?.value || '').trim();
      modal.dataset.nameValid = '0';
      nameInput?.classList.remove('has-error');
      if (nameHint) {
        nameHint.textContent = '';
        nameHint.classList.remove('error');
      }
      if (!name) {
        if (nameHint) {
          nameHint.textContent = '版本名称不能为空';
          nameHint.classList.add('error');
        }
        nameInput?.classList.add('has-error');
        updateCreateButtonState();
        return false;
      }
      try {
        const installedNames = await getInstalledNameSet();
        if (token !== nameCheckToken) return false;
        if (installedNames.has(name)) {
          if (nameHint) {
            nameHint.textContent = '这个版本名称已经存在，请换一个名称';
            nameHint.classList.add('error');
          }
          nameInput?.classList.add('has-error');
          updateCreateButtonState();
          return false;
        }
        modal.dataset.nameValid = '1';
        updateCreateButtonState();
        return true;
      } catch (err) {
        if (token !== nameCheckToken) return false;
        if (nameHint) {
          nameHint.textContent = '暂时无法检查重名，请稍后再试';
          nameHint.classList.add('error');
        }
        nameInput?.classList.add('has-error');
        updateCreateButtonState();
        return false;
      }
    };

    function closeModal() {
      modal.classList.add('hidden');
    }
    closeBtn.addEventListener('click', closeModal);
    cancelBtn.addEventListener('click', closeModal);
    nameInput?.addEventListener('input', validateInstanceName);

    // 监听加载器切换
    document.querySelectorAll('input[name="loader"]').forEach(radio => {
      radio.addEventListener('change', async (e) => {
        document.querySelectorAll('.loader-radio-btn').forEach(l => l.classList.remove('active'));
        e.target.parentElement.classList.add('active');

        const loader = e.target.value;
        const mcVer = modal.dataset.mcVersion || '';

        nameInput.value = loader === 'vanilla' ? mcVer : `${mcVer}-${loader}`;
        validateInstanceName();

        if (loader === 'vanilla') {
          loaderFetchToken++;
          loaderVersionGroup.style.display = 'none';
          loaderSelect.innerHTML = '';
          loaderTargetSpinner.textContent = '';
          modal.dataset.loaderReady = '1';
          updateCreateButtonState();
          return;
        }

        const fetchToken = ++loaderFetchToken;
        loaderVersionGroup.style.display = 'block';
        loaderSelect.innerHTML = '';
        loaderTargetSpinner.textContent = '加载中...';
        modal.dataset.loaderReady = '0';
        updateCreateButtonState();

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

          const currentLoader = document.querySelector('input[name="loader"]:checked')?.value;
          if (fetchToken !== loaderFetchToken || currentLoader !== loader || modal.dataset.mcVersion !== mcVer) {
            return;
          }

          if (versions.length === 0) {
            loaderTargetSpinner.textContent = ' 无可用版本';
            modal.dataset.loaderReady = '0';
            updateCreateButtonState();
          } else {
            loaderTargetSpinner.textContent = '';
            versions.forEach(v => {
              const opt = document.createElement('option');
              opt.value = opt.textContent = v;
              loaderSelect.appendChild(opt);
            });
            modal.dataset.loaderReady = '1';
            updateCreateButtonState();
          }
        } catch (err) {
          if (fetchToken !== loaderFetchToken) return;
          loaderTargetSpinner.textContent = ' 获取失败';
          modal.dataset.loaderReady = '0';
          updateCreateButtonState();
          console.error(err);
        }
      });
    });

    // 确认创建实例
    createBtn.addEventListener('click', async () => {
      if (!(await validateInstanceName())) return;

      // 异步重名检查完成后重新读取选择，避免提交检查期间已经切换掉的 Loader。
      const mcVer = modal.dataset.mcVersion || '';
      const name = document.getElementById('instNameInput').value.trim() || mcVer;
      const metaUrl = document.getElementById('instMetaUrl').value;
      const downloadSource = modal.dataset.downloadSource || dlSource;
      const loaderRadio = document.querySelector('input[name="loader"]:checked');
      if (!loaderRadio) return;
      const loaderType = loaderRadio.value;
      const loaderVer = loaderSelect.value || '';
      if (modal.dataset.loaderReady === '0') return;

      createBtn.disabled = true;
      createBtn.textContent = '✨ 创建中...';

      let unlisten = null;
      let cancelController = null;
      const dlActiveList = document.getElementById('dlActiveList');
      const dlId = createUiTaskId('dl');
      if (!acquireInstallEventName(name)) {
        showToast(`同名安装任务正在进行: ${name}`, 'warn');
        createBtn.textContent = '确认创建';
        createBtn.disabled = false;
        return;
      }
      let eventNameAcquired = true;
      const releaseEventName = () => {
        if (!eventNameAcquired) return;
        eventNameAcquired = false;
        releaseInstallEventName(name);
      };
      try {
        const tauri = await waitForTauri();
        const gameDir = localStorage.getItem('gameDir') || '';

        closeModal();

        if (dlActiveList) {
          createInstallProgressCard(dlActiveList, dlId, name);
          document.getElementById(`${dlId}-cancel`)?.setAttribute('title', '取消安装');

          // 点击摘要行打开详情弹窗
          document.getElementById(`${dlId}-summary`)?.addEventListener('click', () => {
            const stagesContainer = document.getElementById(`${dlId}-stages`);
            openDownloadDetail(dlId, name, stagesContainer);
          });

          // 取消按钮
          document.getElementById(`${dlId}-cancel`)?.addEventListener('click', async (event) => {
            event.preventDefault();
            event.stopPropagation();
            await cancelController?.requestCancel();
          });
        }

        cancelController = createInstallCancelController({
          tauri,
          fileName: name,
          gameDir,
          getCancelButton: () => document.getElementById(`${dlId}-cancel`),
          getStatusElement: () => document.getElementById(`${dlId}-summary-text`),
          logPrefix: 'install',
        });

        unlisten = await tauri.event.listen('install-progress', (event) => {
          const { name: evtName, stage, current, total, detail } = event.payload;
          if (evtName !== name) return;

          const stagesContainer = document.getElementById(`${dlId}-stages`);
          const summaryText = document.getElementById(`${dlId}-summary-text`);
          const mainBar = document.getElementById(`${dlId}-main-bar`);
          const cancelBtn = document.getElementById(`${dlId}-cancel`);
          cancelController?.markBackendReady();

          if (stage === 'done' || stage === 'error' || stage === 'cancelled') {
            cancelController?.settle();
            releaseEventName();
            renderInstallTerminal({ taskId: dlId, stage, detail, statusElement: summaryText, mainBar, stagesContainer, cancelButton: cancelBtn });
            if (stage === 'done') loadInstalledVersions();
            setTimeout(() => removeInstallProgressCard(dlActiveList, dlId), stage === 'done' ? 2000 : 3000);
            if (unlisten) unlisten();
          } else if (stagesContainer) {
            renderInstallProgress({ taskId: dlId, stage, current, total, detail, statusElement: summaryText, mainBar, stagesContainer });
          }
        });

        const javaPath = localStorage.getItem('selectedJavaPath') || '';
        await tauri.core.invoke('create_instance', {
          name: name,
          mcVersion: mcVer,
          metaUrl: metaUrl,
          gameDir: gameDir,
          loaderType: loaderType,
          loaderVersion: loaderVer,
          javaPath: javaPath,
          useMirror: downloadSource === 'bmcl'
        });
      } catch (e) {
        cancelController?.settle();
        releaseEventName();
        console.error('创建失败:', e);
        try {
          if (unlisten) unlisten();
        } catch (cleanupError) {
          console.warn('清理安装监听器失败:', cleanupError);
        }
        const errorMessage = String(e || '未知错误');
        const summaryText = document.getElementById(`${dlId}-summary-text`);
        const stagesContainer = document.getElementById(`${dlId}-stages`);
        const cancelBtn = document.getElementById(`${dlId}-cancel`);
        const mainBar = document.getElementById(`${dlId}-main-bar`);
        renderInstallTerminal({ taskId: dlId, stage: 'error', detail: errorMessage, statusElement: summaryText, mainBar, stagesContainer, cancelButton: cancelBtn });
        showToast(`创建失败: ${errorMessage}`, 'error', 8000);
        setTimeout(() => removeInstallProgressCard(dlActiveList, dlId), 8000);
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

