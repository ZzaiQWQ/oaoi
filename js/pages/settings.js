// ============ 设置页交互 ============
function initSettings() {
  // 游戏目录选择
  const gameDirDisplay = document.getElementById('gameDirDisplay');
  const gameDirBtn = document.getElementById('gameDirBtn');
  const cachedGameDir = localStorage.getItem('gameDir');

  if (cachedGameDir && gameDirDisplay) {
    gameDirDisplay.textContent = cachedGameDir;
    gameDirDisplay.title = cachedGameDir;
  }

  if (gameDirBtn && gameDirDisplay) {
    gameDirBtn.addEventListener('click', async () => {
      try {
        const tauri = await waitForTauri();
        const selected = await tauri.dialog.open({
          title: '选择游戏安装目录',
          directory: true,
        });
        if (selected) {
          const gameDir = selected + '\\oaoi';
          gameDirDisplay.textContent = gameDir;
          gameDirDisplay.title = gameDir;
          localStorage.setItem('gameDir', gameDir);
          console.log('📁 游戏目录:', gameDir);
        }
      } catch (e) {
        console.log('⚠️ 目录选择失败:', e);
      }
    });
  }

  // 玩家名（离线模式 fallback）— 必须在 MS 登录逻辑之前声明
  const playerNameInput = document.getElementById('playerNameInput');
  const cachedPlayerName = localStorage.getItem('playerName');
  if (cachedPlayerName && playerNameInput) {
    playerNameInput.value = cachedPlayerName;
  }
  if (playerNameInput) {
    playerNameInput.addEventListener('change', () => {
      const name = playerNameInput.value.trim();
      if (name) {
        localStorage.setItem('playerName', name);
        // 如果当前是离线模式，更新侧栏
        if (localStorage.getItem('loginMode') !== 'online') {
          updateSidebarPlayer(name, null);
        }
      }
    });
  }

  // 微软登录
  const msLoginBtn = document.getElementById('msLoginBtn');
  const accountDisplay = document.getElementById('accountDisplay');
  const msLoginInfo = document.getElementById('msLoginInfo');
  const msLoginUrl = document.getElementById('msLoginUrl');
  const msLoginCode = document.getElementById('msLoginCode');

  // 恢复已登录状态
  const cachedProfile = localStorage.getItem('msProfile');
  if (cachedProfile && accountDisplay) {
    const profile = JSON.parse(cachedProfile);
    accountDisplay.textContent = `✅ ${profile.name}`;
    accountDisplay.title = `UUID: ${profile.uuid}`;
  }

  if (msLoginBtn) {
    msLoginBtn.addEventListener('click', async () => {
      try {
        const tauri = await waitForTauri();
        msLoginBtn.textContent = '⏳ 浏览器登录中...';
        msLoginBtn.disabled = true;
        if (accountDisplay) accountDisplay.textContent = '⏳ 等待登录...';

        // 一键登录：打开浏览器 → 自动回调 → 返回玩家档案
        const profile = await tauri.core.invoke('start_ms_login');

        // 登录成功
        if (accountDisplay) {
          accountDisplay.textContent = `✅ ${profile.name}`;
          accountDisplay.title = `UUID: ${profile.uuid}`;
        }
        localStorage.setItem('msProfile', JSON.stringify(profile));
        localStorage.setItem('playerName', profile.name);
        if (playerNameInput) playerNameInput.value = profile.name;
        msLoginBtn.textContent = '🔑 已登录';
        msLoginBtn.disabled = false;
        // 自动切换到正版模式并更新侧栏
        if (typeof setLoginMode === 'function') setLoginMode('online');
        console.log('🎮 正版登录成功:', profile.name);
      } catch (e) {
        console.log('❌ 登录失败:', e);
        if (accountDisplay) {
          accountDisplay.textContent = '❌ 登录失败';
          accountDisplay.title = String(e);
        }
        msLoginBtn.textContent = '🔑 重新登录';
        msLoginBtn.disabled = false;
      }
    });
  }

  // playerNameInput 已在上方声明

  // ===== 侧栏头像和登录模式 =====
  const sidebarPlayerName = document.getElementById('sidebarPlayerName');
  const playerAvatar = document.getElementById('playerAvatar');
  const modeOffline = document.getElementById('modeOffline');
  const modeOnline = document.getElementById('modeOnline');

  function updateSidebarPlayer(name, uuid) {
    if (sidebarPlayerName) sidebarPlayerName.textContent = name || '未登录';
    if (playerAvatar) {
      playerAvatar.src = uuid
        ? `https://mc-heads.net/avatar/${uuid}/40`
        : `https://mc-heads.net/avatar/${name || 'MHF_Steve'}/40`;
    }
  }

  function setLoginMode(mode) {
    localStorage.setItem('loginMode', mode);
    if (modeOffline && modeOnline) {
      modeOffline.classList.toggle('active', mode === 'offline');
      modeOnline.classList.toggle('active', mode === 'online');
    }
    if (mode === 'online') {
      const profile = JSON.parse(localStorage.getItem('msProfile') || 'null');
      if (profile) {
        updateSidebarPlayer(profile.name, profile.uuid);
      } else {
        updateSidebarPlayer('未登录', null);
      }
    } else {
      const offlineName = localStorage.getItem('playerName') || '离线玩家';
      updateSidebarPlayer(offlineName, null);
    }
  }

  // 初始化模式
  const savedMode = localStorage.getItem('loginMode') || 'offline';
  setLoginMode(savedMode);

  // 模式切换点击
  if (modeOffline) modeOffline.addEventListener('click', () => setLoginMode('offline'));
  if (modeOnline) modeOnline.addEventListener('click', () => setLoginMode('online'));

  // 内存滑块
  const memSlider = document.getElementById('memSlider');
  const memValue = document.getElementById('memValue');
  const sliderLabels = document.querySelector('.set-slider-labels');
  const cachedMem = localStorage.getItem('memAlloc');

  if (memSlider && memValue) {
    memSlider.addEventListener('input', () => {
      memValue.textContent = memSlider.value;
      localStorage.setItem('memAlloc', memSlider.value);
    });

    // 从 Tauri 获取真实系统内存
    (async () => {
      try {
        const tauri = await waitForTauri();
        const totalMB = await tauri.core.invoke('get_system_memory');
        memSlider.max = totalMB;
        const defaultMem = cachedMem || Math.max(1024, Math.floor(totalMB / 2));
        memSlider.value = defaultMem;
        memValue.textContent = defaultMem;
        if (sliderLabels) {
          const labels = sliderLabels.querySelectorAll('span');
          if (labels.length >= 3) {
            labels[0].textContent = '1024MB';
            labels[1].textContent = Math.floor(totalMB / 2) + 'MB';
            labels[2].textContent = totalMB + 'MB';
          }
        }
        console.log(`💾 系统内存：${totalMB}MB`);
      } catch (e) {
        console.log('⚠️ 无法获取系统内存:', e.message);
      }
    })();
  }

  // Java 路径 - 浏览按钮
  const javaBrowseBtn = document.getElementById('javaBrowseBtn');
  const javaPathDisplay = document.getElementById('javaPathDisplay');

  if (javaBrowseBtn && javaPathDisplay) {
    javaBrowseBtn.addEventListener('click', async () => {
      try {
        const tauri = await waitForTauri();
        const selected = await tauri.dialog.open({
          title: '选择 Java 所在的 bin 文件夹',
          directory: true,
        });
        if (selected) {
          const javaPath = selected + '\\java.exe';
          javaPathDisplay.textContent = javaPath;
          javaPathDisplay.title = javaPath;
          localStorage.setItem('selectedJavaPath', javaPath);
        }
      } catch (e) {
        console.log('⚠️ 文件夹选择失败:', e);
      }
    });
  }

  // Java 路径 - 自动查找按钮
  const javaAutoBtn = document.getElementById('javaAutoBtn');
  const javaResults = document.getElementById('javaResults');
  const javaResultsList = document.getElementById('javaResultsList');
  const javaResultsToggle = document.getElementById('javaResultsToggle');
  const javaCount = document.getElementById('javaCount');

  // 根据游戏版本推荐 Java 版本
  function getRecommendedJavaMajor(mcVersion) {
    if (!mcVersion) return 17;
    const parts = mcVersion.split('.');
    const minor = parseInt(parts[1]) || 0;
    const patch = parseInt(parts[2]) || 0;
    if (minor <= 16) return 8;
    if (minor <= 20 && patch <= 4) return 17;
    return 21;
  }
  // 动态获取当前选中版本用于 Java 推荐
  function getCurrentMCVersion() {
    const sel = document.getElementById('versionSelector');
    if (sel && sel.value) {
      // 实例名可能是 "1.20.1" 或 "1.20.1-fabric"，取第一段
      return sel.value.split('-')[0];
    }
    return localStorage.getItem('selectedVersion')?.split('-')[0] || '1.20.1';
  }

  let recommendedMajor = getRecommendedJavaMajor(getCurrentMCVersion());

  // 渲染 Java 列表（复用）
  function renderJavaList(javas, selectedPath) {
    javaCount.textContent = javas.length;
    javaResults.classList.add('has-results');
    javaResultsList.innerHTML = '';

    javas.forEach(java => {
      const item = document.createElement('div');
      item.className = 'java-result-item' + (java.path === selectedPath ? ' selected' : '');
      const isRecommended = java.major === recommendedMajor;
      const shortPath = java.path.split('\\').slice(-3).join('\\');
      item.innerHTML = `
        <span class="java-result-path" title="${java.path}">${shortPath}</span>
        <span class="java-ver-badge ${isRecommended ? 'recommended' : ''}">
          Java ${java.major}${isRecommended ? ' ★推荐' : ''}
        </span>
      `;
      item.addEventListener('click', () => {
        javaPathDisplay.textContent = java.path;
        javaPathDisplay.title = java.path;
        javaResultsList.querySelectorAll('.java-result-item').forEach(i => i.classList.remove('selected'));
        item.classList.add('selected');
        javaResults.classList.remove('open');
        // 保存选择
        localStorage.setItem('selectedJavaPath', java.path);
      });
      javaResultsList.appendChild(item);
    });
  }

  // 启动时从缓存加载
  const cachedJavas = localStorage.getItem('javaSearchResults');
  const cachedSelected = localStorage.getItem('selectedJavaPath');
  if (cachedJavas) {
    try {
      const javas = JSON.parse(cachedJavas);
      if (javas.length > 0) {
        const selectedPath = cachedSelected || javas[0].path;
        javaPathDisplay.textContent = selectedPath;
        javaPathDisplay.title = selectedPath;
        renderJavaList(javas, selectedPath);
      }
    } catch (e) { /* 缓存解析失败，忽略 */ }
  }

  // 自动查找按钮
  if (javaAutoBtn && javaPathDisplay) {
    javaAutoBtn.addEventListener('click', async () => {
      javaAutoBtn.disabled = true;
      javaAutoBtn.textContent = '⏳ 搜索中...';
      javaPathDisplay.textContent = '正在搜索...';
      javaResultsList.innerHTML = '';

      try {
        const tauri = await waitForTauri();
        const javas = await tauri.core.invoke('find_java');

        if (javas && javas.length > 0) {
          // 保存到缓存
          localStorage.setItem('javaSearchResults', JSON.stringify(javas));

          let recommended = javas.find(j => j.major === recommendedMajor);
          let selected = recommended || javas[0];
          javaPathDisplay.textContent = selected.path;
          javaPathDisplay.title = selected.path;
          localStorage.setItem('selectedJavaPath', selected.path);

          renderJavaList(javas, selected.path);
          console.log(`☕ 找到 ${javas.length} 个 Java (推荐: Java ${recommendedMajor})`);
        } else {
          javaPathDisplay.textContent = '❌ 未找到 Java';
          javaResults.classList.remove('has-results');
        }
      } catch (e) {
        javaPathDisplay.textContent = '❌ 搜索失败';
        console.log('⚠️ 自动搜索失败:', e);
      }

      javaAutoBtn.disabled = false;
      javaAutoBtn.textContent = '🔍 自动查找';
    });
  }

  // 折叠/展开搜索结果
  if (javaResultsToggle && javaResults) {
    javaResultsToggle.addEventListener('click', () => {
      javaResults.classList.toggle('open');
    });
  }

  // 开关切换
  document.querySelectorAll('.set-toggle').forEach(toggle => {
    toggle.addEventListener('click', () => {
      toggle.classList.toggle('on');
    });
  });

  // 主题选择 - 切换主页背景
  const heroBg = document.querySelector('.hero-bg');
  // 恢复缓存的主题
  const cachedTheme = localStorage.getItem('selectedTheme');
  if (cachedTheme && heroBg) {
    heroBg.src = cachedTheme;
    // 高亮对应的主题项
    document.querySelectorAll('.set-theme-item').forEach(t => {
      const img = t.querySelector('.set-theme-preview img');
      if (img && img.src.endsWith(cachedTheme.replace(/^assets\//, ''))) {
        document.querySelectorAll('.set-theme-item').forEach(x => x.classList.remove('active'));
        document.querySelectorAll('.set-theme-check').forEach(c => c.remove());
        t.classList.add('active');
        const preview = t.querySelector('.set-theme-preview');
        const check = document.createElement('div');
        check.className = 'set-theme-check';
        check.textContent = '✓';
        preview.appendChild(check);
      }
    });
  }

  document.querySelectorAll('.set-theme-item').forEach(item => {
    item.addEventListener('click', () => {
      document.querySelectorAll('.set-theme-item').forEach(t => t.classList.remove('active'));
      item.classList.add('active');
      document.querySelectorAll('.set-theme-check').forEach(c => c.remove());
      const preview = item.querySelector('.set-theme-preview');
      const check = document.createElement('div');
      check.className = 'set-theme-check';
      check.textContent = '✓';
      preview.appendChild(check);

      // 更换主页背景
      const themeSrc = preview.querySelector('img')?.src;
      if (themeSrc && heroBg) {
        const relativeSrc = preview.querySelector('img').getAttribute('src');
        heroBg.src = relativeSrc;
        localStorage.setItem('selectedTheme', relativeSrc);
      }
    });
  });
}
