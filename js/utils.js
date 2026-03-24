// ============ 全局共享工具函数 ============

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
  libraries: '依赖库', assets: '资源文件', mods: 'Mod 文件',
  resourcepacks: '材质包', shaderpacks: '光影包', config: '配置文件', other: '其他文件',
  java: 'Java 环境', loader: '加载器', forge: 'Forge',
  neoforge: 'NeoForge', fabric: 'Fabric', quilt: 'Quilt',
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

/**
 * 用 tauri 打开外部 URL，失败时 fallback 到 window.open
 */
async function openExternalUrl(url) {
  try {
    const tauri = await waitForTauri();
    await tauri.core.invoke('open_url', { url });
  } catch {
    window.open(url, '_blank');
  }
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
      background: rgba(255, 255, 255, 0.92);
      backdrop-filter: blur(16px);
      box-shadow: 0 8px 32px rgba(232, 69, 116, 0.15), 0 0 0 1px rgba(255, 200, 215, 0.25);
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
      box-shadow: 0 10px 40px rgba(232, 69, 116, 0.2), 0 0 0 1px rgba(255, 200, 215, 0.35);
    }
    .oaoi-toast-warn {
      border-left: 4px solid #f59e0b;
    }
    .oaoi-toast-error {
      border-left: 4px solid #ef4444;
      background: rgba(255, 245, 245, 0.95);
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
      background: rgba(232, 69, 116, 0.08);
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
  const { title = '确认', confirmText = '确定', cancelText = '取消', kind = 'warning' } = options;

  return new Promise((resolve) => {
    const overlay = document.createElement('div');
    overlay.className = 'oaoi-confirm-overlay';
    overlay.innerHTML = `
      <div class="oaoi-confirm-card">
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

    function close(result) {
      overlay.querySelector('.oaoi-confirm-card').classList.add('oaoi-confirm-out');
      overlay.classList.add('oaoi-confirm-overlay-out');
      setTimeout(() => { overlay.remove(); resolve(result); }, 200);
    }

    overlay.querySelector('.oaoi-confirm-btn.cancel').addEventListener('click', () => close(false));
    overlay.querySelector('.oaoi-confirm-btn.confirm').addEventListener('click', () => close(true));
    overlay.addEventListener('click', (e) => { if (e.target === overlay) close(false); });

    // ESC 取消
    function onKey(e) { if (e.key === 'Escape') { close(false); document.removeEventListener('keydown', onKey); } }
    document.addEventListener('keydown', onKey);

    // 自动聚焦确认按钮
    requestAnimationFrame(() => overlay.querySelector('.oaoi-confirm-btn.confirm').focus());
  });
}

// 注入确认弹窗样式（仅一次）
(function injectConfirmStyles() {
  if (document.getElementById('oaoiConfirmStyles')) return;
  const style = document.createElement('style');
  style.id = 'oaoiConfirmStyles';
  style.textContent = `
    .oaoi-confirm-overlay {
      position: fixed;
      inset: 0;
      z-index: 99998;
      background: rgba(0, 0, 0, 0.3);
      backdrop-filter: blur(4px);
      display: flex;
      align-items: center;
      justify-content: center;
      animation: confirmOverlayIn 0.2s ease;
    }
    .oaoi-confirm-overlay-out {
      animation: confirmOverlayOut 0.2s ease forwards;
    }
    @keyframes confirmOverlayIn {
      from { opacity: 0; }
      to   { opacity: 1; }
    }
    @keyframes confirmOverlayOut {
      to { opacity: 0; }
    }
    .oaoi-confirm-card {
      background: linear-gradient(135deg, #fff5f7 0%, #ffffff 100%);
      border-radius: 18px;
      width: 340px;
      max-width: 88vw;
      box-shadow: 0 16px 48px rgba(232, 69, 116, 0.18), 0 0 0 1px rgba(255, 200, 215, 0.3);
      overflow: hidden;
      animation: confirmCardIn 0.3s cubic-bezier(0.34, 1.56, 0.64, 1);
    }
    .oaoi-confirm-out {
      animation: confirmCardOut 0.2s ease forwards;
    }
    @keyframes confirmCardIn {
      from { opacity: 0; transform: scale(0.92) translateY(12px); }
      to   { opacity: 1; transform: scale(1) translateY(0); }
    }
    @keyframes confirmCardOut {
      to { opacity: 0; transform: scale(0.95) translateY(8px); }
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
      box-shadow: 0 3px 12px rgba(232, 69, 116, 0.3);
    }
    .oaoi-confirm-btn.confirm:hover {
      transform: translateY(-1px);
      box-shadow: 0 5px 18px rgba(232, 69, 116, 0.4);
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
