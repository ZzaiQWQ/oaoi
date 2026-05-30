// ============ 关于页 ============
const ABOUT_CHANGELOG_REFRESH_MS = 60000;
let aboutChangelogTimer = null;
let aboutChangelogLastAttempt = 0;

function initAboutPage() {
  const aboutPage = document.getElementById('pageAbout');
  if (!aboutPage) return;

  document.querySelector('[data-page="about"]')?.addEventListener('click', () => {
    setTimeout(() => {
      loadAboutChangelog(true);
      startAboutChangelogRefresh();
    }, 0);
  });

  document.querySelectorAll('.nav-item').forEach(item => {
    item.addEventListener('click', () => {
      if (item.getAttribute('data-page') !== 'about') {
        stopAboutChangelogRefresh();
      }
    });
  });

  if (aboutPage.classList.contains('active')) {
    loadAboutChangelog(true);
    startAboutChangelogRefresh();
  }
}

function startAboutChangelogRefresh() {
  stopAboutChangelogRefresh();
  aboutChangelogTimer = setInterval(() => {
    const aboutPage = document.getElementById('pageAbout');
    if (!aboutPage?.classList.contains('active')) {
      stopAboutChangelogRefresh();
      return;
    }
    loadAboutChangelog(false);
  }, ABOUT_CHANGELOG_REFRESH_MS);
}

function stopAboutChangelogRefresh() {
  if (aboutChangelogTimer) {
    clearInterval(aboutChangelogTimer);
    aboutChangelogTimer = null;
  }
}

async function loadAboutChangelog(force = false) {
  const list = document.querySelector('.about-changelog-list');
  if (!list) return;

  const now = Date.now();
  if (!force && now - aboutChangelogLastAttempt < ABOUT_CHANGELOG_REFRESH_MS) return;
  aboutChangelogLastAttempt = now;

  try {
    const tauri = await waitForTauri();
    const data = await tauri.core.invoke('get_changelog');
    const items = normalizeAboutChangelog(data);
    if (!items.length) return;
    list.innerHTML = items.map(renderAboutLogItem).join('');
  } catch (err) {
    console.warn('[about] 更新日志拉取失败，使用本地默认日志:', err);
  }
}

function normalizeAboutChangelog(data) {
  const rawItems = Array.isArray(data)
    ? data
    : Array.isArray(data?.items)
      ? data.items
      : Array.isArray(data?.logs)
        ? data.logs
        : Array.isArray(data?.changelog)
          ? data.changelog
          : [];

  return rawItems
    .map(item => {
      if (typeof item === 'string') {
        return { title: item.trim(), desc: '' };
      }
      if (!item || typeof item !== 'object') return null;

      const version = stringValue(item.version);
      const title = stringValue(item.title) || stringValue(item.name) || (version ? `v${version}` : '');
      const changes = Array.isArray(item.changes)
        ? item.changes.map(stringValue).filter(Boolean).join('；')
        : '';
      const desc =
        stringValue(item.desc) ||
        stringValue(item.description) ||
        stringValue(item.note) ||
        stringValue(item.notes) ||
        stringValue(item.content) ||
        changes;

      return title || desc ? { title: title || '更新内容', desc } : null;
    })
    .filter(Boolean)
    .slice(0, 30);
}

function stringValue(value) {
  return typeof value === 'string' || typeof value === 'number' ? String(value).trim() : '';
}

function renderAboutLogItem(item) {
  return `
    <div class="about-log-item"><span class="about-log-dot"></span>
      <div>
        <p class="about-log-title">${escapeHtml(item.title)}</p>
        <p class="about-log-desc">${escapeHtml(item.desc)}</p>
      </div>
    </div>
  `;
}
