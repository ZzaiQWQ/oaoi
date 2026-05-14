function initServerTabs() {
  const root = document.getElementById('pageServers');
  if (!root) return;

  const tabs = root.querySelectorAll('[data-server-tab]');
  const panels = root.querySelectorAll('[data-server-panel]');

  tabs.forEach((tab) => {
    tab.addEventListener('click', () => {
      const target = tab.dataset.serverTab;
      tabs.forEach((item) => item.classList.toggle('active', item === tab));
      panels.forEach((panel) => {
        panel.classList.toggle('active', panel.dataset.serverPanel === target);
      });
    });
  });
}

document.addEventListener('DOMContentLoaded', initServerTabs);
