// ============ 全局共享工具函数 ============

const OAOI_DEFAULT_JVM_ARGS = [
  '-XX:+UnlockExperimentalVMOptions',
  '-XX:+UseG1GC',
  '-XX:G1NewSizePercent=20',
  '-XX:G1ReservePercent=20',
  '-XX:MaxGCPauseMillis=50',
  '-XX:G1HeapRegionSize=32M',
  '-XX:-OmitStackTraceInFastThrow',
  '-Dfml.ignoreInvalidMinecraftCertificates=True',
  '-Dfml.ignorePatchDiscrepancies=True',
].join(' ');

const OAOI_COMPAT_JVM_ARGS = [
  '-XX:+UseG1GC',
  '-XX:-UseAdaptiveSizePolicy',
  '-XX:-OmitStackTraceInFastThrow',
  '-Dfml.ignoreInvalidMinecraftCertificates=True',
  '-Dfml.ignorePatchDiscrepancies=True',
].join(' ');

const OAOI_JVM_PRESETS = {
  recommended: {
    name: '推荐',
    description: '适合大多数版本和整合包',
    args: OAOI_DEFAULT_JVM_ARGS,
  },
  compat: {
    name: '兼容',
    description: '参数更保守，适合老 Forge 或特殊整合包',
    args: OAOI_COMPAT_JVM_ARGS,
  },
  clean: {
    name: '纯净',
    description: '不添加额外 JVM 参数，适合排查问题',
    args: '',
  },
  custom: {
    name: '自定义',
    description: '使用你手动编辑的 JVM 参数',
    args: '',
  },
};

function getGlobalJvmArgs() {
  if (localStorage.getItem('customJvmArgs') === null) {
    localStorage.setItem('customJvmArgs', OAOI_DEFAULT_JVM_ARGS);
    localStorage.setItem('jvmArgsPreset', 'recommended');
  }
  return localStorage.getItem('customJvmArgs') || '';
}

function getJvmArgsPresetByValue(value) {
  const normalized = String(value || '').trim();
  for (const key of ['recommended', 'compat', 'clean']) {
    if ((OAOI_JVM_PRESETS[key].args || '').trim() === normalized) return key;
  }
  return 'custom';
}

/**
 * HTML 转义，防止 XSS
 */
function escapeHtml(s) {
  return String(s).replace(/&/g, '&amp;').replace(/"/g, '&quot;')
                  .replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

/**
 * 安装进度阶段中文标签
 */
const STAGE_LABELS = {
  downloading: '下载整合包', detecting: '识别格式', installing: '安装中',
  extracting: '解压中', meta: '元数据', client: 'client.jar',
  libraries: '依赖库', assetIndex: '资源索引', assets: '资源文件', mods: 'Mod 文件',
  resourcepacks: '材质包', shaderpacks: '光影包', config: '配置文件', other: '其他文件',
  java: 'Java 环境', loader: '加载器', forge: 'Forge',
  'forge-installer': 'Forge 安装器', 'forge-libs': 'Forge 依赖库',
  neoforge: 'NeoForge', 'neoforge-installer': 'NeoForge 安装器', 'neoforge-libs': 'NeoForge 依赖库',
  fabric: 'Fabric', 'fabric-libs': 'Fabric 组件', quilt: 'Quilt',
  processors: '处理器', overrides: '覆盖文件'
};

/**
 * 格式化下载数 (1234567 → "1.2M", 12345 → "12K")
 */
function formatDownloads(n) {
  if (n > 1000000) return (n / 1000000).toFixed(1) + 'M';
  if (n > 1000) return (n / 1000).toFixed(0) + 'K';
  return String(n);
}

/**
 * 格式化文件大小 (字节 → "1.5 MB" / "320 KB")
 */
function formatFileSize(bytes) {
  if (bytes > 1048576) return (bytes / 1048576).toFixed(1) + ' MB';
  if (bytes > 1024) return (bytes / 1024).toFixed(0) + ' KB';
  if (bytes > 0) return bytes + ' B';
  return '';
}

function formatProgressSize(bytes) {
  const value = Number(bytes || 0);
  if (value >= 1048576) return `${(value / 1048576).toFixed(1)}MB`;
  if (value >= 1024) return `${(value / 1024).toFixed(0)}KB`;
  return `${value}B`;
}

function formatStageProgress(current, total, stage = '') {
  const cur = Number(current || 0);
  const tot = Number(total || 0);
  const looksLikeBytes = stage === 'downloading' || cur >= 1048576 || tot >= 1048576;
  return looksLikeBytes ? `${formatProgressSize(cur)} / ${formatProgressSize(tot)}` : `${cur}/${tot}`;
}

/**
 * 判断来源标签 (MR/CF/MR+CF)
 */
function getSourceInfo(item) {
  const hasMR = item.mr_url && item.mr_url.length > 0;
  const hasCF = item.cf_url && item.cf_url.length > 0;
  return {
    label: (hasMR && hasCF) ? 'MR+CF' : hasCF ? 'CurseForge' : 'Modrinth',
    cssClass: (hasMR && hasCF) ? 'both' : hasCF ? 'cf' : 'mr',
    hasMR, hasCF
  };
}

function normalizeExternalHttpUrl(url) {
  try {
    const parsed = new URL(url, window.location.href);
    if (parsed.protocol === 'http:' || parsed.protocol === 'https:') {
      return parsed.href;
    }
  } catch {}
  return '';
}

/**
 * 用系统默认浏览器打开外部 URL，避免 Tauri 内置 WebView 直接跳转。
 */
async function openExternalUrl(url) {
  const externalUrl = normalizeExternalHttpUrl(url);
  if (!externalUrl) {
    showToast('链接格式无效', 'error');
    return;
  }

  try {
    const tauri = await waitForTauri();
    await tauri.core.invoke('open_url', { url: externalUrl });
  } catch (err) {
    console.warn('[external-link] 打开外部链接失败:', err);
    if (window.__TAURI__) {
      showToast('打开外部链接失败，请检查系统默认浏览器', 'error');
      return;
    }
    window.open(externalUrl, '_blank', 'noopener,noreferrer');
  }
}

function bindExternalLinkInterceptor() {
  if (window.__oaoiExternalLinkInterceptorBound) return;
  window.__oaoiExternalLinkInterceptorBound = true;

  document.addEventListener('click', (event) => {
    const link = event.target.closest?.('a[href]');
    if (!link || event.defaultPrevented || event.button !== 0) return;
    if (event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;

    const externalUrl = normalizeExternalHttpUrl(link.getAttribute('href'));
    if (!externalUrl) return;

    event.preventDefault();
    openExternalUrl(externalUrl);
  });
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', bindExternalLinkInterceptor, { once: true });
} else {
  bindExternalLinkInterceptor();
}

// ============ 全局自定义弹窗（替代 alert） ============

/**
 * 显示应用内 Toast 弹窗
 * @param {string} message - 提示文字
 * @param {'warn'|'error'|'info'|'success'} type - 类型
 * @param {number} duration - 自动消失时间(ms)，0 表示不自动消失
 */
function showToast(message, type = 'warn', duration = 4000) {
  // 确保容器存在
  let container = document.getElementById('toastContainer');
  if (!container) {
    container = document.createElement('div');
    container.id = 'toastContainer';
    document.body.appendChild(container);
  }

  const icons = { warn: '⚠️', error: '❌', info: 'ℹ️', success: '✅' };
  const toast = document.createElement('div');
  toast.className = `oaoi-toast oaoi-toast-${type}`;
  toast.innerHTML = `
    <span class="oaoi-toast-icon">${icons[type] || '⚠️'}</span>
    <span class="oaoi-toast-msg">${escapeHtml(message)}</span>
    <button class="oaoi-toast-close">✕</button>
  `;

  container.appendChild(toast);

  // 关闭按钮
  toast.querySelector('.oaoi-toast-close').addEventListener('click', () => dismiss());

  // 点击整体也关闭
  toast.addEventListener('click', (e) => {
    if (e.target.closest('.oaoi-toast-close')) return;
    dismiss();
  });

  // 自动消失
  let timer = null;
  if (duration > 0) {
    timer = setTimeout(() => dismiss(), duration);
  }

  function dismiss() {
    if (timer) clearTimeout(timer);
    toast.classList.add('oaoi-toast-out');
    toast.addEventListener('animationend', () => toast.remove(), { once: true });
  }
}

// 注入 Toast 样式（仅一次）
(function injectToastStyles() {
  if (document.getElementById('oaoiToastStyles')) return;
  const style = document.createElement('style');
  style.id = 'oaoiToastStyles';
  style.textContent = `
    #toastContainer {
      position: fixed;
      top: 40px;
      left: 50%;
      transform: translateX(-50%);
      z-index: 99999;
      display: flex;
      flex-direction: column;
      align-items: center;
      gap: 8px;
      pointer-events: none;
    }
    .oaoi-toast {
      pointer-events: auto;
      display: flex;
      align-items: center;
      gap: 10px;
      padding: 12px 18px;
      border-radius: 14px;
      background: var(--modal-surface-white-92, rgba(255, 255, 255, 0.92));
      backdrop-filter: blur(16px);
      box-shadow: var(--theme-toast-shadow, 0 8px 32px rgba(232, 69, 116, 0.15), 0 0 0 1px rgba(255, 200, 215, 0.25));
      font-size: 13px;
      color: var(--text-dark, #4a2030);
      max-width: 420px;
      min-width: 200px;
      cursor: pointer;
      animation: toastSlideIn 0.35s cubic-bezier(0.34, 1.56, 0.64, 1);
      transition: opacity 0.2s, transform 0.2s;
    }
    .oaoi-toast:hover {
      transform: scale(1.02);
      box-shadow: var(--theme-toast-hover-shadow, 0 10px 40px rgba(232, 69, 116, 0.2), 0 0 0 1px rgba(255, 200, 215, 0.35));
    }
    .oaoi-toast-warn {
      border-left: 4px solid #f59e0b;
    }
    .oaoi-toast-error {
      border-left: 4px solid #ef4444;
      background: var(--modal-surface-pink-100-95, rgba(255, 245, 245, 0.95));
    }
    .oaoi-toast-info {
      border-left: 4px solid var(--pink-500, #ff6b8a);
    }
    .oaoi-toast-success {
      border-left: 4px solid #22c55e;
    }
    .oaoi-toast-icon {
      font-size: 18px;
      flex-shrink: 0;
    }
    .oaoi-toast-msg {
      flex: 1;
      line-height: 1.45;
      word-break: break-word;
    }
    .oaoi-toast-close {
      background: none;
      border: none;
      color: var(--text-light, #c4849e);
      font-size: 14px;
      cursor: pointer;
      padding: 2px 4px;
      border-radius: 6px;
      flex-shrink: 0;
      transition: color 0.15s, background 0.15s;
    }
    .oaoi-toast-close:hover {
      color: var(--pink-600, #e84574);
      background: var(--theme-accent-soft, rgba(232, 69, 116, 0.08));
    }
    @keyframes toastSlideIn {
      from { opacity: 0; transform: translateY(-20px) scale(0.95); }
      to   { opacity: 1; transform: translateY(0) scale(1); }
    }
    .oaoi-toast-out {
      animation: toastSlideOut 0.25s ease forwards;
    }
    @keyframes toastSlideOut {
      to { opacity: 0; transform: translateY(-16px) scale(0.95); }
    }
  `;
  document.head.appendChild(style);
})();

// ============ 全局自定义确认弹窗（替代 dialog.ask / confirm） ============

/**
 * 显示应用内确认弹窗
 * @param {string} message - 提示文字
 * @param {object} options - 可选项
 * @param {string} options.title - 标题（默认 '确认'）
 * @param {string} options.confirmText - 确认按钮文字（默认 '确定'）
 * @param {string} options.cancelText - 取消按钮文字（默认 '取消'）
 * @param {'warning'|'info'|'danger'} options.kind - 类型
 * @returns {Promise<boolean>}
 */
function showConfirm(message, options = {}) {
  const { title = '确认', confirmText = '确定', cancelText = '取消', kind = 'warning', dialogClass = '' } = options;
  const safeDialogClass = String(dialogClass)
    .split(/\s+/)
    .map(cls => cls.replace(/[^a-zA-Z0-9_-]/g, ''))
    .filter(Boolean)
    .join(' ');

  return new Promise((resolve) => {
    const overlay = document.createElement('div');
    overlay.className = 'oaoi-confirm-overlay oaoi-modal-host';
    overlay.innerHTML = `
      <div class="oaoi-confirm-card oaoi-modal-card${safeDialogClass ? ` ${safeDialogClass}` : ''}">
        <div class="oaoi-confirm-header">
          <span class="oaoi-confirm-title">${escapeHtml(title)}</span>
        </div>
        <div class="oaoi-confirm-body">${escapeHtml(message)}</div>
        <div class="oaoi-confirm-actions">
          <button class="oaoi-confirm-btn cancel">${escapeHtml(cancelText)}</button>
          <button class="oaoi-confirm-btn confirm">${escapeHtml(confirmText)}</button>
        </div>
      </div>
    `;

    document.body.appendChild(overlay);

    let closed = false;
    function close(result) {
      if (closed) return;
      closed = true;
      document.removeEventListener('keydown', onKey);
      setTimeout(() => { overlay.remove(); resolve(result); }, 200);
    }

    overlay.querySelector('.oaoi-confirm-btn.cancel').addEventListener('click', () => close(false));
    overlay.querySelector('.oaoi-confirm-btn.confirm').addEventListener('click', () => close(true));
    overlay.addEventListener('click', (e) => { if (e.target === overlay) close(false); });

    // ESC 取消
    function onKey(e) { if (e.key === 'Escape') close(false); }
    document.addEventListener('keydown', onKey);

    // 自动聚焦确认按钮
    requestAnimationFrame(() => overlay.querySelector('.oaoi-confirm-btn.confirm').focus());
  });
}

function showAlert(message, options = {}) {
  const { title = '提示', confirmText = '我知道了', kind = 'info' } = options;

  return new Promise((resolve) => {
    const overlay = document.createElement('div');
    overlay.className = 'oaoi-confirm-overlay oaoi-modal-host';
    overlay.innerHTML = `
      <div class="oaoi-confirm-card oaoi-modal-card oaoi-alert-confirm">
        <div class="oaoi-confirm-header">
          <span class="oaoi-confirm-title">${escapeHtml(title)}</span>
        </div>
        <div class="oaoi-confirm-body">${escapeHtml(message)}</div>
        <div class="oaoi-confirm-actions">
          <button class="oaoi-confirm-btn confirm">${escapeHtml(confirmText)}</button>
        </div>
      </div>
    `;

    document.body.appendChild(overlay);

    let closed = false;
    function close() {
      if (closed) return;
      closed = true;
      document.removeEventListener('keydown', onKey);
      setTimeout(() => { overlay.remove(); resolve(true); }, 200);
    }

    overlay.querySelector('.oaoi-confirm-btn.confirm').addEventListener('click', close);
    overlay.addEventListener('click', (e) => { if (e.target === overlay) close(); });

    function onKey(e) {
      if (e.key === 'Escape' || e.key === 'Enter') {
        close();
      }
    }
    document.addEventListener('keydown', onKey);

    requestAnimationFrame(() => overlay.querySelector('.oaoi-confirm-btn.confirm').focus());
  });
}

let oaoiUiTaskSequence = 0;
const oaoiInstallEventNames = new Set();

function createUiTaskId(prefix = 'task') {
  oaoiUiTaskSequence += 1;
  const suffix = typeof globalThis.crypto?.randomUUID === 'function'
    ? globalThis.crypto.randomUUID()
    : `${Date.now()}-${oaoiUiTaskSequence}`;
  return `${prefix}-${suffix}`;
}

function acquireInstallEventName(name) {
  const key = String(name || '').trim().toLocaleLowerCase();
  if (!key || oaoiInstallEventNames.has(key)) return false;
  oaoiInstallEventNames.add(key);
  return true;
}

function releaseInstallEventName(name) {
  oaoiInstallEventNames.delete(String(name || '').trim().toLocaleLowerCase());
}

function ensureDownloadDetailModal() {
  let modal = document.getElementById('dlDetailModal');
  if (modal) return modal;

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
  modal = document.getElementById('dlDetailModal');
  const close = () => {
    modal.classList.add('hidden');
    delete modal.dataset.taskId;
  };
  document.getElementById('dlDetailClose')?.addEventListener('click', close);
  modal?.addEventListener('click', (event) => {
    if (event.target === modal) close();
  });
  return modal;
}

function openDownloadDetail(taskId, title, stagesContainer) {
  const modal = ensureDownloadDetailModal();
  if (!modal) return;
  modal.dataset.taskId = taskId;
  const titleEl = modal.querySelector('.dl-detail-title');
  if (titleEl) titleEl.textContent = `📦 ${title}`;
  const modalStages = document.getElementById('dlDetailStages');
  if (modalStages) modalStages.innerHTML = stagesContainer?.innerHTML || '';
  modal.classList.remove('hidden');
}

function syncDownloadDetail(taskId, stagesContainer) {
  const modal = document.getElementById('dlDetailModal');
  if (!modal || modal.classList.contains('hidden') || modal.dataset.taskId !== taskId) return;
  const modalStages = document.getElementById('dlDetailStages');
  if (modalStages) modalStages.innerHTML = stagesContainer?.innerHTML || '';
}

function closeDownloadDetail(taskId) {
  const modal = document.getElementById('dlDetailModal');
  if (!modal || (taskId && modal.dataset.taskId !== taskId)) return;
  modal.classList.add('hidden');
  delete modal.dataset.taskId;
}

function createInstallProgressCard(container, taskId, fileName) {
  if (!container) return null;
  container.querySelector('.dl-active-empty')?.remove();
  if (!container.querySelector('.dl-section-title')) {
    container.insertAdjacentHTML('afterbegin', '<h2 class="dl-section-title">⏳ 活跃下载</h2>');
  }
  container.insertAdjacentHTML('beforeend', `
    <div class="dl-progress-card" id="${taskId}">
      <div class="dl-progress-card-header">
        <div class="dl-progress-name">📦 ${escapeHtml(fileName)}</div>
        <button class="dl-cancel-btn" id="${taskId}-cancel" title="取消下载">✕</button>
      </div>
      <div class="dl-progress-summary" id="${taskId}-summary">
        <span class="dl-summary-text" id="${taskId}-summary-text">准备中...</span>
      </div>
      <div class="dl-progress-bar-wrap" style="height:4px;">
        <div class="dl-progress-bar" id="${taskId}-main-bar" style="width:0%"></div>
      </div>
      <div class="dl-progress-stages" id="${taskId}-stages" style="display:none"></div>
    </div>
  `);
  return document.getElementById(taskId);
}

function removeInstallProgressCard(container, taskId) {
  document.getElementById(taskId)?.remove();
  closeDownloadDetail(taskId);
  if (container && !container.querySelector('.dl-progress-card')) {
    container.innerHTML = `
      <h2 class="dl-section-title">⏳ 活跃下载</h2>
      <div class="dl-active-empty">暂无下载任务</div>
    `;
  }
}

function createInstallCancelController(options) {
  const {
    tauri,
    fileName,
    gameDir,
    getCancelButton,
    getStatusElement,
    requireBackendReady = false,
    logPrefix = 'install',
  } = options;
  let retryTimer = null;
  let inFlight = false;
  let settled = false;
  let backendReady = !requireBackendReady;

  const markBackendReady = () => {
    backendReady = true;
    const button = getCancelButton?.();
    if (button && !inFlight && !settled) button.disabled = false;
  };

  const settle = () => {
    settled = true;
    if (retryTimer) {
      clearTimeout(retryTimer);
      retryTimer = null;
    }
  };

  const requestCancel = async (attempt = 0) => {
    if (settled || (inFlight && attempt === 0)) return;
    inFlight = true;
    const button = getCancelButton?.();
    const status = getStatusElement?.();
    if (button) {
      button.disabled = true;
      button.textContent = '…';
    }
    if (status) status.textContent = '正在取消...';
    try {
      await tauri.core.invoke('cancel_modpack_install', { fileName, gameDir });
    } catch (error) {
      const missingTask = /未找到.*(?:安装)?任务|任务.*不存在|not found/i.test(String(error || ''));
      if (missingTask && attempt < 8 && !settled) {
        retryTimer = setTimeout(() => requestCancel(attempt + 1), 150);
        return;
      }
      inFlight = false;
      if (button) {
        button.disabled = requireBackendReady && !backendReady;
        button.textContent = '✕';
      }
      if (status) status.textContent = `取消失败: ${error}`;
      console.warn(`[${logPrefix}] 取消安装失败:`, error);
    }
  };

  return {
    markBackendReady,
    requestCancel,
    settle,
    isSettled: () => settled,
  };
}

function renderInstallProgress(options) {
  const { taskId, stage, current, total, detail, statusElement, mainBar, stagesContainer } = options;
  const label = STAGE_LABELS[stage] || stage;
  const hasTotal = Number(total) > 0;
  const progressText = hasTotal ? formatStageProgress(current, total, stage) : '';
  const percent = hasTotal
    ? Math.min(100, Math.round((Number(current) / Number(total)) * 100))
    : 0;

  if (statusElement) statusElement.textContent = hasTotal ? `${label} ${progressText}` : label;
  if (mainBar && hasTotal) mainBar.style.width = `${percent}%`;
  if (!stagesContainer) return { label, progressText, percent };

  const stageId = `${taskId}-stage-${stage}`;
  let row = document.getElementById(stageId);
  if (!row) {
    stagesContainer.insertAdjacentHTML('beforeend', `
      <div class="dl-stage-row" id="${stageId}">
        <div class="dl-stage-head">
          <span class="dl-stage-label">${escapeHtml(label)}</span>
          <span class="dl-stage-count" id="${stageId}-count"></span>
        </div>
        <div class="dl-progress-bar-wrap" style="height:4px;">
          <div class="dl-progress-bar" id="${stageId}-bar" style="width:0%"></div>
        </div>
      </div>
    `);
    row = document.getElementById(stageId);
  }
  const countElement = document.getElementById(`${stageId}-count`);
  const stageBar = document.getElementById(`${stageId}-bar`);
  if (hasTotal) {
    if (countElement) countElement.textContent = progressText;
    if (stageBar) {
      stageBar.style.width = `${percent}%`;
      if (percent >= 100) stageBar.style.opacity = '0.5';
    }
  } else if (countElement) {
    countElement.textContent = detail || '';
  }
  syncDownloadDetail(taskId, stagesContainer);
  return { label, progressText, percent };
}

function renderInstallTerminal(options) {
  const { taskId, stage, detail, statusElement, mainBar, stagesContainer, cancelButton } = options;
  const done = stage === 'done';
  const cancelled = stage === 'cancelled';
  const icon = done ? '✅' : (cancelled ? '🚫' : '❌');
  const message = done ? '安装完成！' : (cancelled ? '已取消' : String(detail || '安装失败'));
  if (statusElement) statusElement.textContent = `${icon} ${message}`;
  if (mainBar) {
    mainBar.style.width = done ? '100%' : '0%';
    if (done) mainBar.style.opacity = '0.5';
  }
  if (cancelButton) cancelButton.style.display = 'none';
  if (stagesContainer) {
    const color = done ? '' : ' style="color:#ef4444"';
    stagesContainer.innerHTML = `<div class="dl-stage-row"><span class="dl-stage-label"${color}>${icon} ${escapeHtml(message)}</span></div>`;
    syncDownloadDetail(taskId, stagesContainer);
  }
  return { done, cancelled, icon, message };
}

// 注入确认弹窗样式（仅一次）
(function injectConfirmStyles() {
  if (document.getElementById('oaoiConfirmStyles')) return;
  const style = document.createElement('style');
  style.id = 'oaoiConfirmStyles';
  style.textContent = `
    .oaoi-confirm-overlay.oaoi-modal-host {
      --modal-z: 99998;
      --modal-width: fit-content;
    }
    .oaoi-confirm-card.oaoi-modal-card {
      min-width: 280px;
      max-width: min(420px, calc(100vw - 48px));
    }
    .oaoi-confirm-card.oaoi-update-confirm {
      width: 460px;
      max-width: calc(100vw - 48px);
      max-height: calc(100vh - 48px);
    }
    .oaoi-confirm-card.oaoi-alert-confirm {
      max-height: calc(100vh - 48px);
    }
    .oaoi-confirm-header {
      display: flex;
      align-items: center;
      gap: 10px;
      padding: 18px 20px 8px;
    }
    .oaoi-confirm-icon {
      font-size: 22px;
    }
    .oaoi-confirm-title {
      font-size: 15px;
      font-weight: 700;
      color: var(--text-dark, #4a2030);
    }
    .oaoi-confirm-body {
      padding: 8px 20px 18px;
      font-size: 13px;
      line-height: 1.6;
      color: var(--text-mid, #8a5070);
      word-break: break-word;
      white-space: pre-line;
    }
    .oaoi-confirm-card.oaoi-update-confirm .oaoi-confirm-body {
      max-height: min(260px, calc(100vh - 190px));
      overflow-y: auto;
      overscroll-behavior: contain;
    }
    .oaoi-confirm-card.oaoi-alert-confirm .oaoi-confirm-body {
      max-height: min(260px, calc(100vh - 160px));
      overflow-y: auto;
      overscroll-behavior: contain;
    }
    .oaoi-confirm-actions {
      display: flex;
      gap: 10px;
      padding: 0 20px 18px;
      justify-content: flex-end;
    }
    .oaoi-confirm-btn {
      padding: 8px 22px;
      border: none;
      border-radius: 10px;
      font-size: 13px;
      font-weight: 600;
      cursor: pointer;
      transition: all 0.2s;
    }
    .oaoi-confirm-btn.cancel {
      background: rgba(0, 0, 0, 0.05);
      color: var(--text-mid, #8a5070);
    }
    .oaoi-confirm-btn.cancel:hover {
      background: rgba(0, 0, 0, 0.1);
    }
    .oaoi-confirm-btn.confirm {
      background: var(--rose-gradient, linear-gradient(135deg, #ff8fab, #ff6b8a, #e84574));
      color: white;
      box-shadow: 0 3px 12px var(--theme-accent-border, rgba(232, 69, 116, 0.3));
    }
    .oaoi-confirm-btn.confirm:hover {
      transform: translateY(-1px);
      box-shadow: 0 5px 18px var(--theme-accent-border, rgba(232, 69, 116, 0.4));
    }
    .oaoi-confirm-btn.confirm:active {
      transform: translateY(0);
    }
    .oaoi-confirm-btn:focus-visible {
      outline: 2px solid var(--pink-400, #ff8fab);
      outline-offset: 2px;
    }
  `;
  document.head.appendChild(style);
})();
