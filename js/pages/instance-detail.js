// ============ 实例详情页逻辑 ============

let currentDetailInstance = null;
let currentDetailInfo = null; // { mc_version, loader_type }
let currentModList = [];
const modUrlCache = {}; // 缓存已查过的 mod 链接
let modListLoadSeq = 0;
let modListRenderSeq = 0;
let modUrlLookupSeq = 0;
let modUrlLookupTimer = null;
let instanceSettingsModal = null;
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
  modal.className = 'modal-overlay hidden instance-settings-modal';
  modal.innerHTML = `
    <div class="modal-content" data-no-drag>
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
    modal.classList.add('hidden');
    modal.style.display = 'none';
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
    <div class="hidden modpack-export-modal" id="modpackExportModal" data-no-drag>
      <div class="modal-content" data-no-drag>
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

  const close = () => modal.classList.add('hidden');
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
    listEl.innerHTML = `<div class="mod-list-empty">扫描失败: ${escapeHtml(err)}</div>`;
    summaryEl.textContent = '扫描失败';
    }
  }, 250);
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
  if (listEl) listEl.innerHTML = '<div class="mod-list-empty">加载中...</div>';
  try {
    await afterNextPaint();
    if (loadSeq !== modListLoadSeq || currentDetailInstance !== instanceName) return;
    const tauri = await waitForTauri();
    const gameDir = localStorage.getItem('gameDir') || '';
    const mods = await tauri.core.invoke('list_mods', { gameDir, name: instanceName });
    if (loadSeq !== modListLoadSeq || currentDetailInstance !== instanceName) return;
    currentModList = mods;
    if (countEl) countEl.textContent = mods.length;
    renderModList(mods);
  } catch (err) {
    console.warn('加载 Mod 列表失败:', err);
    if (listEl) listEl.innerHTML = '<div class="mod-list-empty">加载失败</div>';
  }
}

function renderModItem(mod) {
  const fileName = mod.file_name || '';
  const safeFileName = escapeHtml(fileName);
  const baseName = fileName.replace(/\.jar\.disabled$/i, '').replace(/\.jar$/i, '');
  const displayName = mod.cn_name ? `${mod.cn_name} (${baseName})` : baseName;
  const actionId = fileName.replace(/[^a-zA-Z0-9]/g, '_');
  return `
    <div class="mod-item ${mod.enabled ? '' : 'disabled'}" data-file="${safeFileName}">
      <button class="mod-toggle ${mod.enabled ? 'active' : ''}" data-file="${safeFileName}" title="${mod.enabled ? '点击禁用' : '点击启用'}"></button>
      <span class="mod-name" title="${safeFileName}">${escapeHtml(displayName)}</span>
      <span class="mod-actions" id="mod-actions-${actionId}">
        <button class="mod-delete-btn" data-file="${safeFileName}" title="删除">🗑</button>
      </span>
      <span class="mod-size">${mod.size_kb > 1024 ? (mod.size_kb / 1024).toFixed(1) + ' MB' : mod.size_kb + ' KB'}</span>
    </div>
  `;
}

// 渲染已安装 mod 列表
function renderModList(mods) {
  const listEl = document.getElementById('modList');
  if (!listEl) return;
  const renderSeq = ++modListRenderSeq;
  if (modUrlLookupTimer) {
    clearTimeout(modUrlLookupTimer);
    modUrlLookupTimer = null;
  }

  const searchVal = (document.getElementById('modSearchInput')?.value || '').toLowerCase();
  const filtered = searchVal
    ? mods.filter(m => m.file_name.toLowerCase().includes(searchVal) || (m.cn_name && m.cn_name.toLowerCase().includes(searchVal)))
    : mods;

  if (filtered.length === 0) {
    listEl.innerHTML = `<div class="mod-list-empty">${mods.length === 0 ? '暂无 Mod' : '无匹配结果'}</div>`;
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
      scheduleModUrlLookup(filtered, renderSeq, searchVal);
    }
  };
  renderChunk();
}

function scheduleModUrlLookup(mods, renderSeq, searchVal) {
  const allFileNames = mods.map(m => m.file_name);
  for (const fn of allFileNames) {
    if (modUrlCache[fn]) _applyModUrls(modUrlCache[fn]);
  }

  const uncached = allFileNames.filter(fn => !modUrlCache[fn]);
  if (uncached.length === 0) return;

  const lookupSeq = ++modUrlLookupSeq;
  const lookupLimit = searchVal ? 200 : 80;
  const pending = uncached.slice(0, lookupLimit);
  modUrlLookupTimer = setTimeout(async () => {
    try {
      const tauri = await waitForTauri();
      const batchSize = 20;
      for (let i = 0; i < pending.length; i += batchSize) {
        if (lookupSeq !== modUrlLookupSeq || renderSeq !== modListRenderSeq) return;
        const urls = await tauri.core.invoke('lookup_mod_urls', { fileNames: pending.slice(i, i + batchSize) });
        if (lookupSeq !== modUrlLookupSeq || renderSeq !== modListRenderSeq) return;
        for (const info of urls) {
          modUrlCache[info.file_name] = info;
          _applyModUrls(info);
        }
        await afterNextPaint();
      }
    } catch (err) {
      console.warn('查询 Mod 链接失败:', err);
    }
  }, 400);
}

function _applyModUrls(info) {
  const safeId = info.file_name.replace(/[^a-zA-Z0-9]/g, '_');
  const actionsEl = document.getElementById('mod-actions-' + safeId);
  if (!actionsEl || actionsEl.querySelector('.mod-link')) return; // 已有链接则跳过

  const deleteBtn = actionsEl.querySelector('.mod-delete-btn');
  let linksHtml = '';
  if (info.mr_url) linksHtml += `<a href="#" class="mod-link mr" data-url="${escapeHtml(info.mr_url)}" title="Modrinth">MR</a>`;
  if (info.cf_url) linksHtml += `<a href="#" class="mod-link cf" data-url="${escapeHtml(info.cf_url)}" title="CurseForge">CF</a>`;

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
      await loadModList(currentDetailInstance);
    } catch (err) {
      console.warn('删除 Mod 失败:', err);
    }
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
        window.open(`https://search.mcmod.cn/s?key=${encodeURIComponent(query)}&filter=1`, '_blank');
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
      modUrlLookupSeq++;
      if (modUrlLookupTimer) clearTimeout(modUrlLookupTimer);
      currentDetailInstance = null;
      currentDetailInfo = null;
      currentModList = [];
    });
  }

  // 文件夹按钮
  document.querySelectorAll('.instance-folder-btn').forEach(btn => {
    btn.addEventListener('click', async () => {
      if (btn.id === 'instanceExportBtn') return;
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

  // Tab 切换
  document.querySelectorAll('.mod-tab').forEach(tab => {
    tab.addEventListener('click', () => switchModTab(tab.dataset.tab));
  });

  // 已安装搜索
  document.getElementById('modSearchInput')?.addEventListener('input', () => renderModList(currentModList));
  document.getElementById('modRefreshBtn')?.addEventListener('click', () => loadModList());

  // 实例独立设置
  document.getElementById('instanceJavaMode')?.addEventListener('change', updateInstanceJavaPathState);
  document.getElementById('instanceUseGlobalJavaBtn')?.addEventListener('click', () => {
    const input = document.getElementById('instanceJavaPathInput');
    if (input) input.value = localStorage.getItem('selectedJavaPath') || '';
  });
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
      ['mem', 'javaMode', 'javaPath', 'jvmArgs'].forEach(prefix => {
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
