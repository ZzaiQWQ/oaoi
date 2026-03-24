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

  // 设置
  initSettings();

  // 下载页
  initDownloadPage();

  console.log('🌸 oaoi 启动器已加载！');
});
