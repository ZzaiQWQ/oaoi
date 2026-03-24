// ============ DIY 个性化页面逻辑 ============

function initDiyPage() {
  const defaults = {
    bgImage: 'assets/bg.png',
    sakuraEnabled: true,
    blurPause: true,
    sakuraCount: 15,
    petalColor: '#ffb3c6',
    petalStyle: 'sakura',
    themeColor: 'pink',
    windowOpacity: 68,
    windowRadius: 16,
    blurRadius: 1
  };

  function getSetting(key) {
    const val = localStorage.getItem('diy_' + key);
    if (val === null) return defaults[key];
    if (val === 'true') return true;
    if (val === 'false') return false;
    const num = Number(val);
    return isNaN(num) ? val : num;
  }

  function setSetting(key, val) {
    localStorage.setItem('diy_' + key, val);
  }

  // ====== 背景图选择 ======
  const bgItems = document.querySelectorAll('.diy-bg-item[data-bg]');
  const currentBg = getSetting('bgImage');
  bgItems.forEach(item => {
    if (item.dataset.bg === currentBg) item.classList.add('active');
    item.addEventListener('click', () => {
      bgItems.forEach(i => i.classList.remove('active'));
      item.classList.add('active');
      setSetting('bgImage', item.dataset.bg);
      applyBackground(item.dataset.bg);
    });
  });

  // 自定义背景
  const customBgBtn = document.getElementById('diyCustomBg');
  if (customBgBtn) {
    customBgBtn.addEventListener('click', async () => {
      try {
        const tauri = await waitForTauri();
        const result = await tauri.dialog.open({
          title: '选择背景图片',
          filters: [{ name: '图片', extensions: ['png', 'jpg', 'jpeg', 'webp', 'gif'] }]
        });
        if (result) {
          const path = typeof result === 'string' ? result : result.path;
          if (path) {
            const url = tauri.core.convertFileSrc(path);
            setSetting('bgImage', url);
            setSetting('bgCustomPath', path);
            bgItems.forEach(i => i.classList.remove('active'));
            customBgBtn.classList.add('active');
            applyBackground(url);
          }
        }
      } catch (e) {
        console.log('⚠️ 选择背景失败:', e);
      }
    });
    if (currentBg && !currentBg.startsWith('assets/')) {
      bgItems.forEach(i => i.classList.remove('active'));
      customBgBtn.classList.add('active');
    }
  }

  // ====== 樱花开关 ======
  const sakuraToggle = document.getElementById('diySakuraToggle');
  if (sakuraToggle) {
    const enabled = getSetting('sakuraEnabled');
    if (enabled) sakuraToggle.classList.add('on');
    else sakuraToggle.classList.remove('on');
    sakuraToggle.addEventListener('click', () => {
      const isOn = sakuraToggle.classList.toggle('on');
      setSetting('sakuraEnabled', isOn);
      applySakura(isOn);
    });
  }

  // ====== 失焦暂停 ======
  const blurPauseToggle = document.getElementById('diyBlurPauseToggle');
  if (blurPauseToggle) {
    const blurEnabled = getSetting('blurPause');
    if (blurEnabled) blurPauseToggle.classList.add('on');
    else blurPauseToggle.classList.remove('on');
    blurPauseToggle.addEventListener('click', () => {
      const isOn = blurPauseToggle.classList.toggle('on');
      setSetting('blurPause', isOn);
      applyBlurPause(isOn);
    });
  }

  // ====== 花瓣数量 ======
  const petalSlider = document.getElementById('diyPetalCount');
  const petalVal = document.getElementById('diyPetalCountVal');
  if (petalSlider && petalVal) {
    petalSlider.value = getSetting('sakuraCount');
    petalVal.textContent = petalSlider.value;
    petalSlider.addEventListener('input', () => {
      petalVal.textContent = petalSlider.value;
      setSetting('sakuraCount', petalSlider.value);
    });
    petalSlider.addEventListener('change', () => {
      applySakuraCount(parseInt(petalSlider.value));
    });
  }

  // ====== 花瓣颜色 ======
  const petalColorInput = document.getElementById('diyPetalColor');
  if (petalColorInput) {
    petalColorInput.value = getSetting('petalColor');
    petalColorInput.addEventListener('input', () => {
      setSetting('petalColor', petalColorInput.value);
      applyPetalColor(petalColorInput.value);
    });
  }

  // ====== 花瓣样式 ======
  const styleBtns = document.querySelectorAll('.diy-style-btn');
  const currentStyle = getSetting('petalStyle');
  styleBtns.forEach(btn => {
    if (btn.dataset.style === currentStyle) {
      styleBtns.forEach(b => b.classList.remove('active'));
      btn.classList.add('active');
    }
    btn.addEventListener('click', () => {
      styleBtns.forEach(b => b.classList.remove('active'));
      btn.classList.add('active');
      setSetting('petalStyle', btn.dataset.style);
      applyPetalStyle(btn.dataset.style);
    });
  });

  // ====== 主题色 ======
  const themeBtns = document.querySelectorAll('.diy-theme-btn');
  const currentTheme = getSetting('themeColor');
  themeBtns.forEach(btn => {
    if (btn.dataset.theme === currentTheme) {
      themeBtns.forEach(b => b.classList.remove('active'));
      btn.classList.add('active');
    }
    btn.addEventListener('click', () => {
      themeBtns.forEach(b => b.classList.remove('active'));
      btn.classList.add('active');
      setSetting('themeColor', btn.dataset.theme);
      applyTheme(btn.dataset.theme);
    });
  });


  // ====== 背景模糊 ======
  const blurSlider = document.getElementById('diyBlurRadius');
  const blurVal = document.getElementById('diyBlurRadiusVal');
  if (blurSlider && blurVal) {
    blurSlider.value = getSetting('blurRadius');
    blurVal.textContent = blurSlider.value + 'px';
    blurSlider.addEventListener('input', () => {
      blurVal.textContent = blurSlider.value + 'px';
      setSetting('blurRadius', blurSlider.value);
      applyBlurRadius(parseInt(blurSlider.value));
    });
  }

  // ====== 窗口透明度 ======
  const opacitySlider = document.getElementById('diyOpacity');
  const opacityVal = document.getElementById('diyOpacityVal');
  if (opacitySlider && opacityVal) {
    opacitySlider.value = getSetting('windowOpacity');
    opacityVal.textContent = opacitySlider.value + '%';
    opacitySlider.addEventListener('input', () => {
      opacityVal.textContent = opacitySlider.value + '%';
      setSetting('windowOpacity', opacitySlider.value);
      applyOpacity(parseInt(opacitySlider.value));
    });
  }

  // ====== 窗口圆角 ======
  const radiusSlider = document.getElementById('diyRadius');
  const radiusVal = document.getElementById('diyRadiusVal');
  if (radiusSlider && radiusVal) {
    radiusSlider.value = getSetting('windowRadius');
    radiusVal.textContent = radiusSlider.value + 'px';
    radiusSlider.addEventListener('input', () => {
      radiusVal.textContent = radiusSlider.value + 'px';
      setSetting('windowRadius', radiusSlider.value);
      applyRadius(parseInt(radiusSlider.value));
    });
  }

  // ====== 重置按钮 ======
  const resetBtn = document.getElementById('diyResetBtn');
  if (resetBtn) {
    resetBtn.addEventListener('click', () => {
      Object.keys(defaults).forEach(k => localStorage.removeItem('diy_' + k));
      localStorage.removeItem('diy_bgCustomPath');
      location.reload();
    });
  }

  // ============ 应用函数 ============
  function applyBackground(path) {
    const globalBg = document.getElementById('globalBgImg');
    if (globalBg) globalBg.src = path;
    const heroBg = document.querySelector('.hero-bg');
    if (heroBg) heroBg.src = path;
  }

  function applySakura(enabled) {
    const container = document.getElementById('sakuraContainer');
    if (container) container.style.display = enabled ? '' : 'none';
  }

  function applySakuraCount(count) {
    const container = document.getElementById('sakuraContainer');
    if (!container) return;
    // 先销毁旧实例，防止内存泄漏
    if (window._sakuraInstance) {
      window._sakuraInstance.destroy();
      window._sakuraInstance = null;
    }
    container.innerHTML = '';
    if (typeof SakuraPetals !== 'undefined') {
      window._sakuraInstance = new SakuraPetals(container, count);
    }
    setTimeout(() => {
      applyPetalColor(getSetting('petalColor'));
      applyPetalStyle(getSetting('petalStyle'));
    }, 600);
  }

  function applyPetalColor(color) {
    document.querySelectorAll('.sakura-petal').forEach(p => p.style.color = color);
  }

  const petalStyles = {
    sakura: ['🌸', '✿', '❀', '💮'],
    leaf: ['🍃', '🍂', '🌿', '☘️'],
    snow: ['❄️', '❅', '❆', '✦'],
    star: ['⭐', '✨', '💫', '🌟']
  };

  function applyPetalStyle(style) {
    const emojis = petalStyles[style] || petalStyles.sakura;
    document.querySelectorAll('.sakura-petal').forEach(p => {
      p.textContent = emojis[Math.floor(Math.random() * emojis.length)];
    });
    // 把样式存到容器 dataset 上，新创建的花瓣也会用到
    const container = document.getElementById('sakuraContainer');
    if (container) container.dataset.petalStyle = style;
  }

  const themes = {
    pink: { h: '#ff6b8a', l: '#fff0f3', m: '#ffd6e0', d: '#ffb3c6', s: '#e84574', bg: 'rgba(255,240,243,0.92)', grad: 'linear-gradient(135deg,#ff8fab,#ff6b8a,#e84574)' },
    blue: { h: '#6ba3ff', l: '#f0f5ff', m: '#d0e0ff', d: '#a0c4ff', s: '#3b7ddd', bg: 'rgba(240,245,255,0.92)', grad: 'linear-gradient(135deg,#a0c4ff,#6ba3ff,#3b7ddd)' },
    purple: { h: '#a06bff', l: '#f5f0ff', m: '#ddd0ff', d: '#c4a0ff', s: '#7b45e8', bg: 'rgba(245,240,255,0.92)', grad: 'linear-gradient(135deg,#c4a0ff,#a06bff,#7b45e8)' },
    green: { h: '#6bcf7f', l: '#f0fff3', m: '#d0ffd6', d: '#a0e8b3', s: '#45b85a', bg: 'rgba(240,255,243,0.92)', grad: 'linear-gradient(135deg,#a0e8b3,#6bcf7f,#45b85a)' },
    orange: { h: '#ffa06b', l: '#fff5f0', m: '#ffe0d0', d: '#ffc4a0', s: '#e87445', bg: 'rgba(255,245,240,0.92)', grad: 'linear-gradient(135deg,#ffc4a0,#ffa06b,#e87445)' }
  };

  function applyTheme(name) {
    const t = themes[name] || themes.pink;
    const r = document.documentElement;
    r.style.setProperty('--pink-100', t.l);
    r.style.setProperty('--pink-200', t.m);
    r.style.setProperty('--pink-300', t.d);
    r.style.setProperty('--pink-400', t.h);
    r.style.setProperty('--pink-500', t.h);
    r.style.setProperty('--pink-600', t.s);
    r.style.setProperty('--pink-700', t.s);
    r.style.setProperty('--rose-gradient', t.grad);
    r.style.setProperty('--sidebar-bg', t.bg);
    r.style.setProperty('--shadow-pink', `0 4px 20px ${t.h}40`);
  }


  function applyOpacity(val) {
    document.documentElement.style.setProperty('--window-opacity', (val / 100).toString());
  }

  function applyRadius(val) {
    const launcher = document.querySelector('.launcher');
    if (launcher) launcher.style.borderRadius = val + 'px';
  }

  function applyBlurRadius(val) {
    // 只设全局 CSS 变量，所有元素通过 CSS var(--blur-radius) 响应
    document.documentElement.style.setProperty('--blur-radius', val + 'px');
  }

  let blurHandler = null;
  let focusHandler = null;
  function applyBlurPause(enabled) {
    if (blurHandler) window.removeEventListener('blur', blurHandler);
    if (focusHandler) window.removeEventListener('focus', focusHandler);
    blurHandler = null; focusHandler = null;
    if (enabled) {
      blurHandler = () => {
        const c = document.getElementById('sakuraContainer');
        if (c) c.querySelectorAll('.sakura-petal').forEach(p => p.style.animationPlayState = 'paused');
      };
      focusHandler = () => {
        const c = document.getElementById('sakuraContainer');
        if (c) c.querySelectorAll('.sakura-petal').forEach(p => p.style.animationPlayState = 'running');
      };
      window.addEventListener('blur', blurHandler);
      window.addEventListener('focus', focusHandler);
    }
  }

  // ===== 窗口大小实时显示 =====
  const sizeDisplay = document.getElementById('diyWindowSizeDisplay');
  function updateSizeDisplay() {
    if (sizeDisplay) {
      sizeDisplay.textContent = `${window.innerWidth} × ${window.innerHeight}`;
    }
  }
  updateSizeDisplay();
  window.addEventListener('resize', updateSizeDisplay);

  // 初始化应用所有设置
  applyBackground(getSetting('bgImage'));
  applySakura(getSetting('sakuraEnabled'));
  applyBlurPause(getSetting('blurPause'));
  applyPetalColor(getSetting('petalColor'));
  applyPetalStyle(getSetting('petalStyle'));
  applyTheme(getSetting('themeColor'));
  applyOpacity(getSetting('windowOpacity'));
  applyRadius(getSetting('windowRadius'));
  applyBlurRadius(getSetting('blurRadius'));
}

// initDiyPage 由 main.js 的 DOMContentLoaded 统一调用
