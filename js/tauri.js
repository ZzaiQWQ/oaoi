/* ============================================
   oaoi - 启动器交互脚本
   ============================================ */

// ============ Tauri 窗口控制 ============
// 等待 __TAURI__ API 加载（可能需要一点时间）
let _tauriCache = null;
function waitForTauri(maxRetries = 20) {
  if (_tauriCache) return Promise.resolve(_tauriCache);
  return new Promise((resolve, reject) => {
    let retries = 0;
    function check() {
      if (window.__TAURI__) {
        _tauriCache = window.__TAURI__;
        resolve(_tauriCache);
      } else if (retries < maxRetries) {
        retries++;
        setTimeout(check, 100);
      } else {
        reject(new Error('Tauri API 未加载'));
      }
    }
    check();
  });
}

async function initWindowControls() {
  try {
    const tauri = await waitForTauri();
    const appWindow = tauri.window.getCurrentWindow();

    // 窗口控制按钮
    document.getElementById('winMinimize')?.addEventListener('click', (e) => {
      e.stopPropagation();
      appWindow.minimize();
    });
    document.getElementById('winMaximize')?.addEventListener('click', async (e) => {
      e.stopPropagation();
      const isMaximized = await appWindow.isMaximized();
      isMaximized ? appWindow.unmaximize() : appWindow.maximize();
    });
    document.getElementById('winClose')?.addEventListener('click', (e) => {
      e.stopPropagation();
      appWindow.close();
    });

    // DIY 恢复默认窗口大小（跟上面三个按钮同模式）
    document.getElementById('diyResetWindowSize')?.addEventListener('click', async (e) => {
      e.stopPropagation();
      await appWindow.setSize(new tauri.window.LogicalSize(800, 480));
      await appWindow.center();
      localStorage.removeItem('diy_windowWidth');
      localStorage.removeItem('diy_windowHeight');
    });

    // 全窗口拖拽：除了原生交互元素和标记了 data-no-drag 的容器
    document.body.addEventListener('mousedown', async (e) => {
      if (e.button !== 0) return;
      const nativeNoDrag = 'button, a, input, select, textarea, [contenteditable="true"]';
      const diySurface = e.target.closest('#pageDiy .diy-scroll');
      const diyControl = e.target.closest([
        nativeNoDrag,
        '.diy-bg-item',
        '.diy-row',
        '.diy-slider-row',
        '.diy-color-row',
        '.diy-style-grid',
        '.diy-theme-grid',
        '.diy-reset-btn'
      ].join(', '));

      // DIY 滚动区本身有 data-no-drag，单独放开空白和标题区域用于拖动窗口。
      if (diySurface && !diyControl) {
        await appWindow.startDragging();
        return;
      }

      // 原生可交互元素 + data-no-drag 容器内的元素，均不触发拖拽
      if (e.target.closest(`${nativeNoDrag}, [data-no-drag]`)) return;
      await appWindow.startDragging();
    });

    // ===== 窗口大小记忆 =====
    // 启动时恢复上次保存的窗口大小
    const savedW = localStorage.getItem('diy_windowWidth');
    const savedH = localStorage.getItem('diy_windowHeight');
    if (savedW && savedH) {
      const w = parseInt(savedW, 10);
      const h = parseInt(savedH, 10);
      if (w >= 400 && h >= 225) {
        await appWindow.setSize(new tauri.window.LogicalSize(w, h));
        await appWindow.center();
      }
    }

    // resize 时防抖保存窗口大小
    let resaveTimer = null;
    appWindow.listen('tauri://resize', async () => {
      clearTimeout(resaveTimer);
      resaveTimer = setTimeout(async () => {
        try {
          const factor = await appWindow.scaleFactor();
          const phys = await appWindow.innerSize();
          const w = Math.round(phys.width / factor);
          const h = Math.round(phys.height / factor);
          localStorage.setItem('diy_windowWidth', w);
          localStorage.setItem('diy_windowHeight', h);
        } catch (e) { console.warn('[tauri] 保存窗口尺寸失败:', e); }
      }, 500);
    });

    console.log('🖥️ Tauri 窗口控制已初始化（全窗口拖拽 + 尺寸记忆）');

    // 最小化/失焦时：注入全局样式冻结所有动画和 GPU 渲染
    const freezeStyle = document.createElement('style');
    freezeStyle.id = 'freeze-on-hide';
    const freezeCSS = `
      *, *::before, *::after {
        animation-play-state: paused !important;
        transition: none !important;
      }
      :root {
        --blur-radius: 0px !important;
      }
    `;

    // 用 Tauri 原生窗口事件（visibilitychange 在 Tauri 最小化时可能不触发）
    appWindow.listen('tauri://blur', () => {
      if (window._blurPauseEnabled === false) return;
      freezeStyle.textContent = freezeCSS;
      if (!freezeStyle.parentNode) document.head.appendChild(freezeStyle);
    });
    appWindow.listen('tauri://focus', () => {
      freezeStyle.remove();
    });

  } catch (e) {
    console.log('⚠️ Tauri API 未加载:', e.message);
    // 不隐藏按钮，保留 UI
  }
}

// ===== 禁用浏览器行为 =====
function isEditableTarget(target) {
  return target && target.closest && target.closest('input, textarea, [contenteditable="true"]');
}

let oaoiTextContextTarget = null;
let oaoiTextContextMenu = null;

function getTextContextMenu() {
  if (oaoiTextContextMenu) return oaoiTextContextMenu;
  const menu = document.createElement('div');
  menu.id = 'oaoiTextContextMenu';
  menu.innerHTML = `
    <button type="button" data-action="copy">复制</button>
    <button type="button" data-action="paste">粘贴</button>
    <button type="button" data-action="cut">剪切</button>
  `;
  const style = document.createElement('style');
  style.textContent = `
    #oaoiTextContextMenu {
      position: fixed;
      z-index: 999999;
      display: none;
      min-width: 92px;
      padding: 5px;
      border-radius: 12px;
      border: 1px solid var(--theme-panel-border, rgba(255, 133, 169, 0.28));
      background: var(--modal-surface-white-96, rgba(255, 255, 255, 0.96));
      box-shadow: 0 12px 28px var(--theme-accent-border, rgba(255, 83, 134, 0.2));
      backdrop-filter: blur(12px);
      -webkit-user-select: none;
      user-select: none;
    }
    #oaoiTextContextMenu button {
      display: block;
      width: 100%;
      height: 28px;
      border: 0;
      border-radius: 8px;
      background: transparent;
      color: var(--theme-drop-status, #8a3d59);
      font: inherit;
      font-size: 12px;
      font-weight: 800;
      text-align: left;
      padding: 0 10px;
      cursor: pointer;
    }
    #oaoiTextContextMenu button:hover {
      background: var(--theme-accent-soft, rgba(255, 107, 138, 0.14));
      color: var(--theme-accent-strong, #e84574);
    }
  `;
  document.head.appendChild(style);
  document.body.appendChild(menu);
  menu.addEventListener('click', async e => {
    const button = e.target.closest('button[data-action]');
    if (!button || !oaoiTextContextTarget) return;
    await runTextContextAction(button.dataset.action, oaoiTextContextTarget);
    hideTextContextMenu();
  });
  oaoiTextContextMenu = menu;
  return menu;
}

function hideTextContextMenu() {
  if (oaoiTextContextMenu) oaoiTextContextMenu.style.display = 'none';
}

function showTextContextMenu(x, y, target) {
  oaoiTextContextTarget = target;
  const menu = getTextContextMenu();
  menu.style.display = 'block';
  const rect = menu.getBoundingClientRect();
  menu.style.left = `${Math.max(8, Math.min(x, window.innerWidth - rect.width - 8))}px`;
  menu.style.top = `${Math.max(8, Math.min(y, window.innerHeight - rect.height - 8))}px`;
}

async function runTextContextAction(action, target) {
  target.focus();
  if (action === 'paste') {
    try {
      const text = await navigator.clipboard.readText();
      insertTextAtCursor(target, text);
    } catch {
      document.execCommand('paste');
    }
    return;
  }
  document.execCommand(action);
}

function insertTextAtCursor(target, text) {
  if (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement) {
    const start = target.selectionStart ?? target.value.length;
    const end = target.selectionEnd ?? target.value.length;
    target.setRangeText(text, start, end, 'end');
    target.dispatchEvent(new Event('input', { bubbles: true }));
    return;
  }
  document.execCommand('insertText', false, text);
}


// 右键菜单：输入框只显示复制/粘贴/剪切，其他地方不显示
document.addEventListener('contextmenu', e => {
  e.preventDefault();
  const editable = isEditableTarget(e.target);
  if (!editable) {
    hideTextContextMenu();
    return;
  }
  showTextContextMenu(e.clientX, e.clientY, editable);
}, true);
// 禁止选中文字（input/textarea 除外）
document.addEventListener('selectstart', e => {
  if (isEditableTarget(e.target)) return;
  e.preventDefault();
}, true);
// 禁止拖拽图片
document.addEventListener('dragstart', e => e.preventDefault(), true);
// 禁止复制和剪切
document.addEventListener('copy', e => {
  if (isEditableTarget(e.target)) return;
  e.preventDefault();
}, true);
document.addEventListener('cut', e => {
  if (isEditableTarget(e.target)) return;
  e.preventDefault();
}, true);
document.addEventListener('keydown', e => {
  if (isEditableTarget(e.target)) return;
  const key = String(e.key || '').toLowerCase();
  if ((e.ctrlKey || e.metaKey) && (key === 'c' || key === 'x')) {
    e.preventDefault();
  }
}, true);
document.addEventListener('click', hideTextContextMenu, true);
window.addEventListener('blur', hideTextContextMenu);
window.addEventListener('scroll', hideTextContextMenu, true);
// 仅生产模式下禁用 F12 / DevTools 快捷键
if (!location.hostname.includes('localhost') && !location.port) {
  document.addEventListener('keydown', e => {
    if (e.key === 'F12' || (e.ctrlKey && e.shiftKey && e.key === 'I') || (e.ctrlKey && e.shiftKey && e.key === 'J') || (e.ctrlKey && e.key === 'u')) {
      e.preventDefault();
    }
  });
}
