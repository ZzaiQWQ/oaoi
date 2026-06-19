// ============ 实例详情页逻辑 ============

let currentDetailInstance = null;
let currentDetailInfo = null; // { mc_version, loader_type }
let currentModList = [];
let modListLoadSeq = 0;
let modListRenderSeq = 0;
let modListStreamEventBound = false;
let modListStreamRequestId = '';
let modUpdateSeq = 0;
let modUpdateCheckedKey = '';
let modUpdateLoadingKey = '';
let currentModUpdateList = [];
let currentModSourceLinks = [];
let currentOldModBackups = [];
let currentModRollbackRecords = [];
let currentModBulkTab = 'updates';
let modUpdateReady = false;
let modUpdateCacheEventBound = false;
let instanceSettingsModal = null;
let modBulkUpdateModal = null;
const modpackExportTask = {
  running: false,
  name: '',
  format: '',
  progress: { stage: 'idle', current: 0, total: 100, detail: '' },
  result: null,
  error: null,
  unlisten: null,
};

function cleanModpackArchiveName(name) {
  let cleaned = String(name || '').trim();
  while (/\.(zip|mrpack)$/i.test(cleaned)) {
    cleaned = cleaned.replace(/\.(zip|mrpack)$/i, '').trim();
  }
  return cleaned;
}

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
  updateInstanceSettingsSummary();

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
    const cacheKey = `all:${instance.loader_type || ''}:${currentOnlineCategory}:`;
    const cached = onlineSearchCache[cacheKey];
    if (cached) {
      renderOnlineResults(cached._data || cached, '');
    } else {
      const typeLabel = { mod: 'Mod', resourcepack: '材质包', shader: '光影包' }[currentOnlineCategory] || 'Mod';
      onlineList.innerHTML = `<div class="mod-list-empty">输入关键词搜索 Modrinth + CurseForge 上的全部 ${typeLabel} 版本</div>`;
    }
  }

  currentModList = [];
  resetModUpdateState('正在读取更新缓存...');
  const modListEl = document.getElementById('modList');
  if (modListEl) modListEl.innerHTML = '<div class="mod-list-empty">加载中...</div>';
  const modCountEl = document.getElementById('modCount');
  if (modCountEl) modCountEl.textContent = String(instance.modCount ?? 0);
  requestAnimationFrame(() => {
    setTimeout(() => {
      if (currentDetailInstance === instanceName) loadModList(instanceName);
    }, 0);
  });
}

function instanceSettingKey(prefix) {
  return `${prefix}_${currentDetailInstance}`;
}

function updateInstanceSettingsSummary() {
  if (!currentDetailInstance) return;
  const summary = document.getElementById('instanceConfigBtn');
  if (!summary) return;
  const parts = [];
  const mem = localStorage.getItem(instanceSettingKey('mem'));
  const javaMode = localStorage.getItem(instanceSettingKey('javaMode'));
  const jvmPreset = getInstanceJvmPresetKey();
  if (mem) parts.push(`${mem} MB`);
  if (javaMode && javaMode !== 'global') parts.push(javaMode === 'auto' ? '自动 Java' : '指定 Java');
  if (jvmPreset === 'clean') parts.push('纯净 JVM');
  else if (jvmPreset !== 'global') parts.push('自定义 JVM');
  summary.textContent = parts.length ? `参数设置 (${parts.join(' · ')})` : '参数设置';
}

function getInstanceJvmPresetKey() {
  if (!currentDetailInstance) return 'global';
  const saved = localStorage.getItem(instanceSettingKey('jvmPreset'));
  if (saved && (saved === 'global' || OAOI_JVM_PRESETS?.[saved])) return saved;
  const value = localStorage.getItem(instanceSettingKey('jvmArgs'));
  if (value === null) return 'global';
  return typeof getJvmArgsPresetByValue === 'function'
    ? getJvmArgsPresetByValue(value)
    : 'custom';
}

function updateInstanceJvmPresetUI() {
  const row = document.getElementById('instanceJvmPresetRow');
  const input = document.getElementById('instanceJvmArgsInput');
  if (!row || !input) return;
  const preset = getInstanceJvmPresetKey();
  row.querySelectorAll('.instance-jvm-preset').forEach(btn => {
    btn.classList.toggle('active', btn.dataset.preset === preset);
  });
  input.disabled = preset === 'global' || preset === 'clean';
  input.placeholder = preset === 'global'
    ? '跟随全局 JVM 参数'
    : preset === 'clean'
      ? '纯净模式：不添加额外 JVM 参数'
      : '可继续编辑，编辑后变为自定义';
}

function setInstanceJvmPreset(preset) {
  const input = document.getElementById('instanceJvmArgsInput');
  if (!input) return;
  localStorage.setItem(instanceSettingKey('jvmPreset'), preset);
  if (preset === 'global') {
    input.value = '';
  } else if (preset === 'clean') {
    input.value = '';
  } else if (preset !== 'custom' && OAOI_JVM_PRESETS?.[preset]) {
    input.value = OAOI_JVM_PRESETS[preset].args || '';
  }
  updateInstanceJvmPresetUI();
}

function ensureInstanceSettingsModal() {
  if (instanceSettingsModal) return instanceSettingsModal;
  const modal = document.createElement('div');
  modal.id = 'instanceSettingsModal';
  modal.className = 'hidden instance-settings-modal oaoi-modal-host';
  modal.innerHTML = `
    <div class="modal-content oaoi-modal-card" data-no-drag>
      <div class="modal-header">
        <h2>参数设置</h2>
        <button class="modal-close" id="instanceSettingsModalClose">&times;</button>
      </div>
      <div class="modal-body">
        <div class="instance-settings-grid">
          <label class="instance-setting-field">
            <span>内存 MB</span>
            <input type="number" id="instanceMemInput" min="512" step="512" placeholder="跟随默认">
          </label>
          <label class="instance-setting-field">
            <span>Java</span>
            <select id="instanceJavaMode">
              <option value="global">跟随全局</option>
              <option value="auto">自动匹配</option>
              <option value="manual">指定路径</option>
            </select>
          </label>
          <label class="instance-setting-field instance-setting-wide" id="instanceJavaPathRow">
            <span>Java 路径</span>
            <div class="instance-path-row">
              <input type="text" id="instanceJavaPathInput" placeholder="留空则使用全局 Java 路径">
              <button type="button" id="instanceUseGlobalJavaBtn">使用全局</button>
            </div>
          </label>
          <label class="instance-setting-field instance-setting-wide">
            <span>JVM 参数</span>
            <div class="instance-jvm-preset-row" id="instanceJvmPresetRow">
              <button type="button" class="instance-jvm-preset active" data-preset="global">跟随全局</button>
              <button type="button" class="instance-jvm-preset" data-preset="recommended">推荐</button>
              <button type="button" class="instance-jvm-preset" data-preset="compat">兼容</button>
              <button type="button" class="instance-jvm-preset" data-preset="clean">纯净</button>
              <button type="button" class="instance-jvm-preset" data-preset="custom">自定义</button>
            </div>
            <textarea id="instanceJvmArgsInput" class="instance-jvm-args-textarea" spellcheck="false" placeholder="留空则跟随全局 JVM 参数"></textarea>
          </label>
        </div>
        <div class="instance-settings-actions">
          <button type="button" id="instanceSettingsSave">保存设置</button>
          <button type="button" id="instanceSettingsReset">恢复默认</button>
          <span id="instanceSettingsHint"></span>
        </div>
      </div>
    </div>
  `;
  document.body.appendChild(modal);
  instanceSettingsModal = modal;

  const close = () => {
    modal.remove();
    instanceSettingsModal = null;
  };
  modal.querySelector('#instanceSettingsModalClose')?.addEventListener('click', close);
  modal.addEventListener('click', (e) => {
    if (e.target === modal) close();
  });
  modal.querySelector('#instanceJavaMode')?.addEventListener('change', updateInstanceJavaPathState);
  modal.querySelector('#instanceUseGlobalJavaBtn')?.addEventListener('click', () => {
    const input = modal.querySelector('#instanceJavaPathInput');
    if (input) input.value = localStorage.getItem('selectedJavaPath') || '';
  });
  modal.querySelector('#instanceSettingsSave')?.addEventListener('click', saveInstanceSettings);
  modal.querySelector('#instanceSettingsReset')?.addEventListener('click', resetInstanceSettings);
  modal.querySelectorAll('.instance-jvm-preset').forEach(btn => {
    btn.addEventListener('click', () => setInstanceJvmPreset(btn.dataset.preset));
  });
  modal.querySelector('#instanceJvmArgsInput')?.addEventListener('input', () => {
    const value = modal.querySelector('#instanceJvmArgsInput')?.value || '';
    const preset = typeof getJvmArgsPresetByValue === 'function'
      ? getJvmArgsPresetByValue(value)
      : 'custom';
    localStorage.setItem(instanceSettingKey('jvmPreset'), preset);
    updateInstanceJvmPresetUI();
  });
  return modal;
}

function showInstanceSettingsModal() {
  const modal = ensureInstanceSettingsModal();
  loadInstanceSettings();
  modal.style.display = 'flex';
  modal.classList.remove('hidden');
}

function updateInstanceJavaPathState() {
  const mode = document.getElementById('instanceJavaMode')?.value || 'global';
  const pathInput = document.getElementById('instanceJavaPathInput');
  const useGlobalBtn = document.getElementById('instanceUseGlobalJavaBtn');
  const manual = mode === 'manual';
  if (pathInput) pathInput.disabled = !manual;
  if (useGlobalBtn) useGlobalBtn.disabled = !manual;
}

function loadInstanceSettings() {
  if (!currentDetailInstance) return;
  const memInput = document.getElementById('instanceMemInput');
  const javaMode = document.getElementById('instanceJavaMode');
  const javaPath = document.getElementById('instanceJavaPathInput');
  const jvmArgs = document.getElementById('instanceJvmArgsInput');
  const hint = document.getElementById('instanceSettingsHint');
  const globalMem = parseInt(localStorage.getItem('memAlloc') || '4096') || 4096;
  const memoryMode = localStorage.getItem('memoryMode') || 'manual';
  const autoMemory = typeof getInstanceAutoMemory === 'function'
    ? getInstanceAutoMemory(currentDetailInfo)
    : null;
  const fallbackText = memoryMode === 'auto' && autoMemory
    ? `自动 ${autoMemory.memory} MB（${autoMemory.source}）`
    : `全局手动 ${globalMem} MB`;

  if (memInput) {
    memInput.value = localStorage.getItem(instanceSettingKey('mem')) || '';
    memInput.placeholder = `留空使用${fallbackText}`;
  }
  if (javaMode) javaMode.value = localStorage.getItem(instanceSettingKey('javaMode')) || 'global';
  if (javaPath) javaPath.value = localStorage.getItem(instanceSettingKey('javaPath')) || '';
  if (jvmArgs) jvmArgs.value = localStorage.getItem(instanceSettingKey('jvmArgs')) || '';
  if (hint) {
    hint.textContent = `留空会使用${fallbackText}，其它项跟随全局`;
  }
  updateInstanceSettingsSummary();
  updateInstanceJavaPathState();
  updateInstanceJvmPresetUI();
}

function saveInstanceSettings() {
  if (!currentDetailInstance) return;
  const memInput = document.getElementById('instanceMemInput');
  const javaMode = document.getElementById('instanceJavaMode');
  const javaPath = document.getElementById('instanceJavaPathInput');
  const jvmArgs = document.getElementById('instanceJvmArgsInput');
  const hint = document.getElementById('instanceSettingsHint');

  const memValue = (memInput?.value || '').trim();
  if (memValue) localStorage.setItem(instanceSettingKey('mem'), memValue);
  else localStorage.removeItem(instanceSettingKey('mem'));

  const modeValue = javaMode?.value || 'global';
  if (modeValue === 'global') localStorage.removeItem(instanceSettingKey('javaMode'));
  else localStorage.setItem(instanceSettingKey('javaMode'), modeValue);

  const javaValue = (javaPath?.value || '').trim();
  if (javaValue) localStorage.setItem(instanceSettingKey('javaPath'), javaValue);
  else localStorage.removeItem(instanceSettingKey('javaPath'));

  const jvmValue = (jvmArgs?.value || '').trim();
  const jvmPreset = getInstanceJvmPresetKey();
  if (jvmPreset === 'global') {
    localStorage.removeItem(instanceSettingKey('jvmArgs'));
    localStorage.removeItem(instanceSettingKey('jvmPreset'));
  } else if (jvmPreset === 'clean') {
    localStorage.setItem(instanceSettingKey('jvmArgs'), '');
    localStorage.setItem(instanceSettingKey('jvmPreset'), 'clean');
  } else {
    localStorage.setItem(instanceSettingKey('jvmArgs'), jvmValue);
    const resolvedPreset = typeof getJvmArgsPresetByValue === 'function'
      ? getJvmArgsPresetByValue(jvmValue)
      : 'custom';
    localStorage.setItem(instanceSettingKey('jvmPreset'), resolvedPreset);
  }

  if (hint) {
    hint.textContent = '已保存';
    setTimeout(() => { if (hint) hint.textContent = '留空则使用全局设置'; }, 1800);
  }
  updateInstanceSettingsSummary();
}

function resetInstanceSettings() {
  if (!currentDetailInstance) return;
  ['mem', 'javaMode', 'javaPath', 'jvmArgs', 'jvmPreset'].forEach(prefix => {
    localStorage.removeItem(instanceSettingKey(prefix));
  });
  loadInstanceSettings();
  updateInstanceSettingsSummary();
  const hint = document.getElementById('instanceSettingsHint');
  if (hint) {
    hint.textContent = '已恢复默认';
    setTimeout(() => { if (hint) hint.textContent = '留空则使用全局设置'; }, 1800);
  }
}

function ensureModpackExportModal() {
  let modal = document.getElementById('modpackExportModal');
  if (modal) return modal;

  document.body.insertAdjacentHTML('beforeend', `
    <div class="hidden modpack-export-modal oaoi-modal-host" id="modpackExportModal" data-no-drag>
      <div class="modal-content oaoi-modal-card" data-no-drag>
        <div class="modal-header">
          <button class="modal-close" id="modpackExportClose">&times;</button>
        </div>
        <div class="modal-body">
          <div class="export-top-row">
            <label class="export-format-card active">
              <input type="radio" name="modpackExportFormat" value="modrinth" checked>
              <span>Modrinth 标准包</span>
              <small>.mrpack</small>
            </label>
            <label class="export-format-card">
              <input type="radio" name="modpackExportFormat" value="curseforge">
              <span>CurseForge 标准包</span>
              <small>.zip</small>
            </label>
            <label class="export-name-row">
              <span>名称</span>
              <input type="text" id="modpackExportName" maxlength="80" placeholder="整合包名称">
            </label>
            <label class="export-version-row">
              <span>版本</span>
              <input type="text" id="modpackExportVersion" maxlength="40" placeholder="1.0.0">
            </label>
          </div>
          <div class="export-output-row">
            <span>保存到</span>
            <button type="button" id="modpackExportOutputBtn">选择目录</button>
            <em id="modpackExportOutputPath"></em>
          </div>
          <div class="export-toolbar">
            <span id="modpackExportSummary">扫描中...</span>
            <div>
              <button type="button" id="modpackExportDefault">推荐项</button>
              <button type="button" id="modpackExportAll">全选</button>
            </div>
          </div>
          <div class="export-progress hidden" id="modpackExportProgress">
            <div class="export-progress-head">
              <span id="modpackExportProgressText">准备导出...</span>
              <strong id="modpackExportProgressCount">0%</strong>
            </div>
            <div class="export-progress-track">
              <div class="export-progress-bar" id="modpackExportProgressBar"></div>
            </div>
          </div>
          <div class="export-item-list" id="modpackExportItems">
            <div class="mod-list-empty">正在扫描当前版本文件夹...</div>
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-secondary" id="modpackExportCancel">取消</button>
          <button class="btn btn-primary" id="modpackExportStart">开始导出</button>
        </div>
      </div>
    </div>
  `);
  return document.getElementById('modpackExportModal');
}

function renderModpackExportTaskState() {
  const modal = document.getElementById('modpackExportModal');
  if (!modal) return;
  const progressEl = document.getElementById('modpackExportProgress');
  const progressText = document.getElementById('modpackExportProgressText');
  const progressCount = document.getElementById('modpackExportProgressCount');
  const progressBar = document.getElementById('modpackExportProgressBar');
  const summaryEl = document.getElementById('modpackExportSummary');
  const startBtn = document.getElementById('modpackExportStart');
  const defaultBtn = document.getElementById('modpackExportDefault');
  const allBtn = document.getElementById('modpackExportAll');

  const progress = modpackExportTask.progress || {};
  const total = Number(progress.total || 0);
  const current = Number(progress.current || 0);
  const pct = total > 0 ? Math.max(0, Math.min(100, Math.round((current / total) * 100))) : 0;
  const detail = progress.detail || '正在导出...';

  if (progressEl) progressEl.classList.toggle('hidden', !modpackExportTask.running && !modpackExportTask.result && !modpackExportTask.error);
  if (progressText) progressText.textContent = modpackExportTask.error || detail;
  if (progressCount) progressCount.textContent = modpackExportTask.error ? '失败' : `${pct}%`;
  if (progressBar) progressBar.style.width = modpackExportTask.error ? '100%' : `${pct}%`;
  if (summaryEl && modpackExportTask.running) summaryEl.textContent = `正在导出 ${modpackExportTask.format || ''}`;
  if (startBtn) {
    startBtn.disabled = modpackExportTask.running;
    startBtn.textContent = modpackExportTask.running ? '导出中...' : '开始导出';
  }
  if (defaultBtn) defaultBtn.disabled = modpackExportTask.running;
  if (allBtn) allBtn.disabled = modpackExportTask.running;
}

async function ensureModpackExportProgressListener(tauri) {
  if (modpackExportTask.unlisten || !tauri?.event?.listen) return;
  modpackExportTask.unlisten = await tauri.event.listen('modpack-export-progress', (event) => {
    const payload = event.payload || {};
    if (modpackExportTask.name && payload.name && payload.name !== modpackExportTask.name) return;
    modpackExportTask.progress = {
      stage: payload.stage || '',
      current: Number(payload.current || 0),
      total: Number(payload.total || 0),
      detail: payload.detail || '',
    };
    if (payload.stage === 'done') {
      modpackExportTask.running = false;
    } else if (payload.stage === 'error') {
      modpackExportTask.running = false;
      modpackExportTask.error = payload.detail || '导出失败';
    }
    renderModpackExportTaskState();
  });
}

function showModpackExportModal() {
  if (!currentDetailInstance) return;
  const modal = ensureModpackExportModal();
  const listEl = document.getElementById('modpackExportItems');
  const summaryEl = document.getElementById('modpackExportSummary');
  const startBtn = document.getElementById('modpackExportStart');
  const closeBtn = document.getElementById('modpackExportClose');
  const cancelBtn = document.getElementById('modpackExportCancel');
  const nameInput = document.getElementById('modpackExportName');
  const versionInput = document.getElementById('modpackExportVersion');
  const outputBtn = document.getElementById('modpackExportOutputBtn');
  const outputPathEl = document.getElementById('modpackExportOutputPath');
  let exportItems = [];
  let closed = false;

  const close = () => {
    closed = true;
    modal.remove();
  };
  modal.classList.remove('hidden');
  listEl.innerHTML = '<div class="mod-list-empty">正在扫描当前版本文件夹...</div>';
  summaryEl.textContent = '扫描中...';
  startBtn.disabled = true;
  startBtn.textContent = '开始导出';
  if (nameInput) {
    const defaultName = cleanModpackArchiveName(currentDetailInfo?.name || currentDetailInstance);
    nameInput.value = defaultName || currentDetailInstance;
  }
  if (versionInput && !versionInput.value.trim()) {
    versionInput.value = '1.0.0';
  }
  const gameDir = localStorage.getItem('gameDir') || '';
  const defaultOutputDir = localStorage.getItem('modpackExportOutputDir') || (gameDir ? `${gameDir}\\exports` : '');
  if (outputPathEl) {
    outputPathEl.textContent = defaultOutputDir || '默认 exports';
    outputPathEl.title = defaultOutputDir || '默认 exports';
    modal.dataset.exportOutputDir = defaultOutputDir;
  }
  if (outputBtn) {
    outputBtn.onclick = async () => {
      try {
        const tauri = await waitForTauri();
        const selected = await tauri.dialog.open({
          title: '选择整合包导出位置',
          directory: true,
        });
        if (selected) {
          modal.dataset.exportOutputDir = selected;
          localStorage.setItem('modpackExportOutputDir', selected);
          if (outputPathEl) {
            outputPathEl.textContent = selected;
            outputPathEl.title = selected;
          }
        }
      } catch (err) {
        console.warn('选择导出位置失败:', err);
        showToast('选择导出位置失败: ' + err, 'error');
      }
    };
  }
  if (!modpackExportTask.running) {
    modpackExportTask.result = null;
    modpackExportTask.error = null;
    document.getElementById('modpackExportProgress')?.classList.add('hidden');
  }

  modal.querySelectorAll('.export-format-card').forEach(card => {
    const input = card.querySelector('input');
    card.classList.toggle('active', input.checked);
    card.onclick = () => {
      input.checked = true;
      modal.querySelectorAll('.export-format-card').forEach(item => item.classList.remove('active'));
      card.classList.add('active');
    };
  });

  closeBtn.onclick = close;
  cancelBtn.onclick = close;
  modal.onclick = (e) => {
    if (e.target === modal) close();
  };

  if (modpackExportTask.running && modpackExportTask.name === currentDetailInstance) {
    listEl.innerHTML = '<div class="mod-list-empty">导出正在后台进行，关闭窗口不会取消任务。</div>';
    renderModpackExportTaskState();
    return;
  }

  const refreshSummary = () => {
    const checkedCount = listEl.querySelectorAll('input[type="checkbox"]:checked').length;
    const totalCount = exportItems.length;
    summaryEl.textContent = totalCount ? `已选 ${checkedCount}/${totalCount}` : '没有可导出的文件';
  };

  const requestedInstance = currentDetailInstance;
  const loadToken = `${requestedInstance}:${Date.now()}`;
  modal.dataset.exportLoadToken = loadToken;
  setTimeout(async () => {
    try {
    const tauri = await waitForTauri();
    await ensureModpackExportProgressListener(tauri);
    exportItems = await tauri.core.invoke('get_modpack_export_items', {
      gameDir,
      name: requestedInstance,
    });
    if (closed) return;
    if (modal.dataset.exportLoadToken !== loadToken || currentDetailInstance !== requestedInstance) return;

    if (!exportItems.length) {
      listEl.innerHTML = '<div class="mod-list-empty">这个版本空得很安静，没找到可导出的东西</div>';
      summaryEl.textContent = '没有可导出的文件';
      return;
    }

    listEl.innerHTML = exportItems.map(item => `
      <label class="export-item-row">
        <input type="checkbox" value="${escapeHtml(item.path)}" ${item.defaultChecked ? 'checked' : ''}>
        <span class="export-item-main">
          <strong>${escapeHtml(item.label || item.path)}</strong>
          <em>${escapeHtml(item.path)}</em>
        </span>
        <span class="export-item-meta">${item.kind === 'folder' ? `${item.count} 项` : '文件'} · ${formatFileSize(item.size || 0)}</span>
      </label>
    `).join('');

    listEl.querySelectorAll('input[type="checkbox"]').forEach(input => {
      input.addEventListener('change', refreshSummary);
    });
    document.getElementById('modpackExportDefault').onclick = () => {
      listEl.querySelectorAll('input[type="checkbox"]').forEach((input, idx) => {
        input.checked = !!exportItems[idx]?.defaultChecked;
      });
      refreshSummary();
    };
    document.getElementById('modpackExportAll').onclick = () => {
      const boxes = [...listEl.querySelectorAll('input[type="checkbox"]')];
      const shouldCheck = boxes.some(input => !input.checked);
      boxes.forEach(input => input.checked = shouldCheck);
      refreshSummary();
    };
    startBtn.disabled = false;
    refreshSummary();

    startBtn.onclick = async () => {
      const includePaths = [...listEl.querySelectorAll('input[type="checkbox"]:checked')]
        .map(input => input.value);
      if (!includePaths.length) {
        showToast('至少选一个文件夹或文件，不然导出空气了', 'warn');
        return;
      }
      const format = modal.querySelector('input[name="modpackExportFormat"]:checked')?.value || 'modrinth';
      const exportName = cleanModpackArchiveName(nameInput?.value || currentDetailInstance);
      const exportVersion = (versionInput?.value || '1.0.0').trim() || '1.0.0';
      if (!exportName) {
        showToast('整合包名称不能为空', 'warn');
        nameInput?.focus();
        return;
      }
      modpackExportTask.running = true;
      modpackExportTask.name = currentDetailInstance;
      modpackExportTask.format = format === 'curseforge' ? 'CurseForge' : 'Modrinth';
      modpackExportTask.result = null;
      modpackExportTask.error = null;
      modpackExportTask.progress = { stage: 'prepare', current: 0, total: 100, detail: 'Preparing export...' };
      startBtn.disabled = true;
      startBtn.textContent = '导出中...';
      renderModpackExportTaskState();
      try {
        const result = await tauri.core.invoke('export_modpack', {
          gameDir,
          name: currentDetailInstance,
          format,
          exportName,
          exportVersion,
          outputDir: modal.dataset.exportOutputDir || null,
          includePaths,
        });
        modpackExportTask.running = false;
        modpackExportTask.result = result;
        modpackExportTask.progress = { stage: 'done', current: 100, total: 100, detail: 'Export complete' };
        renderModpackExportTaskState();
        const bundled = result.bundledFiles ? `，${result.bundledFiles} 个文件已内置打包` : '';
        const warnings = Array.isArray(result.warnings) && result.warnings.length
          ? `；${result.warnings.join('；')}`
          : '';
        showToast(`导出完成：${result.path}${bundled}${warnings}`, 'success', 10000);
        close();
      } catch (err) {
        modpackExportTask.running = false;
        modpackExportTask.error = String(err);
        modpackExportTask.progress = { stage: 'error', current: 0, total: 0, detail: String(err) };
        renderModpackExportTaskState();
        showToast('导出失败: ' + err, 'error', 10000);
        startBtn.disabled = false;
        startBtn.textContent = '开始导出';
      }
    };
    } catch (err) {
    if (closed) return;
    listEl.innerHTML = `<div class="mod-list-empty">扫描失败: ${escapeHtml(err)}</div>`;
    summaryEl.textContent = '扫描失败';
    }
  }, 250);
}

function formatAnalyzeIssues(items, formatter, limit = 8) {
  if (!Array.isArray(items) || items.length === 0) return '';
  const shown = items.slice(0, limit).map(formatter);
  const remain = items.length > limit ? `\n还有 ${items.length - limit} 项未显示` : '';
  return `${shown.join('\n')}${remain}`;
}

function formatAnalyzeModFileName(file) {
  return String(file || '')
    .split(/[\\/]/)
    .pop()
    .replace(/\.jar$/i, '');
}

function formatInstanceAnalyzeResult(result) {
  const scanned = Number(result?.scannedFiles || 0);
  const parsed = Number(result?.parsedMods || 0);
  const issueCount = Number(result?.issueCount || 0);
  const duplicates = result?.duplicates || [];
  const missing = result?.missingDependencies || [];
  const mismatches = result?.loaderMismatches || [];
  const warnings = result?.warnings || [];

  const parts = [
    `扫描文件：${scanned}`,
    `识别 Mod：${parsed}`,
    `发现问题：${issueCount}`,
  ];

  if (!issueCount) {
    parts.push('', '没有发现明显的重复 Mod、缺失前置或 Loader 不匹配。');
    return parts.join('\n');
  }

  if (duplicates.length) {
    parts.push('', `重复 Mod：${duplicates.length}`);
    parts.push(formatAnalyzeIssues(duplicates, item => {
      return (item.files || []).map(file => formatAnalyzeModFileName(file.file)).join('\n');
    }));
  }

  if (missing.length) {
    parts.push('', `缺少前置：${missing.length}`);
    parts.push(formatAnalyzeIssues(missing, item => {
      const req = item.versionReq ? ` ${item.versionReq}` : '';
      return `- ${item.modName || item.modId} 缺少 ${item.dependencyId}${req}`;
    }));
  }

  if (mismatches.length) {
    parts.push('', `运行环境不匹配：${mismatches.length}`);
    parts.push(formatAnalyzeIssues(mismatches, item => {
      return `- ${item.modName || item.modId}: ${item.modLoader} 不能用于 ${item.instanceLoader}`;
    }));
  }

  if (warnings.length) {
    parts.push('', `无法识别/读取：${warnings.length}`);
    parts.push(formatAnalyzeIssues(warnings, item => `- ${item.file}: ${item.message}`, 6));
  }

  return parts.filter(part => part !== '').join('\n');
}

async function analyzeCurrentInstanceMods() {
  if (!currentDetailInstance) return;
  const btn = document.getElementById('instanceAnalyzeBtn');
  const originalText = btn?.textContent || '一键检测';
  if (btn) {
    btn.disabled = true;
    btn.textContent = '检测中...';
  }
  try {
    const tauri = await waitForTauri();
    const gameDir = localStorage.getItem('gameDir') || '';
    const result = await tauri.core.invoke('analyze_instance_mods', {
      gameDir,
      name: currentDetailInstance,
      mcVersion: currentDetailInfo?.mc_version || '',
      loader: currentDetailInfo?.loader_type || '',
    });
    await showAlert(formatInstanceAnalyzeResult(result), {
      title: '一键检测结果',
      confirmText: '我知道了',
    });
  } catch (err) {
    await showAlert(`检测失败：${err}`, {
      title: '一键检测失败',
      confirmText: '我知道了',
    });
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = originalText;
    }
  }
}

// Tab 切换
function switchModTab(tab) {
  document.querySelectorAll('.mod-tab').forEach(t => t.classList.toggle('active', t.dataset.tab === tab));
  document.getElementById('modTabInstalledContent')?.classList.toggle('active', tab === 'installed');
  document.getElementById('modTabOnlineContent')?.classList.toggle('active', tab === 'online');
  document.getElementById('modTabUpdatesContent')?.classList.toggle('active', tab === 'updates');
  // 显示/隐藏类别按钮
  document.getElementById('onlineCategoryTabs')?.classList.toggle('visible', tab === 'online');
  if (tab === 'updates' && currentDetailInstance) {
    if (modUpdateCheckedKey !== getModUpdateCheckKey()) {
      loadModUpdateCache({ warm: true });
    }
  }
  // 切到在线搜索时自动加载热门
  if (tab === 'online') {
    const listEl = document.getElementById('onlineModList');
    const cacheKey = `all:${currentDetailInfo?.loader_type || ''}:${currentOnlineCategory}:`;
    if (listEl && listEl.querySelector('.mod-list-empty') && !onlineSearchCache[cacheKey]) {
      searchOnlineMods();
    }
  }
}

function afterNextPaint() {
  return new Promise(resolve => requestAnimationFrame(() => setTimeout(resolve, 0)));
}

// 加载已安装 mod 列表
async function loadModList(instanceName = currentDetailInstance) {
  if (!instanceName) return;
  const listEl = document.getElementById('modList');
  const countEl = document.getElementById('modCount');
  const loadSeq = ++modListLoadSeq;
  const requestId = `${instanceName}|${loadSeq}|${Date.now()}`;
  modListStreamRequestId = requestId;
  currentModList = [];
  if (listEl) listEl.innerHTML = '<div class="mod-list-empty">加载中...</div>';
  if (countEl) countEl.textContent = '0';
  try {
    await afterNextPaint();
    if (loadSeq !== modListLoadSeq || currentDetailInstance !== instanceName) return;
    const tauri = await waitForTauri();
    await bindModListStreamEvents();
    const gameDir = localStorage.getItem('gameDir') || '';
    tauri.core.invoke('stream_mods', { gameDir, name: instanceName, requestId })
      .catch(err => loadModListFallback(instanceName, loadSeq, err));
  } catch (err) {
    console.warn('加载 Mod 列表失败:', err);
    if (listEl) listEl.innerHTML = '<div class="mod-list-empty">加载失败</div>';
  }
}

async function loadModListFallback(instanceName, loadSeq, reason) {
  if (loadSeq !== modListLoadSeq || currentDetailInstance !== instanceName) return;
  console.warn('流式加载 Mod 列表失败，回退到普通列表:', reason);
  const listEl = document.getElementById('modList');
  const countEl = document.getElementById('modCount');
  try {
    const tauri = await waitForTauri();
    const gameDir = localStorage.getItem('gameDir') || '';
    const mods = await tauri.core.invoke('list_mods', { gameDir, name: instanceName });
    if (loadSeq !== modListLoadSeq || currentDetailInstance !== instanceName) return;
    currentModList = mods;
    if (countEl) countEl.textContent = mods.length;
    renderModList(mods, { final: true });
  } catch (err) {
    console.warn('加载 Mod 列表失败:', err);
    if (listEl) listEl.innerHTML = '<div class="mod-list-empty">加载失败</div>';
  }
}

async function bindModListStreamEvents() {
  if (modListStreamEventBound) return;
  const tauri = await waitForTauri();
  await tauri.event.listen('mod-list-stream', (event) => {
    const payload = event.payload || {};
    if (payload.request_id !== modListStreamRequestId) return;
    if (payload.name !== currentDetailInstance) return;
    const countEl = document.getElementById('modCount');
    if (payload.status === 'batch') {
      const incoming = Array.isArray(payload.mods) ? payload.mods : [];
      const seen = new Set(currentModList.map(item => item.file_name));
      incoming.forEach(item => {
        if (!item?.file_name || seen.has(item.file_name)) return;
        currentModList.push(item);
        seen.add(item.file_name);
      });
      if (countEl) countEl.textContent = String(currentModList.length);
      renderModList(currentModList, { final: false });
      return;
    }
    if (payload.status === 'icon') {
      applyModIconPatches(payload.icons);
      return;
    }
    if (payload.status === 'done') {
      if (countEl) countEl.textContent = String(currentModList.length);
      renderModList(currentModList, { final: true });
    }
  });
  modListStreamEventBound = true;
}

function modActionId(fileName) {
  return String(fileName || '').replace(/[^a-zA-Z0-9]/g, '_');
}

function renderModIcon(iconUrl, className, id = '') {
  const idAttr = id ? ` id="${escapeHtml(id)}"` : '';
  if (iconUrl) {
    return `<img${idAttr} class="${className}" src="${escapeHtml(iconUrl)}" alt="">`;
  }
  return `<span${idAttr} class="${className} mod-icon-placeholder"></span>`;
}

function getInstalledModIcon(fileName) {
  const normalized = String(fileName || '').replace(/\.disabled$/i, '');
  const mod = currentModList.find(item => {
    const itemName = String(item.file_name || '');
    return itemName === fileName || itemName.replace(/\.disabled$/i, '') === normalized;
  });
  return mod?.icon_url || '';
}

function applyModIconPatches(icons) {
  if (!Array.isArray(icons) || !icons.length) return;
  let changed = false;
  for (const patch of icons) {
    const fileName = patch?.file_name || '';
    const iconUrl = patch?.icon_url || '';
    if (!fileName || !iconUrl) continue;
    const mod = currentModList.find(item => item.file_name === fileName);
    if (!mod) continue;
    mod.icon_url = iconUrl;
    changed = true;
    const iconEl = document.getElementById(`mod-icon-${modActionId(fileName)}`);
    if (iconEl) {
      iconEl.outerHTML = renderModIcon(iconUrl, 'mod-icon', `mod-icon-${modActionId(fileName)}`);
    }
  }
  if (changed && currentModUpdateList.length) {
    renderModUpdateList(currentModUpdateList);
    icons.forEach(patch => patchModBulkIcon(patch?.file_name || '', patch?.icon_url || ''));
  }
}

function patchModBulkIcon(fileName, iconUrl) {
  if (!fileName || !iconUrl || !modBulkUpdateModal || modBulkUpdateModal.classList.contains('hidden')) return;
  modBulkUpdateModal.querySelectorAll('.mod-bulk-update-item').forEach(item => {
    if (item.dataset.file !== fileName) return;
    const iconEl = item.querySelector('.mod-bulk-update-icon');
    if (iconEl) {
      iconEl.outerHTML = renderModIcon(iconUrl, 'mod-bulk-update-icon');
    }
  });
}

function renderModItem(mod) {
  const fileName = mod.file_name || '';
  const safeFileName = escapeHtml(fileName);
  const baseName = fileName.replace(/\.jar\.disabled$/i, '').replace(/\.jar$/i, '');
  const displayName = mod.cn_name ? `${mod.cn_name} (${baseName})` : baseName;
  const actionId = modActionId(fileName);
  return `
    <div class="mod-item ${mod.enabled ? '' : 'disabled'}" data-file="${safeFileName}">
      <button class="mod-toggle ${mod.enabled ? 'active' : ''}" data-file="${safeFileName}" title="${mod.enabled ? '点击禁用' : '点击启用'}"></button>
      ${renderModIcon(mod.icon_url || '', 'mod-icon', `mod-icon-${actionId}`)}
      <span class="mod-name" title="${safeFileName}">${escapeHtml(displayName)}</span>
      <span class="mod-actions" id="mod-actions-${actionId}">
        <button class="mod-delete-btn" data-file="${safeFileName}" title="删除">🗑</button>
      </span>
      <span class="mod-size">${mod.size_kb > 1024 ? (mod.size_kb / 1024).toFixed(1) + ' MB' : mod.size_kb + ' KB'}</span>
    </div>
  `;
}

// 渲染已安装 mod 列表
function renderModList(mods, options = {}) {
  const listEl = document.getElementById('modList');
  if (!listEl) return;
  const { final = true } = options;
  const renderSeq = ++modListRenderSeq;

  const searchVal = (document.getElementById('modSearchInput')?.value || '').toLowerCase();
  const filtered = searchVal
    ? mods.filter(m => m.file_name.toLowerCase().includes(searchVal) || (m.cn_name && m.cn_name.toLowerCase().includes(searchVal)))
    : mods;

  if (filtered.length === 0) {
    listEl.innerHTML = `<div class="mod-list-empty">${mods.length === 0 ? '暂无 Mod' : '无匹配结果'}</div>`;
    if (final && modUpdateCheckedKey !== getModUpdateCheckKey()) {
      loadModUpdateCache({ warm: true });
    }
    return;
  }

  listEl.innerHTML = '';
  const chunkSize = 80;
  let index = 0;

  const renderChunk = () => {
    if (renderSeq !== modListRenderSeq) return;
    const html = filtered.slice(index, index + chunkSize).map(renderModItem).join('');
    listEl.insertAdjacentHTML('beforeend', html);
    index += chunkSize;
    if (index < filtered.length) {
      setTimeout(renderChunk, 0);
    } else {
      applyModSourceLinks(currentModSourceLinks);
      if (final && modUpdateCheckedKey !== getModUpdateCheckKey()) {
        loadModUpdateCache({ warm: true });
      }
    }
  };
  renderChunk();
}

function getModUpdateCheckKey() {
  return `${currentDetailInstance || ''}|${currentDetailInfo?.mc_version || ''}|${currentDetailInfo?.loader_type || ''}`;
}

function getModUpdateArgs() {
  return {
    gameDir: localStorage.getItem('gameDir') || '',
    name: currentDetailInstance,
    mcVersion: currentDetailInfo?.mc_version || '',
    loader: currentDetailInfo?.loader_type || '',
  };
}

// 更新检测结果跟实例、MC 版本和 Loader 绑定，避免切换页面后显示旧结果。
function resetModUpdateState(statusText = '只显示需要更新的 Mod') {
  modUpdateSeq++;
  modUpdateCheckedKey = '';
  modUpdateLoadingKey = '';
  currentModUpdateList = [];
  currentModSourceLinks = [];
  currentOldModBackups = [];
  currentModRollbackRecords = [];
  modUpdateReady = false;
  const countEl = document.getElementById('modUpdateCount');
  const statusEl = document.getElementById('modUpdateStatus');
  const listEl = document.getElementById('modUpdateList');
  if (countEl) countEl.textContent = '0';
  if (statusEl) statusEl.textContent = statusText;
  if (listEl) listEl.innerHTML = `<div class="mod-list-empty">${escapeHtml(statusText)}</div>`;
}

async function loadModUpdateCache(options = {}) {
  if (!currentDetailInstance) return;
  const { warm = false } = options;
  const checkKey = getModUpdateCheckKey();
  const seq = modUpdateSeq;
  const loadingKey = `${checkKey}|${seq}`;
  if (modUpdateLoadingKey === loadingKey) return;
  modUpdateLoadingKey = loadingKey;
  try {
    const tauri = await waitForTauri();
    const view = await tauri.core.invoke('get_mod_update_cache', getModUpdateArgs());
    if (seq !== modUpdateSeq || checkKey !== getModUpdateCheckKey()) return;
    let nextView = view;
    if (warm && view?.stale) {
      const refreshing = view.refreshing || await warmModUpdateCache();
      nextView = { ...view, refreshing };
    }
    applyModUpdateCacheView(nextView);
  } catch (err) {
    if (seq !== modUpdateSeq) return;
    const statusEl = document.getElementById('modUpdateStatus');
    const listEl = document.getElementById('modUpdateList');
    if (statusEl) statusEl.textContent = '读取更新缓存失败';
    if (listEl) listEl.innerHTML = `<div class="mod-list-empty">读取缓存失败: ${escapeHtml(err)}</div>`;
  } finally {
    if (modUpdateLoadingKey === loadingKey) {
      modUpdateLoadingKey = '';
    }
  }
}

// 预热只启动后台任务，列表仍然优先显示已有缓存，避免页面空等。
async function warmModUpdateCache() {
  if (!currentDetailInstance) return;
  try {
    const tauri = await waitForTauri();
    await tauri.core.invoke('warm_mod_update_cache', {
      ...getModUpdateArgs(),
      forceUpdate: false,
    });
    return true;
  } catch (err) {
    console.warn('预热 Mod 更新缓存失败:', err);
    return false;
  }
}

// 缓存结果和后台刷新结果共用这一处渲染，防止状态文案互相打架。
function applyModUpdateCacheView(view, options = {}) {
  if (!view || view.name !== currentDetailInstance) return;
  if (view.mcVersion !== (currentDetailInfo?.mc_version || '')) return;
  if (view.loader !== (currentDetailInfo?.loader_type || '')) return;

  currentModUpdateList = Array.isArray(view.updates) ? view.updates : [];
  currentModSourceLinks = Array.isArray(view.links) ? view.links : [];
  modUpdateReady = !view.refreshing && !view.stale;
  modUpdateCheckedKey = getModUpdateCheckKey();
  applyModSourceLinks(currentModSourceLinks);
  const statusEl = document.getElementById('modUpdateStatus');
  const emptyText = view.refreshing
    ? '正在后台检测更新...'
    : (view.stale ? '缓存已过期，正在后台刷新' : '暂无需要更新的 Mod');
  renderModUpdateList(currentModUpdateList, emptyText);

  if (statusEl) {
    const checked = view.checkedAt ? `，上次 ${formatModUpdateTime(view.checkedAt)}` : '';
    if (view.refreshing) {
      statusEl.textContent = currentModUpdateList.length
        ? `先显示缓存结果${checked}，后台刷新中...`
        : '正在后台检测 Mod 更新...';
    } else if (currentModUpdateList.length) {
      statusEl.textContent = `${view.stale ? '缓存结果' : '发现'} ${currentModUpdateList.length} 个需要更新的 Mod${checked}`;
    } else if (view.stale) {
      statusEl.textContent = options.fromEvent ? '暂无需要更新的 Mod' : '缓存已过期，正在后台检测...';
    } else {
      statusEl.textContent = `暂无需要更新的 Mod${checked}`;
    }
  }
}

function formatModUpdateTime(seconds) {
  const date = new Date(Number(seconds) * 1000);
  if (Number.isNaN(date.getTime())) return '';
  return date.toLocaleString('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

// 后端刷新完成会推事件回来；只有当前实例匹配时才更新 UI。
async function bindModUpdateCacheEvents() {
  if (modUpdateCacheEventBound) return;
  modUpdateCacheEventBound = true;
  try {
    const tauri = await waitForTauri();
    await tauri.event.listen('mod-update-cache', (event) => {
      const payload = event.payload || {};
      if (payload.name !== currentDetailInstance) return;
      if (payload.mcVersion !== (currentDetailInfo?.mc_version || '')) return;
      if (payload.loader !== (currentDetailInfo?.loader_type || '')) return;
      if ((payload.status === 'ready' || payload.status === 'partial') && payload.view) {
        applyModUpdateCacheView(payload.view, { fromEvent: true });
        return;
      }
      const statusEl = document.getElementById('modUpdateStatus');
      if (statusEl) {
        const hasOldCache = currentModUpdateList.length || currentModSourceLinks.length;
        statusEl.textContent = hasOldCache
          ? `后台检测失败，继续使用旧缓存：${payload.message || '未知错误'}`
          : `后台检测失败：${payload.message || '未知错误'}`;
      }
    });
  } catch (err) {
    console.warn('监听 Mod 更新缓存事件失败:', err);
  }
}

function renderModUpdateList(updates, emptyText = '暂无需要更新的 Mod') {
  const listEl = document.getElementById('modUpdateList');
  const countEl = document.getElementById('modUpdateCount');
  const bulkBtn = document.getElementById('modBulkUpdateBtn');
  if (countEl) countEl.textContent = String(updates.length);
  if (bulkBtn) bulkBtn.disabled = !currentDetailInstance || !modUpdateReady || updates.length === 0;
  if (!listEl) return;
  if (!updates.length) {
    listEl.innerHTML = `<div class="mod-list-empty">${escapeHtml(emptyText)}</div>`;
    return;
  }

  listEl.innerHTML = updates.map(update => {
    const iconHtml = renderModIcon(
      getInstalledModIcon(update.fileName || ''),
      'mod-update-icon'
    );
    const linksHtml = [
      update.mrUrl ? `<a href="#" class="mod-link mr" data-url="${escapeHtml(update.mrUrl)}" title="Modrinth">MR</a>` : '',
      update.cfUrl ? `<a href="#" class="mod-link cf" data-url="${escapeHtml(update.cfUrl)}" title="CurseForge">CF</a>` : '',
    ].filter(Boolean).join('');
    return `
      <div class="mod-update-item">
        ${iconHtml}
        <div class="mod-update-main">
          <div class="mod-update-file" title="${escapeHtml(update.fileName || '')}">
            当前版本：${escapeHtml(update.fileName || '')}
          </div>
          <div class="mod-update-file" title="${escapeHtml(update.latestFileName || '')}">
            最新版：${escapeHtml(update.latestFileName || '')}
          </div>
        </div>
        <div class="mod-update-meta">
          ${linksHtml ? `<div class="mod-update-links">${linksHtml}</div>` : ''}
        </div>
      </div>
    `;
  }).join('');
}

function ensureModBulkUpdateModal() {
  if (modBulkUpdateModal) return modBulkUpdateModal;
  const modal = document.createElement('div');
  modal.id = 'modBulkUpdateModal';
  modal.className = 'hidden mod-bulk-update-modal oaoi-modal-host oaoi-modal-compact';
  modal.innerHTML = `
    <div class="modal oaoi-modal-card" data-no-drag>
      <div class="modal-header">
        <h3>一键更新 Mod</h3>
        <button class="modal-close" id="modBulkUpdateClose" type="button">&times;</button>
      </div>
      <div class="modal-body mod-bulk-update-body">
        <div class="mod-bulk-tabs" role="tablist" aria-label="Mod 更新操作">
          <button class="mod-bulk-tab active" type="button" data-mod-bulk-tab="updates">
            待更新 <span id="modBulkUpdateTabCount">0</span>
          </button>
          <button class="mod-bulk-tab" type="button" data-mod-bulk-tab="rollback">
            可回档 <span id="modBulkRollbackTabCount">0</span>
          </button>
          <button class="mod-bulk-tab" type="button" data-mod-bulk-tab="old">
            旧版 <span id="modBulkOldTabCount">0</span>
          </button>
        </div>
        <div class="mod-bulk-panel active" data-mod-bulk-panel="updates">
          <div class="mod-bulk-update-head">
            <label class="mod-bulk-check-all">
              <input type="checkbox" id="modBulkUpdateSelectAll" checked>
              <span>全选</span>
            </label>
            <span id="modBulkUpdateSummary">已选择 0 个</span>
          </div>
          <div class="mod-bulk-update-list" id="modBulkUpdateList"></div>
        </div>
        <div class="mod-bulk-panel" data-mod-bulk-panel="rollback">
          <div class="mod-bulk-maintenance-list mod-bulk-rollback-list" id="modBulkRollbackList">
            <div class="mod-list-empty">正在读取回档记录...</div>
          </div>
        </div>
        <div class="mod-bulk-panel" data-mod-bulk-panel="old">
          <div class="mod-bulk-maintenance-list mod-bulk-old-list" id="modBulkOldList">
            <div class="mod-list-empty">正在读取旧版列表...</div>
          </div>
        </div>
        <div class="mod-bulk-update-status" id="modBulkUpdateStatus"></div>
      </div>
      <div class="modal-footer">
        <button class="btn btn-secondary" id="modBulkUpdateCancel">取消</button>
        <button class="btn btn-secondary hidden" id="modBulkRollback" type="button">一键回档</button>
        <button class="btn btn-secondary hidden" id="modBulkDeleteOld" type="button">删除旧版</button>
        <button class="btn btn-primary" id="modBulkUpdateStart">开始更新</button>
      </div>
    </div>
  `;
  document.body.appendChild(modal);
  modBulkUpdateModal = modal;

  const close = () => {
    if (modal.dataset.running === 'true') return;
    modal.classList.add('hidden');
    modal.style.display = 'none';
  };
  modal.querySelector('#modBulkUpdateClose')?.addEventListener('click', close);
  modal.querySelector('#modBulkUpdateCancel')?.addEventListener('click', close);
  modal.addEventListener('click', (e) => {
    if (e.target === modal) close();
  });
  modal.querySelector('#modBulkUpdateSelectAll')?.addEventListener('change', (e) => {
    modal.querySelectorAll('.mod-bulk-update-check').forEach(input => {
      input.checked = e.target.checked;
    });
    updateModBulkSelectionSummary(modal);
  });
  modal.querySelector('#modBulkUpdateList')?.addEventListener('change', (e) => {
    if (!e.target?.classList?.contains('mod-bulk-update-check')) return;
    syncModBulkSelectAll(modal);
    updateModBulkSelectionSummary(modal);
  });
  modal.querySelector('.mod-bulk-tabs')?.addEventListener('click', (e) => {
    const tab = e.target?.closest?.('[data-mod-bulk-tab]');
    if (!tab || !modal.contains(tab)) return;
    switchModBulkTab(modal, tab.dataset.modBulkTab || 'updates');
  });
  modal.querySelector('#modBulkUpdateStart')?.addEventListener('click', () => runSelectedModUpdates(modal));
  modal.querySelector('#modBulkDeleteOld')?.addEventListener('click', () => deleteOldModBackups(modal));
  modal.querySelector('#modBulkRollback')?.addEventListener('click', () => rollbackModUpdates(modal));
  return modal;
}

function showModBulkUpdateModal() {
  const modal = ensureModBulkUpdateModal();
  modal.dataset.running = 'false';
  switchModBulkTab(modal, 'updates');
  renderModBulkUpdateChoices(modal);
  refreshModRollbackRecords(modal);
  refreshOldModBackups(modal);
  modal.style.display = 'flex';
  modal.classList.remove('hidden');
}

function renderModBulkUpdateChoices(modal) {
  const listEl = modal.querySelector('#modBulkUpdateList');
  const statusEl = modal.querySelector('#modBulkUpdateStatus');
  const startBtn = modal.querySelector('#modBulkUpdateStart');
  const selectAll = modal.querySelector('#modBulkUpdateSelectAll');
  if (statusEl) statusEl.textContent = '';
  if (startBtn) {
    startBtn.disabled = !modUpdateReady || currentModUpdateList.length === 0;
    startBtn.textContent = '开始更新';
  }
  if (selectAll) selectAll.checked = true;
  if (!listEl) return;
  if (!currentModUpdateList.length) {
    listEl.innerHTML = '<div class="mod-list-empty">暂无需要更新的 Mod</div>';
    updateModBulkSelectionSummary(modal);
    return;
  }
  listEl.innerHTML = currentModUpdateList.map(update => `
    <label class="mod-bulk-update-item" data-file="${escapeHtml(update.fileName || '')}">
      <input class="mod-bulk-update-check" type="checkbox" value="${escapeHtml(update.fileName || '')}" checked>
      ${renderModIcon(getInstalledModIcon(update.fileName || ''), 'mod-bulk-update-icon')}
      <span class="mod-bulk-update-lines">
        <span title="${escapeHtml(update.fileName || '')}">当前版本：${escapeHtml(update.fileName || '')}</span>
        <span title="${escapeHtml(update.latestFileName || '')}">最新版：${escapeHtml(update.latestFileName || '')}</span>
      </span>
    </label>
  `).join('');
  updateModBulkSelectionSummary(modal);
  syncModBulkTabState(modal);
}

function selectedModUpdateFileNames(modal) {
  return Array.from(modal.querySelectorAll('.mod-bulk-update-check:checked'))
    .map(input => input.value)
    .filter(Boolean);
}

function updateModBulkSelectionSummary(modal) {
  const summary = modal.querySelector('#modBulkUpdateSummary');
  const total = modal.querySelectorAll('.mod-bulk-update-check').length;
  const selected = selectedModUpdateFileNames(modal).length;
  if (summary) summary.textContent = `已选择 ${selected} / ${total} 个`;
  const startBtn = modal.querySelector('#modBulkUpdateStart');
  if (startBtn && modal.dataset.running !== 'true') startBtn.disabled = selected === 0;
  syncModBulkTabState(modal);
}

function switchModBulkTab(modal, tabName) {
  const allowed = ['updates', 'rollback', 'old'];
  const nextTab = allowed.includes(tabName) ? tabName : 'updates';
  currentModBulkTab = nextTab;
  if (modal) modal.dataset.activeTab = nextTab;
  modal?.querySelectorAll('[data-mod-bulk-tab]').forEach(tab => {
    tab.classList.toggle('active', tab.dataset.modBulkTab === nextTab);
  });
  modal?.querySelectorAll('[data-mod-bulk-panel]').forEach(panel => {
    panel.classList.toggle('active', panel.dataset.modBulkPanel === nextTab);
  });
  syncModBulkTabState(modal);
}

function syncModBulkTabState(modal) {
  if (!modal) return;
  const tabName = modal.dataset.activeTab || currentModBulkTab || 'updates';
  const running = modal.dataset.running === 'true';
  const updateCount = currentModUpdateList.length;
  const rollbackCount = currentModRollbackRecords.length;
  const oldCount = currentOldModBackups.length;
  const updateTabCount = modal.querySelector('#modBulkUpdateTabCount');
  const rollbackTabCount = modal.querySelector('#modBulkRollbackTabCount');
  const oldTabCount = modal.querySelector('#modBulkOldTabCount');
  if (updateTabCount) updateTabCount.textContent = String(updateCount);
  if (rollbackTabCount) rollbackTabCount.textContent = String(rollbackCount);
  if (oldTabCount) oldTabCount.textContent = String(oldCount);

  const startBtn = modal.querySelector('#modBulkUpdateStart');
  const rollbackBtn = modal.querySelector('#modBulkRollback');
  const deleteBtn = modal.querySelector('#modBulkDeleteOld');
  const selected = selectedModUpdateFileNames(modal).length;
  if (startBtn) {
    startBtn.classList.toggle('hidden', tabName !== 'updates');
    if (!running) startBtn.disabled = !modUpdateReady || selected === 0;
  }
  if (rollbackBtn) {
    rollbackBtn.classList.toggle('hidden', tabName !== 'rollback');
    if (!running) rollbackBtn.disabled = rollbackCount === 0;
  }
  if (deleteBtn) {
    deleteBtn.classList.toggle('hidden', tabName !== 'old');
    if (!running) deleteBtn.disabled = oldCount === 0;
  }
}

function syncModBulkSelectAll(modal) {
  const selectAll = modal.querySelector('#modBulkUpdateSelectAll');
  if (!selectAll) return;
  const checks = Array.from(modal.querySelectorAll('.mod-bulk-update-check'));
  selectAll.checked = checks.length > 0 && checks.every(input => input.checked);
}

async function refreshModRollbackRecords(modal) {
  const listEl = modal.querySelector('#modBulkRollbackList');
  if (listEl) listEl.innerHTML = '<div class="mod-list-empty">正在读取回档记录...</div>';
  try {
    const tauri = await waitForTauri();
    const records = await tauri.core.invoke('list_mod_update_rollbacks', {
      gameDir: localStorage.getItem('gameDir') || '',
      name: currentDetailInstance,
    });
    renderModRollbackRecords(modal, records);
  } catch (err) {
    currentModRollbackRecords = [];
    const rollbackBtn = modal.querySelector('#modBulkRollback');
    if (rollbackBtn) rollbackBtn.disabled = true;
    if (listEl) listEl.innerHTML = `<div class="mod-list-empty">读取回档记录失败: ${escapeHtml(err)}</div>`;
    syncModBulkTabState(modal);
  }
}

function renderModRollbackRecords(modal, records) {
  currentModRollbackRecords = Array.isArray(records) ? records : [];
  const listEl = modal.querySelector('#modBulkRollbackList');
  const rollbackBtn = modal.querySelector('#modBulkRollback');
  if (rollbackBtn) rollbackBtn.disabled = currentModRollbackRecords.length === 0 || modal.dataset.running === 'true';
  if (!listEl) return;
  if (!currentModRollbackRecords.length) {
    listEl.innerHTML = '<div class="mod-list-empty">暂无可回档 Mod</div>';
    syncModBulkTabState(modal);
    return;
  }
  listEl.innerHTML = currentModRollbackRecords.map(record => `
    <div class="mod-bulk-rollback-item" title="${escapeHtml(record.newFileName || '')}">
      <span>${escapeHtml(record.displayName || record.newFileName || '')}</span>
      <small>${escapeHtml(record.newFileName || '')} → ${escapeHtml(record.oldFileName || '')}</small>
    </div>
  `).join('');
  syncModBulkTabState(modal);
}

async function rollbackModUpdates(modal) {
  if (!currentModRollbackRecords.length || modal.dataset.running === 'true') return;
  const rollbackBtn = modal.querySelector('#modBulkRollback');
  const statusEl = modal.querySelector('#modBulkUpdateStatus');
  const recordIds = currentModRollbackRecords.map(item => item.id).filter(Boolean);
  if (!recordIds.length) return;
  if (rollbackBtn) rollbackBtn.disabled = true;
  if (statusEl) statusEl.textContent = `正在回档 ${recordIds.length} 个 Mod...`;
  try {
    const tauri = await waitForTauri();
    const records = await tauri.core.invoke('rollback_mod_updates', {
      gameDir: localStorage.getItem('gameDir') || '',
      name: currentDetailInstance,
      recordIds,
    });
    renderModRollbackRecords(modal, records);
    await loadModList(currentDetailInstance);
    await refreshOldModBackups(modal);
    if (statusEl) statusEl.textContent = 'Mod 已回档';
  } catch (err) {
    if (statusEl) statusEl.textContent = `回档失败：${err}`;
    await showAlert?.(String(err), { title: '回档失败' });
    renderModRollbackRecords(modal, currentModRollbackRecords);
  }
}

async function refreshOldModBackups(modal) {
  const listEl = modal.querySelector('#modBulkOldList');
  if (listEl) listEl.innerHTML = '<div class="mod-list-empty">正在读取旧版列表...</div>';
  try {
    const tauri = await waitForTauri();
    const backups = await tauri.core.invoke('list_old_mod_backups', {
      gameDir: localStorage.getItem('gameDir') || '',
      name: currentDetailInstance,
    });
    renderOldModBackups(modal, backups);
  } catch (err) {
    currentOldModBackups = [];
    const deleteBtn = modal.querySelector('#modBulkDeleteOld');
    if (deleteBtn) deleteBtn.disabled = true;
    if (listEl) listEl.innerHTML = `<div class="mod-list-empty">读取旧版列表失败: ${escapeHtml(err)}</div>`;
    syncModBulkTabState(modal);
  }
}

function renderOldModBackups(modal, backups) {
  currentOldModBackups = Array.isArray(backups) ? backups : [];
  const listEl = modal.querySelector('#modBulkOldList');
  const deleteBtn = modal.querySelector('#modBulkDeleteOld');
  if (deleteBtn) deleteBtn.disabled = currentOldModBackups.length === 0 || modal.dataset.running === 'true';
  if (!listEl) return;
  if (!currentOldModBackups.length) {
    listEl.innerHTML = '<div class="mod-list-empty">暂无旧版 Mod</div>';
    syncModBulkTabState(modal);
    return;
  }
  listEl.innerHTML = currentOldModBackups.map(item => {
    const modified = item.modifiedMs ? formatModBackupTime(item.modifiedMs) : '';
    const size = formatFileSize(item.size || 0);
    return `
      <div class="mod-bulk-old-item" title="${escapeHtml(item.fileName || '')}">
        <span>${escapeHtml(item.fileName || '')}</span>
        <small>${escapeHtml([size, modified].filter(Boolean).join(' · '))}</small>
      </div>
    `;
  }).join('');
  syncModBulkTabState(modal);
}

function formatModBackupTime(ms) {
  const date = new Date(Number(ms));
  if (Number.isNaN(date.getTime())) return '';
  return date.toLocaleString('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

async function deleteOldModBackups(modal) {
  if (!currentOldModBackups.length || modal.dataset.running === 'true') return;
  const deleteBtn = modal.querySelector('#modBulkDeleteOld');
  const statusEl = modal.querySelector('#modBulkUpdateStatus');
  const fileNames = currentOldModBackups.map(item => item.fileName).filter(Boolean);
  if (!fileNames.length) return;
  if (deleteBtn) deleteBtn.disabled = true;
  if (statusEl) statusEl.textContent = `正在删除 ${fileNames.length} 个旧版 Mod...`;
  try {
    const tauri = await waitForTauri();
    const backups = await tauri.core.invoke('delete_old_mod_backups', {
      gameDir: localStorage.getItem('gameDir') || '',
      name: currentDetailInstance,
      fileNames,
    });
    renderOldModBackups(modal, backups);
    await refreshModRollbackRecords(modal);
    await loadModList(currentDetailInstance);
    if (statusEl) statusEl.textContent = '旧版 Mod 已删除';
  } catch (err) {
    if (statusEl) statusEl.textContent = `删除旧版失败：${err}`;
    await showAlert?.(String(err), { title: '删除旧版失败' });
    renderOldModBackups(modal, currentOldModBackups);
  }
}

async function runSelectedModUpdates(modal) {
  if (!modUpdateReady) return;
  const selectedFileNames = selectedModUpdateFileNames(modal);
  if (!selectedFileNames.length) return;
  const startBtn = modal.querySelector('#modBulkUpdateStart');
  const cancelBtn = modal.querySelector('#modBulkUpdateCancel');
  const deleteBtn = modal.querySelector('#modBulkDeleteOld');
  const rollbackBtn = modal.querySelector('#modBulkRollback');
  const statusEl = modal.querySelector('#modBulkUpdateStatus');
  modal.dataset.running = 'true';
  if (startBtn) {
    startBtn.disabled = true;
    startBtn.textContent = '更新中...';
  }
  if (cancelBtn) cancelBtn.disabled = true;
  if (deleteBtn) deleteBtn.disabled = true;
  if (rollbackBtn) rollbackBtn.disabled = true;
  modal.querySelectorAll('.mod-bulk-update-check, #modBulkUpdateSelectAll')
    .forEach(input => input.disabled = true);
  if (statusEl) statusEl.textContent = `正在更新 ${selectedFileNames.length} 个 Mod...`;

  try {
    const tauri = await waitForTauri();
    const result = await tauri.core.invoke('update_mods_from_cache', {
      ...getModUpdateArgs(),
      selectedFileNames,
    });
    if (result?.view) {
      applyModUpdateCacheView(result.view, { fromEvent: true });
    }
    await loadModList(currentDetailInstance);
    const failed = Array.isArray(result?.failed) ? result.failed : [];
    if (statusEl) {
      statusEl.textContent = failed.length
        ? `已更新 ${result?.updated || 0} 个，失败 ${failed.length} 个`
        : `已更新 ${result?.updated || selectedFileNames.length} 个 Mod`;
    }
    renderModBulkUpdateChoices(modal);
    if (Array.isArray(result?.oldBackups)) {
      renderOldModBackups(modal, result.oldBackups);
    } else {
      await refreshOldModBackups(modal);
    }
    if (Array.isArray(result?.rollbackRecords)) {
      renderModRollbackRecords(modal, result.rollbackRecords);
    } else {
      await refreshModRollbackRecords(modal);
    }
    if (failed.length) {
      await showAlert?.(failed.join('\n'), { title: '部分更新失败' });
    }
  } catch (err) {
    if (statusEl) statusEl.textContent = `更新失败：${err}`;
    await showAlert?.(String(err), { title: '更新失败' });
  } finally {
    modal.dataset.running = 'false';
    if (cancelBtn) cancelBtn.disabled = false;
    if (startBtn) startBtn.textContent = '开始更新';
    modal.querySelectorAll('.mod-bulk-update-check, #modBulkUpdateSelectAll')
      .forEach(input => input.disabled = false);
    updateModBulkSelectionSummary(modal);
    renderOldModBackups(modal, currentOldModBackups);
    renderModRollbackRecords(modal, currentModRollbackRecords);
  }
}

function applyModSourceLinks(links) {
  document.querySelectorAll('#modList .mod-link.mr, #modList .mod-link.cf')
    .forEach(link => link.remove());
  for (const info of links) {
    if (!info || !info.fileName || (!info.mrUrl && !info.cfUrl)) continue;
    _applyModUrls({
      file_name: info.fileName,
      mr_url: info.mrUrl || '',
      cf_url: info.cfUrl || '',
    });
  }
}

function _applyModUrls(info) {
  const safeId = info.file_name.replace(/[^a-zA-Z0-9]/g, '_');
  const actionsEl = document.getElementById('mod-actions-' + safeId);
  if (!actionsEl) return;

  const deleteBtn = actionsEl.querySelector('.mod-delete-btn');
  let linksHtml = '';
  if (info.mr_url && !actionsEl.querySelector('.mod-link.mr')) {
    linksHtml += `<a href="#" class="mod-link mr" data-url="${escapeHtml(info.mr_url)}" title="Modrinth">MR</a>`;
  }
  if (info.cf_url && !actionsEl.querySelector('.mod-link.cf')) {
    linksHtml += `<a href="#" class="mod-link cf" data-url="${escapeHtml(info.cf_url)}" title="CurseForge">CF</a>`;
  }

  if (linksHtml && deleteBtn) {
    deleteBtn.insertAdjacentHTML('beforebegin', linksHtml);
  }

  actionsEl.querySelectorAll('.mod-link').forEach(link => {
    if (link.dataset.bound === 'true') return;
    link.dataset.bound = 'true';
    link.addEventListener('click', async (e) => {
      e.preventDefault();
      e.stopPropagation();
      await openExternalUrl(link.dataset.url);
    });
  });
}

function bindModListEvents() {
  const listEl = document.getElementById('modList');
  if (!listEl || listEl.dataset.bound === 'true') return;
  listEl.dataset.bound = 'true';
  listEl.addEventListener('click', async (e) => {
    const target = e.target?.closest ? e.target : e.target?.parentElement;
    if (!target) return;
    const link = target.closest('.mod-link');
    if (link && listEl.contains(link)) {
      e.preventDefault();
      e.stopPropagation();
      await openExternalUrl(link.dataset.url);
      return;
    }

    const toggleBtn = target.closest('.mod-toggle');
    if (toggleBtn && listEl.contains(toggleBtn)) {
      e.stopPropagation();
      try {
        const tauri = await waitForTauri();
        const gameDir = localStorage.getItem('gameDir') || '';
        await tauri.core.invoke('toggle_mod', { gameDir, name: currentDetailInstance, fileName: toggleBtn.dataset.file });
        resetModUpdateState('正在读取更新缓存...');
        await loadModList(currentDetailInstance);
      } catch (err) {
        console.warn('切换 Mod 状态失败:', err);
      }
      return;
    }

    const deleteBtn = target.closest('.mod-delete-btn');
    if (!deleteBtn || !listEl.contains(deleteBtn)) return;
    e.stopPropagation();
    if (!deleteBtn.dataset.confirming) {
      deleteBtn.dataset.confirming = 'true';
      deleteBtn.textContent = '确认?';
      deleteBtn.classList.add('confirming');
      deleteBtn.dataset.timerId = String(setTimeout(() => {
        delete deleteBtn.dataset.confirming;
        deleteBtn.textContent = '🗑';
        deleteBtn.classList.remove('confirming');
      }, 2000));
      return;
    }

    clearTimeout(parseInt(deleteBtn.dataset.timerId));
    try {
      const tauri = await waitForTauri();
      const gameDir = localStorage.getItem('gameDir') || '';
      await tauri.core.invoke('delete_mod', { gameDir, name: currentDetailInstance, fileName: deleteBtn.dataset.file });
      resetModUpdateState('正在读取更新缓存...');
      await loadModList(currentDetailInstance);
    } catch (err) {
      console.warn('删除 Mod 失败:', err);
    }
  });
}

function bindModUpdateListEvents() {
  const listEl = document.getElementById('modUpdateList');
  if (!listEl || listEl.dataset.bound === 'true') return;
  listEl.dataset.bound = 'true';
  listEl.addEventListener('click', async (e) => {
    const link = e.target?.closest?.('.mod-link');
    if (!link || !listEl.contains(link)) return;
    e.preventDefault();
    e.stopPropagation();
    await openExternalUrl(link.dataset.url);
  });
}

let currentOnlineCategory = 'mod';
let _onlineSearchId = 0; // 防止异步竞态

// 持久化缓存（localStorage，3天过期，新的覆盖旧的）
const CACHE_KEY = 'onlineSearchCache_v2';
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
    const now = Date.now();
    for (const k of Object.keys(cache)) {
      const v = cache[k];
      // 数组值包装成 { _data, _ts }；已包装过的跳过
      if (Array.isArray(v)) {
        cache[k] = { _data: v, _ts: now };
      } else if (v && typeof v === 'object' && !v._ts) {
        v._ts = now;
      }
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
  const allVersionPrefix = `all:${loader || ''}:`;
  let changed = false;
  for (const key of Object.keys(onlineSearchCache)) {
    if (key.startsWith(prefix) || key.startsWith(allVersionPrefix)) {
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
  const cacheKey = `all:${currentDetailInfo?.loader_type || ''}:${currentOnlineCategory}:${query}`;
  const cached = onlineSearchCache[cacheKey];
  if (cached) {
    renderOnlineResults(cached._data || cached, query);
    return;
  }

  listEl.innerHTML = '<div class="mod-list-empty">搜索中...</div>';

  const searchId = ++_onlineSearchId;

  try {
    const tauri = await waitForTauri();
    const mcVersion = '';
    const loader = currentDetailInfo?.loader_type || '';
    const projectType = currentOnlineCategory;
    const results = await tauri.core.invoke('search_online_mods', { query, mcVersion, loader, projectType });
    // 丢弃过期请求（快速切换类别时旧请求晚回来）
    if (searchId !== _onlineSearchId) return;
    onlineSearchCache[cacheKey] = results;
    _saveCache(onlineSearchCache);
    renderOnlineResults(results, query);
  } catch (err) {
    if (searchId !== _onlineSearchId) return;
    listEl.innerHTML = `<div class="mod-list-empty">搜索失败: ${escapeHtml(err)}</div>`;
  }
}

function ensureOnlineModVersionModal() {
  let modal = document.getElementById('onlineModVersionModal');
  if (modal) return modal;

  document.body.insertAdjacentHTML('beforeend', `
    <div class="modal-overlay hidden online-mod-version-modal" id="onlineModVersionModal">
      <div class="modal-content" data-no-drag style="max-width: 620px;">
        <div class="modal-header">
          <h2 id="onlineModVersionTitle">选择版本</h2>
        </div>
        <div class="modal-body">
          <div id="onlineModVersionList" class="online-mod-version-list">
            <div class="mod-list-empty">加载中...</div>
          </div>
        </div>
        <div class="modal-footer">
          <input class="mod-search-input online-mod-version-filter" id="onlineModVersionFilter" placeholder="筛选 Minecraft 版本或文件名">
          <button id="onlineModVersionCancel" class="btn btn-secondary">取消</button>
        </div>
      </div>
    </div>
  `);
  return document.getElementById('onlineModVersionModal');
}

async function showOnlineModVersionModal(mod) {
  const modal = ensureOnlineModVersionModal();
  const titleEl = document.getElementById('onlineModVersionTitle');
  const filterEl = document.getElementById('onlineModVersionFilter');
  const listEl = document.getElementById('onlineModVersionList');
  const cancelBtn = document.getElementById('onlineModVersionCancel');
  if (!modal || !listEl) return null;

  const title = mod.cn_title ? `${mod.cn_title} (${mod.title})` : mod.title;
  if (titleEl) titleEl.textContent = `${title} - 选择下载版本`;
  if (filterEl) filterEl.value = '';
  listEl.innerHTML = '<div class="mod-list-empty">正在加载版本列表...</div>';
  modal.classList.remove('hidden');

  const tauri = await waitForTauri();
  let versions = [];
  let loadError = '';
  try {
    versions = await tauri.core.invoke('get_online_mod_versions', {
      projectId: mod.project_id,
      loader: currentDetailInfo?.loader_type || '',
      projectType: currentOnlineCategory,
    });
  } catch (err) {
    loadError = String(err);
  }

  return new Promise((resolve) => {
    let resolved = false;

    const close = (value) => {
      if (resolved) return;
      resolved = true;
      modal.classList.add('hidden');
      cancelBtn?.removeEventListener('click', onCancel);
      modal.removeEventListener('click', onOverlayClick);
      filterEl?.removeEventListener('input', render);
      resolve(value);
    };

    const onCancel = () => close(null);
    const onOverlayClick = (e) => {
      if (e.target === modal) close(null);
    };

    const render = () => {
      if (loadError) {
        listEl.innerHTML = `<div class="mod-list-empty">加载失败: ${escapeHtml(loadError)}</div>`;
        return;
      }

      const term = (filterEl?.value || '').trim().toLowerCase();
      const currentMc = currentDetailInfo?.mc_version || '';
      const filtered = term
        ? versions.filter(v => {
            const text = `${v.version_name} ${v.mc_versions} ${v.loaders} ${v.file_name}`.toLowerCase();
            return text.includes(term);
          })
        : versions;

      if (filtered.length === 0) {
        listEl.innerHTML = '<div class="mod-list-empty">暂无可用版本</div>';
        return;
      }

      listEl.innerHTML = filtered.map((v, idx) => {
        const mcList = String(v.mc_versions || '').split(',').map(s => s.trim()).filter(Boolean);
        const isCurrent = currentMc && mcList.includes(currentMc);
        const size = formatFileSize(v.file_size || 0);
        return `
          <div class="online-mod-version-row ${isCurrent ? 'recommended' : ''}">
            <div class="online-mod-version-info">
              <div class="online-mod-version-name" title="${escapeHtml(v.version_name || v.file_name || '')}">
                ${escapeHtml(v.version_name || v.file_name || '未命名版本')}
              </div>
              <div class="online-mod-version-meta">
                <span>MC ${escapeHtml(v.mc_versions || '未知')}</span>
                ${v.loaders ? `<span>${escapeHtml(v.loaders)}</span>` : ''}
                ${size ? `<span>${size}</span>` : ''}
                ${v.date ? `<span>${escapeHtml(v.date)}</span>` : ''}
                ${isCurrent ? '<span class="online-mod-version-current">当前版本</span>' : ''}
              </div>
              <div class="online-mod-version-file" title="${escapeHtml(v.file_name || '')}">
                ${escapeHtml(v.file_name || '')}
              </div>
            </div>
            <button class="online-mod-version-pick" data-index="${idx}">下载</button>
          </div>
        `;
      }).join('');

      listEl.querySelectorAll('.online-mod-version-pick').forEach(btn => {
        btn.addEventListener('click', () => {
          close(filtered[Number(btn.dataset.index)]);
        });
      });
    };

    cancelBtn?.addEventListener('click', onCancel);
    modal.addEventListener('click', onOverlayClick);
    filterEl?.addEventListener('input', render);

    render();
    filterEl?.focus();
  });
}

function renderOnlineResults(results, query) {
  const listEl = document.getElementById('onlineModList');
  if (!listEl) return;

  if (results.length === 0) {
    const isChinese = /[\u4e00-\u9fa5]/.test(query);
    const safeQuery = escapeHtml(query || '');
    listEl.innerHTML = `<div class="mod-list-empty">
      未找到匹配的结果${isChinese ? '<br><span style="font-size:11px;margin-top:4px;display:inline-block;">中文搜索推荐 <a href="#" class="mcmod-search-link" style="color:var(--pink-700);font-weight:600;">在MC百科搜索「' + safeQuery + '」</a></span>' : ''}
    </div>`;
    if (isChinese) {
      listEl.querySelector('.mcmod-search-link')?.addEventListener('click', (e) => {
        e.preventDefault();
        openExternalUrl(`https://search.mcmod.cn/s?key=${encodeURIComponent(query)}&filter=1`);
      });
    }
    return;
  }

  // escapeHtml 使用全局 utils.js

  listEl.innerHTML = results.map(mod => {
    const dlCount = escapeHtml(formatDownloads(mod.downloads));
    const src = getSourceInfo(mod);
    const sourceLabel = escapeHtml(src.label);
    const sourceClass = src.cssClass === 'both' || src.cssClass === 'cf' ? src.cssClass : 'mr';
    const hasMR = src.hasMR;
    const hasCF = src.hasCF;
    const displayTitle = mod.cn_title ? `${mod.cn_title} (${mod.title})` : mod.title;
    const iconUrl = escapeHtml(mod.icon_url || '');
    const mrUrl = escapeHtml(mod.mr_url || '');
    const cfUrl = escapeHtml(mod.cf_url || '');
    const projectId = escapeHtml(mod.project_id || '');
    const title = escapeHtml(mod.title || '');
    return `
      <div class="online-mod-card">
        <img class="online-mod-icon" src="${iconUrl}" alt="" onerror="this.style.display='none'">
        <div class="online-mod-info">
          <div class="online-mod-title" title="${escapeHtml(displayTitle)}">
            <span class="mod-source-tag ${sourceClass}">${sourceLabel}</span> ${escapeHtml(displayTitle)}
          </div>
          <div class="online-mod-desc" title="${escapeHtml(mod.description)}">${escapeHtml(mod.description)}</div>
          <div class="online-mod-meta">${escapeHtml(mod.author || '')} · ${dlCount} 下载</div>
        </div>
        <div class="online-mod-actions">
          <div class="mod-link-row">
            ${hasMR ? `<button class="mod-link-btn mr" data-url="${mrUrl}" aria-label="在 Modrinth 查看">MR</button>` : ''}
            ${hasCF ? `<button class="mod-link-btn cf" data-url="${cfUrl}" aria-label="在 CurseForge 查看">CF</button>` : ''}
          </div>
          <button class="online-mod-dl-btn" data-project="${projectId}" data-title="${title}">下载</button>
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
      const originalText = btn.textContent;
      btn.textContent = '选择版本...';

      try {
        const mod = results.find(item => item.project_id === btn.dataset.project);
        if (!mod) throw new Error('未找到 Mod 信息');
        const selectedVersion = await showOnlineModVersionModal(mod);
        if (!selectedVersion) {
          btn.disabled = false;
          btn.textContent = originalText;
          return;
        }

        const versionId = selectedVersion.version_id || '';
        const taskName = `online-mod:${currentDetailInstance}:${btn.dataset.project}:${versionId}`;
        let downloading = true;
        const tauri = await waitForTauri();
        const onCancelDownload = async (event) => {
          event.stopPropagation();
          event.preventDefault();
          if (!downloading) return;
          btn.disabled = true;
          btn.textContent = '取消中...';
          try {
            await tauri.core.invoke('cancel_modpack_install', { fileName: taskName });
          } catch (err) {
            btn.disabled = false;
            btn.textContent = '取消';
            console.warn('取消下载失败:', err);
          }
        };
        btn.disabled = false;
        btn.textContent = '取消';
        btn.addEventListener('click', onCancelDownload);
        const gameDir = localStorage.getItem('gameDir') || '';
        const result = await tauri.core.invoke('download_online_mod', {
          gameDir,
          name: currentDetailInstance,
          projectId: btn.dataset.project,
          mcVersion: selectedVersion.mc_version || currentDetailInfo?.mc_version || '',
          loader: selectedVersion.loader || currentDetailInfo?.loader_type || '',
          projectType: currentOnlineCategory,
          versionId,
        }).finally(() => {
          downloading = false;
          btn.removeEventListener('click', onCancelDownload);
        });
        btn.textContent = '已下载';
        btn.classList.add('done');
        // 刷新已安装列表
        resetModUpdateState('正在读取更新缓存...');
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
  ensureModpackExportModal();
  bindModListEvents();
  bindModUpdateListEvents();
  bindModUpdateCacheEvents();

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
      modListRenderSeq++;
      currentDetailInstance = null;
      currentDetailInfo = null;
      currentModList = [];
      resetModUpdateState('正在读取更新缓存...');
    });
  }

  // 文件夹按钮
  document.querySelectorAll('.instance-folder-btn').forEach(btn => {
    btn.addEventListener('click', async () => {
      if (btn.id === 'instanceExportBtn' || btn.id === 'instanceAnalyzeBtn' || btn.id === 'instanceConfigBtn') return;
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
  document.getElementById('instanceExportBtn')?.addEventListener('click', showModpackExportModal);
  document.getElementById('instanceAnalyzeBtn')?.addEventListener('click', analyzeCurrentInstanceMods);

  // Tab 切换
  document.querySelectorAll('.mod-tab').forEach(tab => {
    tab.addEventListener('click', () => switchModTab(tab.dataset.tab));
  });

  // 已安装搜索
  document.getElementById('modSearchInput')?.addEventListener('input', () => renderModList(currentModList));
  document.getElementById('modRefreshBtn')?.addEventListener('click', () => {
    resetModUpdateState('正在读取更新缓存...');
    loadModList();
  });
  document.getElementById('modBulkUpdateBtn')?.addEventListener('click', showModBulkUpdateModal);

  // 实例独立设置
  document.getElementById('instanceConfigBtn')?.addEventListener('click', showInstanceSettingsModal);

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
        `确定删除版本 ${currentDetailInstance} 吗？此操作不可恢复。`,
        { title: '删除确认', kind: 'danger' }
      );
      if (!confirmed) return;
      const tauri = await waitForTauri();
      await tauri.core.invoke('delete_version', { gameDir, name: currentDetailInstance });
      ['mem', 'javaMode', 'javaPath', 'jvmArgs', 'jvmPreset'].forEach(prefix => {
        localStorage.removeItem(instanceSettingKey(prefix));
      });
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
