// ============ 导航切换 ============
function initNavigation() {
  const navItems = document.querySelectorAll('.nav-item');
  const pages = document.querySelectorAll('.page');

  const pageMap = {
    home: document.getElementById('pageHome'),
    servers: document.getElementById('pageServers'),
    download: document.getElementById('pageDownload'),
    settings: document.getElementById('pageSettings'),
    about: document.getElementById('pageAbout'),
    diy: document.getElementById('pageDiy'),
  };

  navItems.forEach(item => {
    item.addEventListener('click', (e) => {
      e.preventDefault();

      // 移除所有 active
      navItems.forEach(n => n.classList.remove('active'));
      pages.forEach(p => p.classList.remove('active'));

      // 添加 active
      item.classList.add('active');

      // 切换页面
      const pageName = item.getAttribute('data-page');
      if (pageMap[pageName]) {
        pageMap[pageName].classList.add('active');
      }
      // 刷新已安装版本列表
      if (pageName === 'download' || pageName === 'home') {
        loadInstalledVersions();
      }

      // 添加点击涟漪效果
      const icon = item.querySelector('.nav-icon');
      icon.style.transform = 'scale(0.85)';
      setTimeout(() => {
        icon.style.transform = 'scale(1)';
      }, 150);
    });
  });
}
