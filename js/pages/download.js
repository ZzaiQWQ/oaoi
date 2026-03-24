// ============ 下载页逻辑 ============
// loadInstalledVersions() 在 home.js 中定义

function initDownloadPage() {
  const dlList = document.getElementById('dlList');
  const dlSearch = document.getElementById('dlSearch');
  let allVersions = [];
  let dlFilter = 'release';

  async function fetchVersions() {
    try {
      const resp = await fetch('https://piston-meta.mojang.com/mc/game/version_manifest_v2.json');
      const data = await resp.json();
      allVersions = data.versions;
      renderVersions();
    } catch (e) {
      if (dlList) dlList.innerHTML = '<div class="dl-loading">❌ 加载失败，请检查网络</div>';
    }
  }

  function renderVersions() {
    if (!dlList) return;
    const query = (dlSearch?.value || '').toLowerCase();
    const filtered = allVersions.filter(v => {
      if (dlFilter !== 'all' && v.type !== dlFilter) return false;
      if (query && !v.id.toLowerCase().includes(query)) return false;
      return true;
    });

    if (filtered.length === 0) {
      dlList.innerHTML = '<div class="dl-loading">没有找到匹配的版本</div>';
      return;
    }

    dlList.innerHTML = filtered.slice(0, 100).map(v => {
      const date = new Date(v.releaseTime);
      const dateStr = `${date.getFullYear()}-${String(date.getMonth()+1).padStart(2,'0')}-${String(date.getDate()).padStart(2,'0')}`;
      const icon = v.type === 'release' ? '📦' : '🧪';
      const typeName = v.type === 'release' ? '正式版' : '快照';
      return `
        <div class="dl-item">
          <div class="dl-item-icon ${v.type}">${icon}</div>
          <div class="dl-item-info">
            <div class="dl-item-name">${v.id}</div>
            <div class="dl-item-meta">
              <span class="dl-item-type ${v.type}">${typeName}</span>
              ${dateStr}
            </div>
          </div>
          <button class="dl-install-btn" data-version="${v.id}" data-url="${v.url}">安装</button>
        </div>
      `;
    }).join('');

    // 打开 新建实例 对话框
    dlList.querySelectorAll('.dl-install-btn').forEach(btn => {
      btn.addEventListener('click', () => {
        const ver = btn.dataset.version;
        const url = btn.dataset.url;
        
        document.getElementById('instMcVersion').value = ver;
        document.getElementById('instNameInput').value = `${ver}`;
        document.getElementById('instMetaUrl').value = url;
        
        document.querySelectorAll('input[name="loader"]').forEach(el => {
            if (el.value === 'vanilla') el.checked = true;
        });
        document.querySelectorAll('.loader-radio-btn').forEach(el => el.classList.remove('active'));
        document.querySelector('input[value="vanilla"]').parentElement.classList.add('active');
        document.getElementById('loaderVersionGroup').style.display = 'none';
        
        const createBtn = document.getElementById('createInstBtn');
        createBtn.disabled = false;
        createBtn.textContent = '确认创建';
        
        document.getElementById('newInstanceModal').classList.remove('hidden');
      });
    });
  }

  // 筛选按钮
  document.querySelectorAll('.dl-filter-btn').forEach(btn => {
    btn.addEventListener('click', () => {
      document.querySelectorAll('.dl-filter-btn').forEach(b => b.classList.remove('active'));
      btn.classList.add('active');
      dlFilter = btn.dataset.filter;
      renderVersions();
    });
  });

  // ===== 新建实例 Modal 逻辑 =====
  const modal = document.getElementById('newInstanceModal');
  if (modal) {
    const closeBtn = document.getElementById('closeModalBtn');
    const cancelBtn = document.getElementById('cancelInstBtn');
    const createBtn = document.getElementById('createInstBtn');
    const loaderVersionGroup = document.getElementById('loaderVersionGroup');
    const loaderSelect = document.getElementById('instLoaderVersion');
    const loaderTargetSpinner = document.getElementById('loaderTargetVersion');

    function closeModal() {
      modal.classList.add('hidden');
    }
    closeBtn.addEventListener('click', closeModal);
    cancelBtn.addEventListener('click', closeModal);

    // 监听加载器切换
    document.querySelectorAll('input[name="loader"]').forEach(radio => {
      radio.addEventListener('change', async (e) => {
        document.querySelectorAll('.loader-radio-btn').forEach(l => l.classList.remove('active'));
        e.target.parentElement.classList.add('active');
        
        const loader = e.target.value;
        const mcVer = document.getElementById('instMcVersion').value;
        const nameInput = document.getElementById('instNameInput');
        
        nameInput.value = loader === 'vanilla' ? mcVer : `${mcVer}-${loader}`;
        
        if (loader === 'vanilla') {
          loaderVersionGroup.style.display = 'none';
          return;
        }
        
        loaderVersionGroup.style.display = 'block';
        loaderSelect.innerHTML = '';
        loaderTargetSpinner.textContent = '加载中...';
        
        try {
          const tauri = await waitForTauri();
          let versions = [];
          if (loader === 'fabric') {
            versions = await tauri.core.invoke('get_fabric_versions', { mcVersion: mcVer });
          } else if (loader === 'forge') {
            versions = await tauri.core.invoke('get_forge_versions', { mcVersion: mcVer });
          }
          
          if (versions.length === 0) {
            loaderTargetSpinner.textContent = ' 无可用版本';
          } else {
            loaderTargetSpinner.textContent = '';
            versions.forEach(v => {
              const opt = document.createElement('option');
              opt.value = opt.textContent = v;
              loaderSelect.appendChild(opt);
            });
          }
        } catch (err) {
          loaderTargetSpinner.textContent = ' 获取失败';
          console.error(err);
        }
      });
    });

    // 确认创建实例
    createBtn.addEventListener('click', async () => {
      const name = document.getElementById('instNameInput').value.trim() || document.getElementById('instMcVersion').value;
      const mcVer = document.getElementById('instMcVersion').value;
      const metaUrl = document.getElementById('instMetaUrl').value;
      const loaderType = document.querySelector('input[name="loader"]:checked').value;
      const loaderVer = loaderSelect.value || '';
      
      createBtn.disabled = true;
      createBtn.textContent = '✨ 创建中...';
      
      try {
        const tauri = await waitForTauri();
        const dlActiveList = document.getElementById('dlActiveList');

        closeModal();

        if (dlActiveList) {
          dlActiveList.innerHTML = `
            <h2 class="dl-section-title">⏳ 活跃下载</h2>
            <div class="dl-progress-card">
              <div class="dl-progress-name">📦 ${name}</div>
              <div class="dl-progress-detail" id="dlProgressDetail">准备中...</div>
              <div class="dl-progress-bar-wrap">
                <div class="dl-progress-bar" id="dlProgressBar" style="width: 0%"></div>
              </div>
            </div>
          `;
        }

        const unlisten = await tauri.event.listen('install-progress', (event) => {
          const { stage, current, total, detail } = event.payload;
          const progressDetail = document.getElementById('dlProgressDetail');
          const progressBar = document.getElementById('dlProgressBar');

          if (stage === 'done') {
            if (progressDetail) progressDetail.textContent = '✅ 安装完成！';
            if (progressBar) progressBar.style.width = '100%';
            loadInstalledVersions();
            setTimeout(() => {
              if (dlActiveList) {
                dlActiveList.innerHTML = `
                  <h2 class="dl-section-title">⏳ 活跃下载</h2>
                  <div class="dl-active-empty">暂无下载任务</div>
                `;
              }
            }, 2000);
            unlisten();
          } else if (stage === 'error') {
            if (progressDetail) progressDetail.textContent = '❌ ' + detail;
            if (progressBar) { progressBar.style.width = '100%'; progressBar.style.background = '#ff6b6b'; }
            setTimeout(() => {
              if (dlActiveList) {
                dlActiveList.innerHTML = `
                  <h2 class="dl-section-title">⏳ 活跃下载</h2>
                  <div class="dl-active-empty">暂无下载任务</div>
                `;
              }
            }, 5000);
            unlisten();
          } else {
            if (progressDetail) progressDetail.textContent = detail;
            if (progressBar && total > 0) {
              progressBar.style.width = Math.round((current / total) * 100) + '%';
            }
          }
        });
        
        const gameDir = localStorage.getItem('gameDir') || '';
        const javaPath = localStorage.getItem('selectedJavaPath') || '';
        await tauri.core.invoke('create_instance', { 
           name: name,
           mcVersion: mcVer,
           metaUrl: metaUrl,
           gameDir: gameDir,
           loaderType: loaderType,
           loaderVersion: loaderVer,
           javaPath: javaPath
        });
      } catch (e) {
        console.error('创建失败:', e);
        createBtn.textContent = '❌ 调用失败';
        setTimeout(() => { createBtn.textContent = '确认创建'; createBtn.disabled = false; }, 3000);
      }
    });
  }

  // 搜索
  if (dlSearch) {
    dlSearch.addEventListener('input', () => renderVersions());
  }

  // 加载版本列表
  if (dlList && allVersions.length === 0) fetchVersions();

  console.log('🌸 下载页已初始化');
}