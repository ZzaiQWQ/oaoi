/* ============================================
   oaoi - 主入口（初始化各模块）
   ============================================ */

document.addEventListener('DOMContentLoaded', () => {
  // Tauri 窗口控制
  initWindowControls();

  // 樱花飘落
  const sakuraContainer = document.getElementById('sakuraContainer');
  if (sakuraContainer) new SakuraPetals(sakuraContainer, 20);

  // 导航
  initNavigation();

  // 主页
  initLaunchButton();
  simulateOnlinePlayers();
  initNewsCards();
  initVersionDropdown();

  // 设置
  initSettings();

  // 下载页
  initDownloadPage();

  // 首次启动：选择游戏目录
  checkFirstLaunch();

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
