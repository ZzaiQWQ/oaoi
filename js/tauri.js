/* ============================================
   oaoi - 启动器交互脚本
   ============================================ */

// ============ Tauri 窗口控制 ============
// 等待 __TAURI__ API 加载（可能需要一点时间）
function waitForTauri(maxRetries = 20) {
  return new Promise((resolve, reject) => {
    let retries = 0;
    function check() {
      if (window.__TAURI__) {
        resolve(window.__TAURI__);
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

    // 全窗口拖拽：除了按钮、链接、输入框等可交互元素
    const interactiveSelector = 'button, a, input, select, textarea, .win-btn, .launch-btn, .nav-item, .news-card, .window-controls, .java-results-header, .java-result-item, .set-toggle, .set-theme-item, .set-path-input, .set-player-input, .srv-card, [data-toggle], .modal-overlay, .modal, .loader-radio-btn, .form-group, .custom-select, .btn';
    document.body.addEventListener('mousedown', async (e) => {
      // 如果点击的是可交互元素，不触发拖拽
      if (e.target.closest(interactiveSelector)) return;
      await appWindow.startDragging();
    });

    console.log('🖥️ Tauri 窗口控制已初始化（全窗口拖拽）');
  } catch (e) {
    console.log('⚠️ Tauri API 未加载:', e.message);
    // 不隐藏按钮，保留 UI
  }
}
class SakuraPetals {
  constructor(container, count = 25) {
    this.container = container;
    this.count = count;
    this.petals = ['🌸', '✿', '❀', '💮'];
    this.init();
  }

  init() {
    for (let i = 0; i < this.count; i++) {
      setTimeout(() => this.createPetal(), i * 400);
    }
  }

  createPetal() {
    const petal = document.createElement('div');
    petal.className = 'sakura-petal';
    petal.textContent = this.petals[Math.floor(Math.random() * this.petals.length)];

    const startX = Math.random() * 100;
    const size = 12 + Math.random() * 14;
    const duration = 8 + Math.random() * 10;
    const delay = Math.random() * 3;
    const swayAmount = 40 + Math.random() * 80;

    petal.style.cssText = `
      left: ${startX}%;
      font-size: ${size}px;
      animation-duration: ${duration}s;
      animation-delay: ${delay}s;
      opacity: ${0.4 + Math.random() * 0.4};
    `;

    // 添加自定义摇摆动画
    petal.style.setProperty('--sway', `${swayAmount}px`);

    this.container.appendChild(petal);

    // 动画结束后重新创建
    petal.addEventListener('animationend', () => {
      petal.remove();
      this.createPetal();
    });
  }
}