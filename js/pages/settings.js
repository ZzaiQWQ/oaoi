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

  // ===== 多账号管理 =====
  const msLoginBtn = document.getElementById('msLoginBtn');
  const accountListEl = document.getElementById('accountList');
  const msLoginStatus = document.getElementById('msLoginStatus');

  function getAccounts() {
    try { return JSON.parse(localStorage.getItem('msAccounts') || '[]'); } catch { return []; }
  }
  function saveAccounts(accounts) { localStorage.setItem('msAccounts', JSON.stringify(accounts)); }
  function getActiveIdx() { return parseInt(localStorage.getItem('activeAccountIdx') || '0') || 0; }
  function setActiveIdx(idx) { localStorage.setItem('activeAccountIdx', String(idx)); }

  const _escHtml = escapeHtml; // 使用全局 utils.js 中的 escapeHtml

  function renderAccountList() {
    if (!accountListEl) return;
    const accounts = getAccounts();
    const activeIdx = getActiveIdx();
    if (accounts.length === 0) {
      accountListEl.innerHTML = '<div class="account-empty">暂无正版账号，点击下方按钮添加</div>';
      return;
    }
    accountListEl.innerHTML = accounts.map((acc, i) => `
      <div class="account-card ${i === activeIdx ? 'active' : ''}" data-idx="${i}">
        <img class="account-card-avatar" src="https://mc-heads.net/avatar/${_escHtml(acc.uuid)}/28" alt="${_escHtml(acc.name)}" onerror="this.onerror=null;this.src='https://crafthead.net/avatar/${_escHtml(acc.uuid)}/28'">
        <div class="account-card-info">
          <div class="account-card-name">${_escHtml(acc.name)}</div>
          <div class="account-card-uuid">${_escHtml(acc.uuid)}</div>
        </div>
        ${i === activeIdx ? '<span class="account-card-badge">使用中</span>' : ''}
        <button class="account-card-del" data-idx="${i}" title="删除此账号">✕</button>
      </div>
    `).join('');

    accountListEl.querySelectorAll('.account-card').forEach(card => {
      card.addEventListener('click', (e) => {
        if (e.target.closest('.account-card-del')) return;
        const idx = parseInt(card.dataset.idx);
        setActiveIdx(idx);
        renderAccountList();
        const acc = accounts[idx];
        if (acc) {
          updateSidebarPlayer(acc.name, acc.uuid);
          if (typeof setLoginMode === 'function') setLoginMode('online');
        }
      });
    });

    accountListEl.querySelectorAll('.account-card-del').forEach(btn => {
      btn.addEventListener('click', async (e) => {
        e.stopPropagation();
        const idx = parseInt(btn.dataset.idx);
        const accs = getAccounts();
        const accountName = accs[idx]?.name || '未知';

        // 弹窗确认删除
        const confirmed = await showConfirm(`确定删除账号 ${accountName} 吗？`, { title: '删除确认', kind: 'danger' });
        if (!confirmed) return;

        accs.splice(idx, 1);
        saveAccounts(accs);
        let ai = getActiveIdx();
        if (idx <= ai && ai > 0) setActiveIdx(ai - 1);
        if (accs.length === 0) setActiveIdx(0);
        renderAccountList();
        if (accs.length === 0 && typeof setLoginMode === 'function') setLoginMode('offline');
      });
    });
  }

  renderAccountList();

  // 旧数据迁移
  const oldProfile = localStorage.getItem('msProfile');
  if (oldProfile) {
    try {
      const p = JSON.parse(oldProfile);
      const accs = getAccounts();
      if (p.name && !accs.find(a => a.uuid === p.uuid)) {
        accs.push(p);
        saveAccounts(accs);
        setActiveIdx(accs.length - 1);
        renderAccountList();
      }
    } catch (e) { console.warn('[auth] 迁移旧账号失败:', e); }
    localStorage.removeItem('msProfile');
  }

  // 启动时自动刷新所有账号的 token（静默，不打扰用户）
  (async () => {
    try {
      const tauri = await waitForTauri();
      const accs = getAccounts();
      let changed = false;
      for (let i = 0; i < accs.length; i++) {
        if (accs[i].refresh_token) {
          try {
            const refreshed = await tauri.core.invoke('refresh_ms_login', { refreshToken: accs[i].refresh_token });
            accs[i].access_token = refreshed.access_token;
            if (refreshed.refresh_token) accs[i].refresh_token = refreshed.refresh_token;
            accs[i].name = refreshed.name;
            changed = true;
            console.log(`🔄 账号 ${refreshed.name} token 已刷新`);
          } catch (e) {
            console.warn(`⚠️ 账号 ${accs[i].name} 刷新失败 (需重新登录):`, e);
          }
        }
      }
      if (changed) {
        saveAccounts(accs);
        renderAccountList();
      }
    } catch (e) { console.warn('[auth] 刷新Token失败:', e); }
  })();

  let msLoginPending = false;
  if (msLoginBtn) {
    msLoginBtn.addEventListener('click', async () => {
      // 已在等待中 → 用户点击取消
      if (msLoginPending) {
        msLoginPending = false;
        msLoginBtn.textContent = '➕ 添加微软账号';
        msLoginBtn.disabled = false;
        if (msLoginStatus) { msLoginStatus.textContent = '已取消登录'; setTimeout(() => { msLoginStatus.style.display = 'none'; }, 2000); }
        // 通知后端停止等待
        try { const t = await waitForTauri(); await t.core.invoke('cancel_ms_login'); } catch (e) { console.warn('[auth] 取消登录失败:', e); }
        return;
      }
      try {
        const tauri = await waitForTauri();
        msLoginPending = true;
        msLoginBtn.textContent = '✕ 取消登录';
        msLoginBtn.disabled = false; // 保持可点击，用于取消
        if (msLoginStatus) { msLoginStatus.style.display = ''; msLoginStatus.textContent = '请在浏览器中完成微软登录...'; }

        const profile = await tauri.core.invoke('start_ms_login');
        msLoginPending = false;

        const accs = getAccounts();
        const existIdx = accs.findIndex(a => a.uuid === profile.uuid);
        if (existIdx >= 0) { accs[existIdx] = profile; setActiveIdx(existIdx); }
        else { accs.push(profile); setActiveIdx(accs.length - 1); }
        saveAccounts(accs);
        renderAccountList();

        msLoginBtn.textContent = '➕ 添加微软账号';
        msLoginBtn.disabled = false;
        if (msLoginStatus) { msLoginStatus.textContent = `✅ ${profile.name} 登录成功`; setTimeout(() => { msLoginStatus.style.display = 'none'; }, 3000); }
        updateSidebarPlayer(profile.name, profile.uuid);
        if (typeof setLoginMode === 'function') setLoginMode('online');
      } catch (e) {
        msLoginPending = false;
        msLoginBtn.textContent = '➕ 添加微软账号';
        msLoginBtn.disabled = false;
        if (msLoginStatus) { msLoginStatus.style.display = ''; msLoginStatus.textContent = `❌ 登录失败: ${e}`; }
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
      const id = uuid || name || 'MHF_Steve';
      playerAvatar.src = `https://mc-heads.net/avatar/${id}/40`;
      playerAvatar.onerror = () => {
        // mc-heads 挂了，用 crafthead 备用
        playerAvatar.onerror = null; // 防止无限循环
        playerAvatar.src = `https://crafthead.net/avatar/${id}/40`;
      };
    }
  }

  function setLoginMode(mode) {
    localStorage.setItem('loginMode', mode);
    if (modeOffline && modeOnline) {
      modeOffline.classList.toggle('active', mode === 'offline');
      modeOnline.classList.toggle('active', mode === 'online');
    }
    if (mode === 'online') {
      const accs = getAccounts();
      const ai = getActiveIdx();
      const profile = accs[ai] || null;
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
    // 滑块 → 数字输入
    memSlider.addEventListener('input', () => {
      memValue.value = memSlider.value;
      localStorage.setItem('memAlloc', memSlider.value);
    });
    // 数字输入 → 滑块
    memValue.addEventListener('input', () => {
      const v = parseInt(memValue.value) || 1024;
      memSlider.value = v;
      localStorage.setItem('memAlloc', v);
    });

    // 从 Tauri 获取真实系统内存
    (async () => {
      try {
        const tauri = await waitForTauri();
        const totalMB = await tauri.core.invoke('get_system_memory');
        memSlider.max = totalMB;
        memValue.max = totalMB;
        const defaultMem = cachedMem ? Math.min(parseInt(cachedMem), totalMB) : Math.max(1024, Math.floor(totalMB / 2));
        memSlider.value = defaultMem;
        memValue.value = defaultMem;
        if (!cachedMem) localStorage.setItem('memAlloc', defaultMem);
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
  // 自定义 JVM 参数
  const jvmArgsInput = document.getElementById('customJvmArgs');
  if (jvmArgsInput) {
    jvmArgsInput.value = localStorage.getItem('customJvmArgs') || '';
    jvmArgsInput.addEventListener('input', () => {
      localStorage.setItem('customJvmArgs', jvmArgsInput.value);
    });
  }

  // Java 模式切换（自动/手动）
  const javaModeAuto = document.getElementById('javaModeAuto');
  const javaModeManual = document.getElementById('javaModeManual');
  const javaAutoDesc = document.getElementById('javaAutoDesc');
  const javaManualControls = document.getElementById('javaManualControls');

  function setJavaMode(mode) {
    localStorage.setItem('javaMode', mode);
    if (mode === 'auto') {
      javaModeAuto?.classList.add('active');
      javaModeManual?.classList.remove('active');
      if (javaAutoDesc) javaAutoDesc.style.display = '';
      if (javaManualControls) javaManualControls.style.display = 'none';
    } else {
      javaModeAuto?.classList.remove('active');
      javaModeManual?.classList.add('active');
      if (javaAutoDesc) javaAutoDesc.style.display = 'none';
      if (javaManualControls) javaManualControls.style.display = '';
    }
  }

  // 恢复保存的模式
  const savedJavaMode = localStorage.getItem('javaMode') || 'auto';
  setJavaMode(savedJavaMode);

  javaModeAuto?.addEventListener('click', () => setJavaMode('auto'));
  javaModeManual?.addEventListener('click', () => setJavaMode('manual'));

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

  // 根据当前选中版本推荐 Java 版本
  let recommendedMajor = (() => {
    const sel = document.getElementById('versionSelector');
    const mcVer = sel?.value?.split('-')[0] || localStorage.getItem('selectedVersion')?.split('-')[0] || '1.20.1';
    return typeof getRequiredJavaMajor === 'function' ? getRequiredJavaMajor(mcVer) : 21;
  })();

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
          Java ${java.major}
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
        const javas = await tauri.core.invoke('find_java', { gameDir: localStorage.getItem('gameDir') || '' });

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

    // 手动模式下，如果从未搜索过 Java，首次自动搜索一次
    if (!localStorage.getItem('javaSearchResults')) {
      javaAutoBtn.click();
    }
  }

  // 折叠/展开搜索结果
  if (javaResultsToggle && javaResults) {
    javaResultsToggle.addEventListener('click', () => {
      javaResults.classList.toggle('open');
    });
  }


  // ============ 设置页左侧 Tab 切换 ============
  document.querySelectorAll('.set-left-tab').forEach(tab => {
    tab.addEventListener('click', () => {
      document.querySelectorAll('.set-left-tab').forEach(t => t.classList.remove('active'));
      tab.classList.add('active');
      const target = tab.dataset.setTab;
      document.getElementById('setTabPerf').style.display = target === 'perf' ? '' : 'none';
      document.getElementById('setTabAi').style.display   = target === 'ai'   ? '' : 'none';
    });
  });

  // ============ AI 崩溃分析设置 ============
  initAiSettings();
}

function initAiSettings() {
  const apiKeyInput  = document.getElementById('aiApiKey');
  const apiUrlInput  = document.getElementById('aiApiUrl');
  const modelInput   = document.getElementById('aiModel');
  const testBtn      = document.getElementById('aiTestBtn');
  const testResult   = document.getElementById('aiTestResult');
  if (!apiKeyInput) return;

  // 加载保存的配置
  apiKeyInput.value = localStorage.getItem('ai_api_key') || '';
  apiUrlInput.value = localStorage.getItem('ai_api_url') || '';
  modelInput.value  = localStorage.getItem('ai_model') || '';

  // 实时保存输入
  apiKeyInput.addEventListener('input', () => localStorage.setItem('ai_api_key', apiKeyInput.value.trim()));
  apiUrlInput.addEventListener('input', () => localStorage.setItem('ai_api_url', apiUrlInput.value.trim()));
  modelInput.addEventListener('input',  () => localStorage.setItem('ai_model', modelInput.value.trim()));

  // 测试连接
  if (testBtn) {
    testBtn.addEventListener('click', async () => {
      const key   = apiKeyInput.value.trim();
      const url   = apiUrlInput.value.trim();
      const model = modelInput.value.trim();
      if (!key) { testResult.textContent = '❌ 请先填写 API Key'; testResult.style.color = '#ef4444'; return; }
      if (!url) { testResult.textContent = '❌ 请先填写 API 地址'; testResult.style.color = '#ef4444'; return; }

      testResult.textContent = '⏳ 测试中...';
      testResult.style.color = 'var(--text-mid)';
      testBtn.disabled = true;

      try {
        const resp = await callAiApi(key, url, model, '你好，请回复"连接成功"四个字。');
        if (resp) {
          testResult.textContent = '✅ 连接成功：' + resp.substring(0, 30);
          testResult.style.color = '#22c55e';
        } else {
          testResult.textContent = '❌ 无响应';
          testResult.style.color = '#ef4444';
        }
      } catch (e) {
        testResult.textContent = '❌ ' + (e.message || '连接失败');
        testResult.style.color = '#ef4444';
      }
      testBtn.disabled = false;
    });
  }
}

/**
 * AI API 调用（OpenAI 兼容格式，支持 DeepSeek / OpenAI / 任何兼容服务）
 */
async function callAiApi(apiKey, apiUrl, model, userMessage, signal) {
  const endpoint = apiUrl.includes('/v1/') ? apiUrl : `${apiUrl}/v1/chat/completions`;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 30000); // 30秒超时
  // 外部取消时也中止请求
  if (signal) signal.addEventListener('abort', () => controller.abort());
  try {
    const resp = await fetch(endpoint, {
      method: 'POST',
      signal: controller.signal,
      headers: {
        'Content-Type': 'application/json',
        'Authorization': `Bearer ${apiKey}`
      },
      body: JSON.stringify({
        model: model,
        messages: [
          { role: 'system', content: '你是 oaoi Minecraft 启动器内置的崩溃日志分析专家。用户正在使用 oaoi 启动器。分析日志后用中文给出：1.崩溃原因 2.涉及的Mod/组件 3.解决方案。简洁明了，不超过200字。注意：绝对不要推荐用户更换其他启动器（如 HMCL、PCL、BakaXL 等），所有解决方案必须基于 oaoi 启动器本身。' },
          { role: 'user', content: userMessage }
        ],
        max_tokens: 500
      })
    });
    if (!resp.ok) throw new Error(`HTTP ${resp.status}: ${await resp.text()}`);
    const data = await resp.json();
    return data?.choices?.[0]?.message?.content || '';
  } finally {
    clearTimeout(timeout);
  }
}
