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
  try { initInstanceDetailPage(); } catch (e) { console.error('实例详情初始化失败:', e); }

  // 首次启动：选择游戏目录
  try { checkFirstLaunch(); } catch (e) { console.error('首次启动检查失败:', e); }

  console.log('🌸 oaoi 启动器已加载！');
});

async function checkFirstLaunch() {
  if (localStorage.getItem('gameDir')) return; // 已选过

  const modal = document.getElementById('firstLaunchModal');
  const dirDisplay = document.getElementById('firstLaunchDirDisplay');
  const dirBtn = document.getElementById('firstLaunchDirBtn');
  const confirmBtn = document.getElementById('firstLaunchConfirmBtn');
  if (!modal) return;

  modal.classList.remove('hidden');
  let selectedDir = '';

  dirBtn.addEventListener('click', async () => {
    try {
      const tauri = await waitForTauri();
      const selected = await tauri.dialog.open({
        title: '选择游戏安装目录',
        directory: true,
      });
      if (selected) {
        selectedDir = selected + '\\oaoi';
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
