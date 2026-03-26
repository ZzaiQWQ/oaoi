// ============ 主页逻辑 ============

// MC 版本 → 所需 Java 版本映射（全局，settings.js 也可用）
function getRequiredJavaMajor(mcVersion) {
  if (!mcVersion) return 21;
  const parts = mcVersion.split('.');
  const major = parseInt(parts[0]) || 1;
  const minor = parseInt(parts[1]) || 0;
  const patch = parseInt(parts[2]) || 0;
  // MC 26.x+ → Java 25
  if (major >= 26) return 25;
  // MC 1.20.5+ → Java 21
  if (minor >= 21 || (minor === 20 && patch >= 5)) return 21;
  // MC 1.17+ → Java 17
  if (minor >= 17) return 17;
  // MC 1.16 及以下 → Java 8
  return 8;
}

function resetLaunchBtn(btn) {
  btn.style.pointerEvents = '';
  btn.style.background = '';
  btn.innerHTML = `<span class="launch-icon">⚔️</span><span>启动游戏</span>`;
}
async function loadInstalledVersions() {
  const sel = document.getElementById('versionSelector');
  const installedList = document.getElementById('installedList');
  const installedCount = document.getElementById('installedCount');
  try {
    const tauri = await waitForTauri();
    const gameDir = localStorage.getItem('gameDir') || '';
    const instances = await tauri.core.invoke('list_installed_versions', { gameDir });

    // 更新主页版本选择器（隐藏的 select + 自定义下拉）
    if (sel) {
      const prev = sel.value;
      sel.innerHTML = '<option value="">-- 选择实例 --</option>';
      instances.forEach(v => {
        const opt = document.createElement('option');
        opt.value = opt.textContent = v.name;
        sel.appendChild(opt);
      });
      const saved = localStorage.getItem('selectedVersion');
      if (saved && instances.find(v => v.name === saved)) sel.value = saved;
      else if (prev && instances.find(v => v.name === prev)) sel.value = prev;

      // 同步自定义下拉组件
      syncVersionDropdown(instances, sel.value);
    }

    // 更新下载页已安装版本列表
    if (installedList) {
      if (installedCount) installedCount.textContent = instances.length;
      if (instances.length === 0) {
        installedList.innerHTML = '<div class="installed-empty">暂无已安装实例，请在下方下载</div>';
      } else {
        installedList.innerHTML = instances.map(v => `
          <div class="installed-card" data-ver="${v.name}">
            <div style="flex: 1; min-width:0; display:flex; flex-direction:column; gap:1px;">
              <span class="ver-name">${v.name}</span>
              <span style="font-size:9px; color:#b0506e; white-space:nowrap; overflow:hidden; text-overflow:ellipsis;">MC ${v.mc_version} <span style="text-transform:capitalize;">${v.loader_type !== 'vanilla' ? '| ' + v.loader_type : ''}</span></span>
            </div>
            <button class="ver-delete" title="删除此实例" data-ver="${v.name}">🗑️</button>
          </div>
        `).join('');
        // 绑定删除事件
        installedList.querySelectorAll('.ver-delete').forEach(btn => {
          btn.addEventListener('click', async (e) => {
            e.stopPropagation();
            const ver = btn.dataset.ver;
            if (!confirm(`确定删除实例 ${ver} 吗？`)) return;
            try {
              await tauri.core.invoke('delete_version', { gameDir, versionId: ver });
              loadInstalledVersions(); // 刷新列表
            } catch (err) {
              alert('删除失败: ' + err);
            }
          });
        });
      }
    }
  } catch (e) {
    console.warn('加载实例列表失败:', e);
  }
}

function initLaunchButton() {
  const btn = document.getElementById('launchBtn');
  const sel = document.getElementById('versionSelector');
  if (!btn) return;

  // 保存选择
  if (sel) sel.addEventListener('change', () => localStorage.setItem('selectedVersion', sel.value));

  // 初始加载版本列表
  loadInstalledVersions();

  btn.addEventListener('click', async () => {
    // 读取设置
    const gameDir = localStorage.getItem('gameDir');
    const playerName = localStorage.getItem('playerName');
    const memAlloc = parseInt(localStorage.getItem('memAlloc') || '4096');
    const selectedVersion = sel ? sel.value : '';

    // 验证
    if (!selectedVersion) { alert('⚠️ 请先选择一个已安装的版本'); return; }
    if (!gameDir) { alert('⚠️ 请先在设置页选择游戏目录'); return; }
    if (!playerName) { alert('⚠️ 请先在设置页输入玩家名称或微软登录'); return; }

    // 自动/手动 Java 选择
    const javaMode = localStorage.getItem('javaMode') || 'auto';
    let javaPath;

    if (javaMode === 'auto') {
      const mcVer = selectedVersion.split('-')[0]; // "1.21.10-fabric" → "1.21.10"
      const requiredMajor = getRequiredJavaMajor(mcVer);

      btn.style.pointerEvents = 'none';
      btn.innerHTML = `<span class="launch-icon">⏳</span><span>查找 Java ${requiredMajor}...</span>`;
      btn.style.background = 'linear-gradient(135deg, #f59e0b 0%, #d97706 100%)';

      try {
        const tauri = await waitForTauri();

        // 1. 先扫描系统（包括 gameDir/java/）
        const javas = await tauri.core.invoke('find_java', { gameDir });
        console.log(`[java] 扫描到 ${javas.length} 个 Java`);
        const matched = javas.find(j => j.major === requiredMajor);

        if (matched) {
          javaPath = matched.path;
          console.log(`[java] 找到匹配: Java ${requiredMajor} → ${javaPath}`);
        } else {
          // 2. 没找到 → 自动下载
          console.log(`[java] 未找到 Java ${requiredMajor}，自动下载...`);
          btn.innerHTML = `<span class="launch-icon">⏳</span><span>下载 Java ${requiredMajor}...</span>`;
          const result = await tauri.core.invoke('download_java', { major: requiredMajor, gameDir });
          if (result && result !== 'downloading') {
            javaPath = result;
            console.log(`[java] 已存在: ${javaPath}`);
          } else {
            javaPath = await new Promise((resolve, reject) => {
              tauri.event.listen('java-download-done', (event) => {
                const d = event.payload;
                if (d.major === requiredMajor) {
                  if (d.success) resolve(d.path);
                  else reject(d.error || '下载失败');
                }
              });
            });
            console.log(`[java] 下载完成: ${javaPath}`);
          }
        }
      } catch (e) {
        alert(`Java ${requiredMajor} 获取失败: ${e}`);
        resetLaunchBtn(btn);
        return;
      }
    } else {
      javaPath = localStorage.getItem('selectedJavaPath');
      if (!javaPath) { alert('⚠️ 请先在设置页选择 Java 路径'); return; }
    }

    // 读取 MS 正版信息（如果有）
    const msProfileStr = localStorage.getItem('msProfile');
    const msProfile = msProfileStr ? JSON.parse(msProfileStr) : null;

    btn.style.pointerEvents = 'none';
    btn.innerHTML = `
      <span class="launch-icon">⏳</span>
      <span>正在启动...</span>
    `;
    btn.style.background = 'linear-gradient(135deg, #c084fc 0%, #a855f7 50%, #9333ea 100%)';
    createPetalBurst(btn);

    try {
      const tauri = await waitForTauri();
      const result = await tauri.core.invoke('launch_minecraft', {
        options: {
          java_path: javaPath,
          game_dir: gameDir,
          version_name: selectedVersion,
          player_name: playerName,
          memory_mb: memAlloc,
          server_ip: null,
          server_port: null,
          access_token: msProfile ? msProfile.access_token : null,
          uuid: msProfile ? msProfile.uuid : null,
        }
      });

      console.log('🎮 ' + result);
      btn.innerHTML = `
        <span class="launch-icon">✅</span>
        <span>启动成功！</span>
      `;
      btn.style.background = 'linear-gradient(135deg, #86efac 0%, #4ade80 50%, #22c55e 100%)';
    } catch (e) {
      console.log('❌ 启动失败:', e);
      btn.innerHTML = `
        <span class="launch-icon">❌</span>
        <span>${e}</span>
      `;
      btn.style.background = 'linear-gradient(135deg, #fca5a5 0%, #f87171 50%, #ef4444 100%)';
    }

    setTimeout(() => {
      btn.innerHTML = `
        <span class="launch-icon">⚔️</span>
        <span>启动游戏</span>
        <span class="launch-sparkle">✦</span>
      `;
      btn.style.background = '';
      btn.style.pointerEvents = 'auto';
    }, 3000);
  });
}

// ============ 在线人数模拟 ============
function simulateOnlinePlayers() {
  const onlineInfo = document.querySelector('.online-info strong');
  if (!onlineInfo) return;

  setInterval(() => {
    const currentCount = parseInt(onlineInfo.textContent);
    const change = Math.floor(Math.random() * 5) - 2; // -2 到 +2
    const newCount = Math.max(100, Math.min(200, currentCount + change));
    onlineInfo.textContent = newCount;
  }, 5000);
}

// ============ 新闻卡片悬停效果 ============
function initNewsCards() {
  const cards = document.querySelectorAll('.news-card');

  cards.forEach(card => {
    card.addEventListener('mouseenter', () => {
      // 轻微倾斜效果
      card.style.transition = 'all 0.3s ease';
    });

    card.addEventListener('mousemove', (e) => {
      const rect = card.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const y = e.clientY - rect.top;
      const centerX = rect.width / 2;
      const centerY = rect.height / 2;
      const rotateX = (y - centerY) / centerY * -3;
      const rotateY = (x - centerX) / centerX * 3;

      card.style.transform = `translateY(-3px) perspective(500px) rotateX(${rotateX}deg) rotateY(${rotateY}deg)`;
    });

    card.addEventListener('mouseleave', () => {
      card.style.transform = '';
    });
  });
}

// ============ 自定义版本下拉框 ============
function syncVersionDropdown(instances, selectedValue) {
  const list = document.getElementById('versionDropdownList');
  const text = document.getElementById('versionDropdownText');
  if (!list || !text) return;

  list.innerHTML = '';
  // 默认项
  const defItem = document.createElement('button');
  defItem.type = 'button';
  defItem.className = 'vd-item' + (!selectedValue ? ' active' : '');
  defItem.textContent = '-- 选择实例 --';
  defItem.dataset.value = '';
  list.appendChild(defItem);

  instances.forEach(v => {
    const item = document.createElement('button');
    item.type = 'button';
    item.className = 'vd-item' + (v.name === selectedValue ? ' active' : '');
    item.textContent = v.name;
    item.dataset.value = v.name;
    list.appendChild(item);
  });

  text.textContent = selectedValue || '-- 选择实例 --';
}

function initVersionDropdown() {
  const dropdown = document.getElementById('versionDropdown');
  const btn = document.getElementById('versionDropdownBtn');
  const list = document.getElementById('versionDropdownList');
  const text = document.getElementById('versionDropdownText');
  const sel = document.getElementById('versionSelector');
  if (!dropdown || !btn || !list) return;

  // 打开/关闭列表
  btn.addEventListener('click', () => {
    list.classList.toggle('hidden');
  });

  // 选择某一项
  list.addEventListener('click', (e) => {
    const item = e.target.closest('.vd-item');
    if (!item) return;
    const value = item.dataset.value;
    list.querySelectorAll('.vd-item').forEach(i => i.classList.remove('active'));
    item.classList.add('active');
    if (sel) sel.value = value;
    if (text) text.textContent = item.textContent;
    if (value) localStorage.setItem('selectedVersion', value);
    list.classList.add('hidden');
  });

  // 点外部关闭
  document.addEventListener('click', (e) => {
    if (!dropdown.contains(e.target)) {
      list.classList.add('hidden');
    }
  });
}