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
      // 原生可交互元素 + data-no-drag 容器内的元素，均不触发拖拽
      if (e.target.closest('button, a, input, select, textarea, [data-no-drag]')) return;
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
// 右键菜单
document.addEventListener('contextmenu', e => e.preventDefault());
// 禁止选中文字（input/textarea 除外）
document.addEventListener('selectstart', e => {
  if (e.target.closest('input, textarea')) return;
  e.preventDefault();
});
// 禁止拖拽图片
document.addEventListener('dragstart', e => e.preventDefault());
// 仅生产模式下禁用 F12 / DevTools 快捷键
if (!location.hostname.includes('localhost') && !location.port) {
  document.addEventListener('keydown', e => {
    if (e.key === 'F12' || (e.ctrlKey && e.shiftKey && e.key === 'I') || (e.ctrlKey && e.shiftKey && e.key === 'J') || (e.ctrlKey && e.key === 'u')) {
      e.preventDefault();
    }
  });
}