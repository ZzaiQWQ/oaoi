// ============ DIY 个性化页面逻辑 ============

function initDiyPage() {
  const defaults = {
    bgImage: 'assets/bg-1.webp',
    sakuraEnabled: true,
    blurPause: true,
    sakuraCount: 15,
    petalColor: '#ffb3c6',
    petalStyle: 'sakura',
    themeColor: 'pink',
    windowOpacity: 68,
    contentSurfaceOpacity: 100,
    modalSurfaceOpacity: 100,
    customBgImage: '',
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
    try {
      localStorage.setItem('diy_' + key, val);
    } catch (e) {
      console.warn('DIY 设置保存失败:', key, e);
    }
  }

  // 清理旧版残留字段，避免旧路径覆盖当前背景设置。
  localStorage.removeItem('diy_bgCustomPath');

  function normalizeImageUrl(path) {
    const raw = String(path || '');
    if (!raw || raw.startsWith('assets/') || raw.startsWith('data:image/')) return raw;
    return defaults.bgImage;
  }

  // ====== 背景图选择 ======
  const bgItems = document.querySelectorAll('.diy-bg-item');
  let currentBg = getSetting('bgImage');
  const customBgBtn = document.getElementById('diyCustomBg');
  const customBgPreview = document.getElementById('diyCustomBgPreview');
  const customFileInput = document.createElement('input');
  customFileInput.type = 'file';
  customFileInput.accept = 'image/png,image/jpeg,image/webp,image/gif';
  customFileInput.hidden = true;
  customFileInput.addEventListener('click', e => e.stopPropagation());
  if (customBgBtn) customBgBtn.appendChild(customFileInput);

  function isBuiltInBg(url) {
    return String(url || '').startsWith('assets/');
  }

  function isStoredCustomBg(url) {
    return String(url || '').startsWith('data:image/');
  }

  function clearBrokenCustomBg() {
    const saved = getSetting('customBgImage');
    if (saved && !isStoredCustomBg(saved)) localStorage.removeItem('diy_customBgImage');
    if (currentBg && !isBuiltInBg(currentBg) && !isStoredCustomBg(currentBg)) {
      currentBg = defaults.bgImage;
      setSetting('bgImage', currentBg);
    }
  }

  clearBrokenCustomBg();

  function setCustomBgPreview(url, selectable = true) {
    if (!customBgBtn || !customBgPreview || !url) return;
    const resolved = normalizeImageUrl(url);
    customBgPreview.src = resolved;
    if (selectable) customBgBtn.dataset.bg = resolved;
    else delete customBgBtn.dataset.bg;
  }

  function selectBgItem(item, url) {
    const resolved = normalizeImageUrl(url);
    if (!item || !resolved) return;
    bgItems.forEach(i => i.classList.remove('active'));
    item.classList.add('active');
    setSetting('bgImage', resolved);
    applyBackground(resolved);
  }

  function chooseCustomBg() {
    if (!customFileInput) return;
    customFileInput.value = '';
    customFileInput.click();
  }

  customFileInput.addEventListener('change', () => {
    const file = customFileInput.files && customFileInput.files[0];
    if (!file || !file.type.startsWith('image/')) return;
    const reader = new FileReader();
    reader.onload = () => {
      const url = String(reader.result || '');
      if (!isStoredCustomBg(url)) return;
      setCustomBgPreview(url);
      setSetting('customBgImage', url);
      selectBgItem(customBgBtn, url);
    };
    reader.readAsDataURL(file);
  });

  const savedCustomBg = isStoredCustomBg(getSetting('customBgImage')) ? getSetting('customBgImage') : '';
  if (savedCustomBg) setCustomBgPreview(savedCustomBg);
  else setCustomBgPreview('assets/bg-1.webp', false);

  bgItems.forEach(item => item.classList.remove('active'));
  bgItems.forEach(item => {
    const itemBg = item === customBgBtn ? savedCustomBg : item.dataset.bg;
    if (itemBg && normalizeImageUrl(itemBg) === normalizeImageUrl(currentBg)) item.classList.add('active');
    item.addEventListener('click', async () => {
      if (item !== customBgBtn) {
        selectBgItem(item, item.dataset.bg);
        return;
      }
      try {
        const customBg = isStoredCustomBg(getSetting('customBgImage')) ? getSetting('customBgImage') : '';
        if (customBg && !customBgBtn.classList.contains('active')) {
          setCustomBgPreview(customBg);
          selectBgItem(customBgBtn, customBg);
          return;
        }
        chooseCustomBg();
      } catch (e) {
        console.log('⚠️ 选择背景失败:', e);
      }
    });
  });

  // 自定义背景只复用内置背景卡片结构，图片来源由文件选择器替换。
  if (customBgBtn) {
    const activeCustomBg = customBgBtn.dataset.bg;
    if (activeCustomBg && normalizeImageUrl(activeCustomBg) === normalizeImageUrl(currentBg)) customBgBtn.classList.add('active');
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

  // ====== 内容区框框背景透明度 ======
  const surfaceOpacitySlider = document.getElementById('diyContentSurfaceOpacity');
  const surfaceOpacityVal = document.getElementById('diyContentSurfaceOpacityVal');
  if (surfaceOpacitySlider && surfaceOpacityVal) {
    surfaceOpacitySlider.value = getSetting('contentSurfaceOpacity');
    surfaceOpacityVal.textContent = surfaceOpacitySlider.value + '%';
    surfaceOpacitySlider.addEventListener('input', () => {
      surfaceOpacityVal.textContent = surfaceOpacitySlider.value + '%';
      setSetting('contentSurfaceOpacity', surfaceOpacitySlider.value);
      applyContentSurfaceOpacity(parseInt(surfaceOpacitySlider.value));
    });
  }

  // ====== 弹窗背景透明度 ======
  const modalOpacitySlider = document.getElementById('diyModalSurfaceOpacity');
  const modalOpacityVal = document.getElementById('diyModalSurfaceOpacityVal');
  if (modalOpacitySlider && modalOpacityVal) {
    modalOpacitySlider.value = getSetting('modalSurfaceOpacity');
    modalOpacityVal.textContent = modalOpacitySlider.value + '%';
    modalOpacitySlider.addEventListener('input', () => {
      modalOpacityVal.textContent = modalOpacitySlider.value + '%';
      setSetting('modalSurfaceOpacity', modalOpacitySlider.value);
      applyModalSurfaceOpacity(parseInt(modalOpacitySlider.value));
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
    const url = normalizeImageUrl(path);
    const globalBg = document.getElementById('globalBgImg');
    if (globalBg) globalBg.src = url;
    const homeBg = document.querySelector('.home-bg');
    if (homeBg) homeBg.src = url;
  }

  function applySakura(enabled) {
    const container = document.getElementById('sakuraContainer');
    if (!container) return;
    if (!enabled) {
      container.style.display = 'none';
      if (window._sakuraInstance) {
        window._sakuraInstance.destroy();
        window._sakuraInstance = null;
      }
      container.innerHTML = '';
      return;
    }
    container.style.display = '';
    if (!window._sakuraInstance && typeof SakuraPetals !== 'undefined') {
      window._sakuraInstance = new SakuraPetals(container, getSetting('sakuraCount'));
    }
    applyPetalColor(getSetting('petalColor'));
    applyPetalStyle(getSetting('petalStyle'));
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
    if (!getSetting('sakuraEnabled')) return;
    if (typeof SakuraPetals !== 'undefined') {
      window._sakuraInstance = new SakuraPetals(container, count);
    }
    setTimeout(() => {
      applyPetalColor(getSetting('petalColor'));
      applyPetalStyle(getSetting('petalStyle'));
    }, 600);
  }

  function applyPetalColor(color) {
    const container = document.getElementById('sakuraContainer');
    if (container) container.style.setProperty('--diy-petal-color', color);
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

  function buildWindowTheme({ light, mid, pale, soft, main, strong, dark }) {
    // 粉色主题不走这里，避免默认窗口颜色被重新计算。
    return {
      overlay: `rgba(${mid}, 0.42)`,
      modalSurface: `radial-gradient(circle at 10% 0%, rgba(${mid}, 0.72), transparent 36%), radial-gradient(circle at 92% 96%, rgba(${main}, 0.14), transparent 32%), linear-gradient(180deg, rgba(255, 255, 255, 0.98), rgba(${light}, 0.97))`,
      modalBorder: `1px solid rgba(${main}, 0.34)`,
      modalShadow: `0 24px 60px rgba(${dark}, 0.18), 0 0 0 1px rgba(255, 255, 255, 0.82) inset`,
      javaSurface: `radial-gradient(circle at 12% 0%, rgba(${mid}, 0.72), transparent 36%), linear-gradient(180deg, rgba(255, 255, 255, 0.98), rgba(${light}, 0.97))`,
      simpleSurface: `linear-gradient(135deg, rgba(${light}, 1) 0%, #ffffff 100%)`,
      toastShadow: `0 8px 32px rgba(${strong}, 0.15), 0 0 0 1px rgba(${mid}, 0.25)`,
      toastHoverShadow: `0 10px 40px rgba(${strong}, 0.2), 0 0 0 1px rgba(${mid}, 0.35)`,
      confirmShadow: `0 16px 48px rgba(${strong}, 0.18), 0 0 0 1px rgba(${mid}, 0.3)`,
      crashShadow: `0 20px 60px rgba(${strong}, 0.2), 0 0 0 1px rgba(${mid}, 0.3)`,
      panelBorder: `rgba(${main}, 0.34)`,
      panelBorderSoft: `rgba(${mid}, 0.3)`,
      panelBgSoft: `rgba(${light}, 0.92)`,
      panelBgStrong: `rgba(${light}, 0.96)`,
      panelShadow: `0 8px 28px rgba(${dark}, 0.12), 0 2px 8px rgba(0,0,0,0.06)`,
      actionGradient: `linear-gradient(135deg, rgb(${soft}), rgb(${main}), rgb(${strong}))`,
      actionGradientVertical: `linear-gradient(180deg, rgb(${soft}), rgb(${strong}))`,
      actionShadow: `0 12px 22px rgba(${strong}, 0.28), 0 0 0 4px rgba(${main}, 0.10)`,
      actionShadowHover: `0 14px 26px rgba(${strong}, 0.34), 0 0 0 4px rgba(${main}, 0.14)`,
      accent: `rgb(${main})`,
      accentStrong: `rgb(${strong})`,
      accentSoft: `rgba(${main}, 0.12)`,
      accentBorder: `rgba(${main}, 0.34)`,
      accentRing: `rgba(${main}, 0.14)`,
      trackBg: `rgba(${pale}, 0.78)`,
      dropOverlayBg: `rgba(${light}, 0.72)`,
      dropOverlayBorder: `rgba(${strong}, 0.55)`,
      dropOverlayShadow: `inset 0 0 0 999px rgba(255, 255, 255, 0.12), 0 14px 46px rgba(${dark}, 0.18)`,
      dropIconShadow: `0 8px 24px rgba(${strong}, 0.16)`,
      dropPillBg: `rgba(${light}, 0.92)`,
      dropPillShadow: `0 8px 28px rgba(${dark}, 0.12), 0 2px 8px rgba(0,0,0,0.06)`,
      dropStatus: `rgb(${dark})`,
      starGradient: `linear-gradient(135deg, rgb(${soft}), rgba(${soft}, 0.1))`,
      loaderActiveSurface: `linear-gradient(180deg, rgba(255, 255, 255, 0.98), rgba(${light}, 0.96))`,
      loaderActiveShadow: `0 12px 26px rgba(${main}, 0.16)`,
      loaderActiveInset: `0 0 0 1px rgba(${main}, 0.18) inset`
    };
  }

  const themedWindowVars = [
    '--oaoi-modal-overlay-bg', '--oaoi-modal-surface', '--oaoi-modal-border', '--oaoi-modal-shadow',
    '--theme-java-modal-surface', '--theme-simple-modal-surface', '--theme-toast-shadow', '--theme-toast-hover-shadow',
    '--theme-confirm-shadow', '--theme-crash-shadow', '--theme-panel-border', '--theme-panel-border-soft',
    '--theme-panel-bg-soft', '--theme-panel-bg-strong', '--theme-panel-shadow', '--theme-action-gradient',
    '--theme-action-gradient-vertical', '--theme-action-shadow', '--theme-action-shadow-hover', '--theme-accent',
    '--theme-accent-strong', '--theme-accent-soft', '--theme-accent-border', '--theme-accent-ring',
    '--theme-track-bg', '--theme-drop-overlay-bg', '--theme-drop-overlay-border', '--theme-drop-overlay-shadow',
    '--theme-drop-icon-shadow', '--theme-drop-pill-bg', '--theme-drop-pill-shadow', '--theme-drop-status',
    '--theme-star-gradient', '--theme-loader-active-surface', '--theme-loader-active-shadow', '--theme-loader-active-inset'
  ];

  const windowThemes = {
    blue: buildWindowTheme({ light: '240, 245, 255', mid: '208, 224, 255', pale: '208, 224, 255', soft: '160, 196, 255', main: '107, 163, 255', strong: '59, 125, 221', dark: '42, 76, 132' }),
    purple: buildWindowTheme({ light: '245, 240, 255', mid: '221, 208, 255', pale: '221, 208, 255', soft: '196, 160, 255', main: '160, 107, 255', strong: '123, 69, 232', dark: '86, 52, 138' }),
    green: buildWindowTheme({ light: '240, 255, 243', mid: '208, 255, 214', pale: '208, 255, 214', soft: '160, 232, 179', main: '107, 207, 127', strong: '69, 184, 90', dark: '45, 108, 60' }),
    orange: buildWindowTheme({ light: '255, 245, 240', mid: '255, 224, 208', pale: '255, 224, 208', soft: '255, 196, 160', main: '255, 160, 107', strong: '232, 116, 69', dark: '132, 74, 42' })
  };

  function applyTheme(name) {
    const themeName = themes[name] ? name : 'pink';
    const t = themes[themeName];
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
    if (themeName === 'pink') {
      themedWindowVars.forEach(v => r.style.removeProperty(v));
    } else {
      const w = windowThemes[themeName];
      if (!w) return;
      r.style.setProperty('--oaoi-modal-overlay-bg', w.overlay);
      r.style.setProperty('--oaoi-modal-surface', w.modalSurface);
      r.style.setProperty('--oaoi-modal-border', w.modalBorder);
      r.style.setProperty('--oaoi-modal-shadow', w.modalShadow);
      r.style.setProperty('--theme-java-modal-surface', w.javaSurface);
      r.style.setProperty('--theme-simple-modal-surface', w.simpleSurface);
      r.style.setProperty('--theme-toast-shadow', w.toastShadow);
      r.style.setProperty('--theme-toast-hover-shadow', w.toastHoverShadow);
      r.style.setProperty('--theme-confirm-shadow', w.confirmShadow);
      r.style.setProperty('--theme-crash-shadow', w.crashShadow);
      r.style.setProperty('--theme-panel-border', w.panelBorder);
      r.style.setProperty('--theme-panel-border-soft', w.panelBorderSoft);
      r.style.setProperty('--theme-panel-bg-soft', w.panelBgSoft);
      r.style.setProperty('--theme-panel-bg-strong', w.panelBgStrong);
      r.style.setProperty('--theme-panel-shadow', w.panelShadow);
      r.style.setProperty('--theme-action-gradient', w.actionGradient);
      r.style.setProperty('--theme-action-gradient-vertical', w.actionGradientVertical);
      r.style.setProperty('--theme-action-shadow', w.actionShadow);
      r.style.setProperty('--theme-action-shadow-hover', w.actionShadowHover);
      r.style.setProperty('--theme-accent', w.accent);
      r.style.setProperty('--theme-accent-strong', w.accentStrong);
      r.style.setProperty('--theme-accent-soft', w.accentSoft);
      r.style.setProperty('--theme-accent-border', w.accentBorder);
      r.style.setProperty('--theme-accent-ring', w.accentRing);
      r.style.setProperty('--theme-track-bg', w.trackBg);
      r.style.setProperty('--theme-drop-overlay-bg', w.dropOverlayBg);
      r.style.setProperty('--theme-drop-overlay-border', w.dropOverlayBorder);
      r.style.setProperty('--theme-drop-overlay-shadow', w.dropOverlayShadow);
      r.style.setProperty('--theme-drop-icon-shadow', w.dropIconShadow);
      r.style.setProperty('--theme-drop-pill-bg', w.dropPillBg);
      r.style.setProperty('--theme-drop-pill-shadow', w.dropPillShadow);
      r.style.setProperty('--theme-drop-status', w.dropStatus);
      r.style.setProperty('--theme-star-gradient', w.starGradient);
      r.style.setProperty('--theme-loader-active-surface', w.loaderActiveSurface);
      r.style.setProperty('--theme-loader-active-shadow', w.loaderActiveShadow);
      r.style.setProperty('--theme-loader-active-inset', w.loaderActiveInset);
    }
    applyContentSurfaceOpacity(getSetting('contentSurfaceOpacity'));
    applyModalSurfaceOpacity(getSetting('modalSurfaceOpacity'));
  }


  function applyOpacity(val) {
    document.documentElement.style.setProperty('--window-opacity', (val / 100).toString());
  }

  function hexToRgb(hex) {
    const raw = String(hex || '').replace('#', '').trim();
    if (raw.length !== 6) return '255, 255, 255';
    const num = parseInt(raw, 16);
    return `${(num >> 16) & 255}, ${(num >> 8) & 255}, ${num & 255}`;
  }

  function setAlphaVar(root, name, rgb, alpha, opacity) {
    root.style.setProperty(name, `rgba(${rgb}, ${(alpha * opacity).toFixed(3)})`);
  }

  function alphaValue(alpha, opacity) {
    return (alpha * opacity).toFixed(3);
  }

  function applyContentSurfaceOpacity(val) {
    const opacity = Math.max(0, Math.min(100, Number(val) || 0)) / 100;
    const root = document.documentElement;
    const theme = themes[getSetting('themeColor')] || themes.pink;
    const white = '255, 255, 255';
    const pink100 = hexToRgb(theme.l);
    const pink200 = hexToRgb(theme.m);
    const pink300 = hexToRgb(theme.d);
    const pink400 = hexToRgb(theme.h);
    const accent = hexToRgb(theme.h);

    root.style.setProperty('--content-surface-opacity', opacity.toFixed(3));
    [15, 16, 20, 25, 26, 30, 35, 40, 45, 50, 52, 55, 58, 60, 62, 64, 65, 68, 70, 72, 78, 80, 84, 85, 86, 90, 92, 94, 95, 96, 97, 98, 100]
      .forEach(v => setAlphaVar(root, `--surface-white-${v}`, white, v / 100, opacity));
    [[50, pink100], [60, pink100], [80, pink100], [92, pink100], [95, pink100], [97, pink100], [100, pink100]]
      .forEach(([v, rgb]) => setAlphaVar(root, `--surface-pink-100-${v}`, rgb, v / 100, opacity));
    [[15, pink200], [20, pink200], [25, pink200], [30, pink200], [40, pink200], [42, pink200], [50, pink200], [60, pink200], [70, pink200], [72, pink200], [75, pink200], [78, pink200], [100, pink200]]
      .forEach(([v, rgb]) => setAlphaVar(root, `--surface-pink-200-${v}`, rgb, v / 100, opacity));
    [[20, pink300], [25, pink300], [30, pink300], [40, pink300], [100, pink300]]
      .forEach(([v, rgb]) => setAlphaVar(root, `--surface-pink-300-${v}`, rgb, v / 100, opacity));
    [[15, pink400], [30, pink400]]
      .forEach(([v, rgb]) => setAlphaVar(root, `--surface-pink-400-${v}`, rgb, v / 100, opacity));
    [8, 10, 12, 14, 15, 20]
      .forEach(v => setAlphaVar(root, `--surface-accent-${String(v).padStart(2, '0')}`, accent, v / 100, opacity));
  }

  function applyModalSurfaceOpacity(val) {
    const opacity = Math.max(0, Math.min(100, Number(val) || 0)) / 100;
    const root = document.documentElement;
    const theme = themes[getSetting('themeColor')] || themes.pink;
    const white = '255, 255, 255';
    const pink100 = hexToRgb(theme.l);
    const pink200 = hexToRgb(theme.m);
    const pink300 = hexToRgb(theme.d);
    const pink400 = hexToRgb(theme.h);
    const accent = hexToRgb(theme.h);

    root.style.setProperty('--modal-surface-opacity', opacity.toFixed(3));
    [50, 60, 70, 78, 80, 86, 90, 92, 95, 96, 98, 100]
      .forEach(v => setAlphaVar(root, `--modal-surface-white-${v}`, white, v / 100, opacity));
    [[95, pink100], [97, pink100], [100, pink100]]
      .forEach(([v, rgb]) => setAlphaVar(root, `--modal-surface-pink-100-${v}`, rgb, v / 100, opacity));
    [[30, pink200], [42, pink200], [72, pink200], [100, pink200]]
      .forEach(([v, rgb]) => setAlphaVar(root, `--modal-surface-pink-200-${v}`, rgb, v / 100, opacity));
    [[25, pink300], [30, pink300]]
      .forEach(([v, rgb]) => setAlphaVar(root, `--modal-surface-pink-300-${v}`, rgb, v / 100, opacity));
    [[15, pink400]]
      .forEach(([v, rgb]) => setAlphaVar(root, `--modal-surface-pink-400-${v}`, rgb, v / 100, opacity));
    [14, 15]
      .forEach(v => setAlphaVar(root, `--modal-surface-accent-${String(v).padStart(2, '0')}`, accent, v / 100, opacity));

    // 弹窗单独调透明度，不跟主界面的框框背景混在一起。
    root.style.setProperty('--oaoi-modal-overlay-bg', `rgba(${pink200}, ${alphaValue(0.42, opacity)})`);
    root.style.setProperty('--oaoi-modal-surface',
      `radial-gradient(circle at 10% 0%, rgba(${pink200}, ${alphaValue(0.72, opacity)}), transparent 36%), ` +
      `radial-gradient(circle at 92% 96%, rgba(${accent}, ${alphaValue(0.14, opacity)}), transparent 32%), ` +
      `linear-gradient(180deg, rgba(${white}, ${alphaValue(0.98, opacity)}), rgba(${pink100}, ${alphaValue(0.97, opacity)}))`);
    root.style.setProperty('--theme-java-modal-surface',
      `radial-gradient(circle at 12% 0%, rgba(${pink200}, ${alphaValue(0.72, opacity)}), transparent 36%), ` +
      `linear-gradient(180deg, rgba(${white}, ${alphaValue(0.98, opacity)}), rgba(${pink100}, ${alphaValue(0.97, opacity)}))`);
    root.style.setProperty('--theme-simple-modal-surface',
      `linear-gradient(135deg, rgba(${pink100}, ${alphaValue(1, opacity)}) 0%, rgba(${white}, ${alphaValue(1, opacity)}) 100%)`);
    root.style.setProperty('--theme-loader-active-surface',
      `linear-gradient(180deg, rgba(${white}, ${alphaValue(0.98, opacity)}), rgba(${pink100}, ${alphaValue(0.96, opacity)}))`);
  }

  function applyRadius(val) {
    document.documentElement.style.setProperty('--window-radius', val + 'px');
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
    window._blurPauseEnabled = !!enabled;
    if (blurHandler) window.removeEventListener('blur', blurHandler);
    if (focusHandler) window.removeEventListener('focus', focusHandler);
    blurHandler = null; focusHandler = null;
    if (enabled) {
      blurHandler = () => {
        if (window._sakuraInstance?.setPaused) window._sakuraInstance.setPaused(true);
      };
      focusHandler = () => {
        if (window._sakuraInstance?.setPaused) window._sakuraInstance.setPaused(false);
      };
      window.addEventListener('blur', blurHandler);
      window.addEventListener('focus', focusHandler);
    } else if (window._sakuraInstance?.setPaused) {
      window._sakuraInstance.setPaused(false);
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
  applyContentSurfaceOpacity(getSetting('contentSurfaceOpacity'));
  applyModalSurfaceOpacity(getSetting('modalSurfaceOpacity'));
  applyRadius(getSetting('windowRadius'));
  applyBlurRadius(getSetting('blurRadius'));
}

// initDiyPage 由 main.js 的 DOMContentLoaded 统一调用
