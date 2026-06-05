/* ============================================
   oaoi - 主入口（初始化各模块）
   ============================================ */

document.addEventListener('DOMContentLoaded', () => {
  // ===== 全局等比自适应缩放引擎 =====
  function autoScale() {
    const baseW = 800, baseH = 480;
    const winW = window.innerWidth;
    const winH = window.innerHeight;

    // 以 800x480 为基准计算缩放比
    const scale = Math.min(winW / baseW, winH / baseH);
    const uiScale = Math.max(0.55, Math.min(1, scale));
    document.documentElement.style.setProperty('--app-ui-scale', uiScale.toFixed(4));
    document.documentElement.style.setProperty('--app-modal-scale', Math.min(1, scale).toFixed(4));

    const container = document.getElementById('app-scale-container');
    if (container) {
      // 容器尺寸 = 窗口尺寸 / 缩放比，这样缩放后恰好填满窗口，无透明边框
      container.style.width = (winW / scale) + 'px';
      container.style.height = (winH / scale) + 'px';
      container.style.transformOrigin = 'top left';
      container.style.transform = `scale(${scale})`;
    }
  }
  window.addEventListener('resize', autoScale);
  autoScale();

  ['newInstanceModal', 'modpackVersionModal'].forEach((id) => {
    const modal = document.getElementById(id);
    if (modal && modal.parentElement !== document.body) {
      document.body.appendChild(modal);
    }
  });


  // Tauri 窗口控制
  initWindowControls();

  // 樱花飘落（读取 DIY 设置，不硬编码数量）
  const sakuraContainer = document.getElementById('sakuraContainer');
  if (sakuraContainer) {
    const sakuraEnabled = localStorage.getItem('diy_sakuraEnabled');
    if (sakuraEnabled !== 'false') {
      const count = parseInt(localStorage.getItem('diy_sakuraCount')) || 15;
      window._sakuraInstance = new SakuraPetals(sakuraContainer, count);
    } else {
      sakuraContainer.style.display = 'none';
    }
  }

  // 导航
  try { initNavigation(); } catch (e) { console.error('导航初始化失败:', e); }
  try { initP2PLink(); } catch (e) { console.error('联机工具初始化失败:', e); }
  try { initAboutPage(); } catch (e) { console.error('关于页初始化失败:', e); }

  // 主页
  try { initLaunchButton(); } catch (e) { console.error('启动按钮初始化失败:', e); }
  try { initNewsHoverEffects(); } catch (e) { console.error('新闻卡片初始化失败:', e); }
  try { initVersionDropdown(); } catch (e) { console.error('版本下拉初始化失败:', e); }

  // 设置
  try { initSettings(); } catch (e) { console.error('设置页初始化失败:', e); }

  // 下载页
  try { initDownloadPage(); } catch (e) { console.error('下载页初始化失败:', e); }

  // DIY 个性化（略延迟确保 DOM 就绪）
  requestAnimationFrame(() => { try { initDiyPage(); } catch (e) { console.error('DIY页初始化失败:', e); } });

  // 实例详情页
  try { initInstanceDetailPage(); } catch (e) { console.error('版本详情初始化失败:', e); }

  // 首次启动：选择游戏目录
  try { checkFirstLaunch(); } catch (e) { console.error('首次启动检查失败:', e); }

  setTimeout(() => {
    checkForUpdates().catch(e => console.warn('[update] 检查更新失败:', e));
  }, 2000);

  updateVersionBadge().catch(e => console.warn('[version] 读取版本失败:', e));

  console.log('🌸 oaoi 启动器已加载！');
});

async function checkForUpdates() {
  const tauri = await waitForTauri();
  const currentVersion = await tauri.core.invoke('get_app_version');
  const manifest = await fetchUpdateManifest(tauri);
  if (!manifest || !manifest.version || !manifest.url) return;
  if (compareVersions(manifest.version, currentVersion) <= 0) return;

  const skipped = sessionStorage.getItem('update_skip_version');
  if (skipped === manifest.version) return;

  const notes = manifest.notes ? `\n\n${manifest.notes}` : '';
  const confirmed = await showConfirm(
    `发现新版本 ${manifest.version}，当前版本 ${currentVersion}。${notes}`,
    {
      title: '发现更新',
      confirmText: '立即更新',
      cancelText: '稍后',
      kind: 'info',
      dialogClass: 'oaoi-update-confirm',
    }
  );
  if (!confirmed) {
    sessionStorage.setItem('update_skip_version', manifest.version);
    return;
  }

  showToast('正在下载更新，完成后会自动重启...', 'info', 15000);
  try {
    await tauri.core.invoke('install_update', {
      url: manifest.url,
      mirrorUrl: manifest.mirror_url || null,
      sha256: manifest.sha256 || ''
    });
  } catch (e) {
    const msg = typeof e === 'string' ? e : (e.message || JSON.stringify(e) || '更新失败');
    showToast(`更新失败：${msg}`, 'error', 12000);
    console.error('[update] install failed:', e);
  }
}

async function updateVersionBadge() {
  const badge = document.getElementById('appVersionBadge');
  if (!badge) return;
  const tauri = await waitForTauri();
  const version = await tauri.core.invoke('get_app_version');
  badge.textContent = `v${version}`;
}

async function fetchUpdateManifest(tauri) {
  return await tauri.core.invoke('get_update_manifest');
}

function compareVersions(a, b) {
  const pa = String(a).replace(/^v/i, '').split('.').map(n => parseInt(n, 10) || 0);
  const pb = String(b).replace(/^v/i, '').split('.').map(n => parseInt(n, 10) || 0);
  const len = Math.max(pa.length, pb.length);
  for (let i = 0; i < len; i++) {
    const diff = (pa[i] || 0) - (pb[i] || 0);
    if (diff !== 0) return diff;
  }
  return 0;
}

async function checkFirstLaunch() {
  if (localStorage.getItem('gameDir')) return; // 已选过

  const modal = document.getElementById('firstLaunchModal');
  const dirDisplay = document.getElementById('firstLaunchDirDisplay');
  const dirBtn = document.getElementById('firstLaunchDirBtn');
  const confirmBtn = document.getElementById('firstLaunchConfirmBtn');
  if (!modal) return;

  modal.classList.remove('hidden');
  let selectedDir = '';

  function normalizeMinecraftDir(path) {
    const value = String(path || '').replace(/[\\/]+$/, '');
    if (!value) return '';
    return /(^|[\\/])\.minecraft$/i.test(value) ? value : `${value}\\.minecraft`;
  }

  dirBtn.addEventListener('click', async () => {
    try {
      const tauri = await waitForTauri();
      const selected = await tauri.dialog.open({
        title: '选择游戏安装目录',
        directory: true,
      });
      if (selected) {
        selectedDir = normalizeMinecraftDir(selected);
        dirDisplay.textContent = selectedDir;
        dirDisplay.style.color = '#c94a6a';
        confirmBtn.disabled = false;
      }
    } catch (e) {
      console.log('⚠️ 目录选择失败:', e);
    }
  });

  confirmBtn.addEventListener('click', () => {
    if (!selectedDir) return;
    localStorage.setItem('gameDir', selectedDir);
    modal.classList.add('hidden');
    // 刷新设置页显示
    const gameDirDisplay = document.getElementById('gameDirDisplay');
    if (gameDirDisplay) {
      gameDirDisplay.textContent = selectedDir;
      gameDirDisplay.title = selectedDir;
    }
  });
}
