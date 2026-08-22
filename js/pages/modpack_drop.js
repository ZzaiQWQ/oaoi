// ============ 整合包拖拽导入（全局） ============
// 替换旧版简单文字状态，改用浮动胶囊 + 详情弹窗
async function initModpackDrop() {
  const overlay = document.getElementById('modpackDropOverlay');
  const pill = document.getElementById('modpackDropPill');
  if (!overlay || !pill) return;
  pill.setAttribute('data-no-drag', '');
  pill.addEventListener('mousedown', (event) => {
    event.preventDefault();
    event.stopPropagation();
  });
  pill.addEventListener('click', (event) => {
    event.stopPropagation();
  });
  let activeDropId = '';
  let hideTimer = null;
  let cleanupTimer = null;
  let receiveTimer = null;

  function setOverlayText(title, desc, icon = '📦') {
    const iconEl = overlay.querySelector('.drop-icon');
    const titleEl = overlay.querySelector('.drop-title');
    const descEl = overlay.querySelector('.drop-desc');
    if (iconEl) iconEl.textContent = icon;
    if (titleEl) titleEl.textContent = title;
    if (descEl) descEl.textContent = desc;
  }

  function showDropOverlay(mode, fileName = '') {
    if (receiveTimer) {
      clearTimeout(receiveTimer);
      receiveTimer = null;
    }
    overlay.classList.toggle('is-received', mode === 'received' || mode === 'blocked');
    overlay.classList.add('is-visible');
    overlay.style.visibility = 'visible';
    overlay.style.opacity = '1';
    if (mode === 'received') {
      setOverlayText('正在导入整合包', fileName || '后台开始处理整合包', '✓');
    } else if (mode === 'blocked') {
      setOverlayText('请先选择游戏目录', '整合包尚未开始导入', '!');
    } else {
      setOverlayText('松开以导入整合包', '支持 CurseForge (.zip) 和 Modrinth (.mrpack)', '📦');
    }
  }

  function hideDropOverlay(delay = 0) {
    if (receiveTimer) clearTimeout(receiveTimer);
    receiveTimer = setTimeout(() => {
      overlay.classList.remove('is-visible', 'is-received');
      overlay.style.visibility = 'hidden';
      overlay.style.opacity = '0';
      receiveTimer = null;
    }, delay);
  }

  function clearDropTimers() {
    if (hideTimer) {
      clearTimeout(hideTimer);
      hideTimer = null;
    }
    if (cleanupTimer) {
      clearTimeout(cleanupTimer);
      cleanupTimer = null;
    }
  }

  function hidePillLater(dropId, delay, hideModal) {
    clearDropTimers();
    hideTimer = setTimeout(() => {
      if (activeDropId !== dropId) return;
      pill.style.animation = 'dropPillSlideOut 0.3s ease forwards';
      cleanupTimer = setTimeout(() => {
        if (activeDropId !== dropId) return;
        pill.style.display = 'none';
        pill.innerHTML = '';
      }, 300);
      if (hideModal) {
        closeDownloadDetail(dropId);
      }
    }, delay);
  }

  const tauri = await waitForTauri();
  const pageEventUnlisteners = [];
  const listenPageEvent = async (eventName, handler) => {
    try {
      const unlisten = await tauri.event.listen(eventName, handler);
      pageEventUnlisteners.push(unlisten);
      return unlisten;
    } catch (error) {
      pageEventUnlisteners.splice(0).forEach(unlisten => {
        try { unlisten(); } catch (cleanupError) { console.warn('[modpack-drop] 清理页面监听器失败:', cleanupError); }
      });
      throw error;
    }
  };
  let dragDepth = 0;
  let dragHoverTimer = null;

  function hasDraggedFiles(event) {
    const types = event.dataTransfer?.types;
    return !!types && Array.from(types).includes('Files');
  }

  function scheduleDragOverlayHide(delay = 260) {
    if (dragHoverTimer) clearTimeout(dragHoverTimer);
    dragHoverTimer = setTimeout(() => {
      dragHoverTimer = null;
      dragDepth = 0;
      if (!overlay.classList.contains('is-received')) hideDropOverlay();
    }, delay);
  }

  function keepDragOverlayVisible() {
    if (dragHoverTimer) {
      clearTimeout(dragHoverTimer);
      dragHoverTimer = null;
    }
    showDropOverlay(localStorage.getItem('gameDir') ? 'ready' : 'blocked');
    scheduleDragOverlayHide(420);
  }

  document.addEventListener('dragenter', (event) => {
    if (!hasDraggedFiles(event)) return;
    event.preventDefault();
    dragDepth += 1;
    keepDragOverlayVisible();
  }, true);

  document.addEventListener('dragover', (event) => {
    if (!hasDraggedFiles(event)) return;
    event.preventDefault();
    keepDragOverlayVisible();
  }, true);

  document.addEventListener('dragleave', (event) => {
    if (!hasDraggedFiles(event)) return;
    dragDepth = Math.max(0, dragDepth - 1);
    if (dragDepth === 0) scheduleDragOverlayHide(180);
  }, true);

  document.addEventListener('drop', (event) => {
    if (!hasDraggedFiles(event)) return;
    dragDepth = 0;
    if (dragHoverTimer) {
      clearTimeout(dragHoverTimer);
      dragHoverTimer = null;
    }
  }, true);

  // 显示/隐藏拖拽蒙层（由 Rust 端的 DragDropEvent 触发）
  await listenPageEvent('modpack-drag-enter', () => {
    dragDepth = Math.max(1, dragDepth);
    keepDragOverlayVisible();
  });
  await listenPageEvent('modpack-drag-leave', () => {
    dragDepth = 0;
    scheduleDragOverlayHide(220);
  });

  const dropQueue = [];
  let dropQueueRunning = false;

  async function processDroppedModpack(job) {
    const { filePath, gameDir, javaPath, useMirror } = job;
    const displayName = filePath.split(/[/\\]/).pop() || '整合包';
    if (!String(gameDir || '').trim()) {
      showDropOverlay('blocked');
      hideDropOverlay(2200);
      showToast('请先选择游戏目录，再导入整合包', 'warn');
      return;
    }
    if (!acquireInstallEventName(displayName)) {
      showToast(`同名安装任务正在进行: ${displayName}`, 'warn');
      return;
    }
    const dlId = createUiTaskId('droppill');
    dragDepth = 0;
    if (dragHoverTimer) {
      clearTimeout(dragHoverTimer);
      dragHoverTimer = null;
    }
    showDropOverlay('received', displayName);
    hideDropOverlay(900);
    activeDropId = dlId;
    clearDropTimers();

    // 1. 渲染浮动胶囊
    pill.className = 'drop-pill';
    pill.setAttribute('data-no-drag', '');
    pill.dataset.dropId = dlId;
    pill.style.display = 'block';
    pill.style.animation = '';
    pill.innerHTML = `
      <div class="drop-pill-header">
        <div class="drop-pill-title">
          <span class="drop-pill-icon">📦</span>
          <span id="${dlId}-name">${escapeHtml(displayName)}</span>
        </div>
        <button class="dl-cancel-btn" id="${dlId}-cancel" type="button" title="取消安装" disabled>✕</button>
        <span class="drop-pill-expand">点击查看详情 ›</span>
      </div>
      <div class="drop-pill-status" id="${dlId}-status">准备中...</div>
      <div class="drop-pill-bar-wrap">
        <div class="drop-pill-bar" id="${dlId}-bar"></div>
      </div>
    `;

    // 隐藏的 stages 容器（用于存储详细进度数据）
    const stagesData = document.createElement('div');
    stagesData.id = `${dlId}-stages`;
    stagesData.className = 'dl-progress-stages';
    stagesData.style.display = 'none';
    pill.appendChild(stagesData);

    // 2. 点击胶囊打开详情弹窗
    pill.onclick = (event) => {
      event.preventDefault();
      event.stopPropagation();
      openDownloadDetail(dlId, displayName, stagesData);
    };

    // 3. 监听安装进度
    const statusEl = document.getElementById(`${dlId}-status`);
    const barEl = document.getElementById(`${dlId}-bar`);
    const cancelBtn = document.getElementById(`${dlId}-cancel`);
    let unlisten = null;
    let terminalSettled = false;
    let terminalWatchdog = null;
    let resolveTerminal = null;
    const terminalPromise = new Promise(resolve => {
      resolveTerminal = resolve;
    });

    const cancelController = createInstallCancelController({
      tauri,
      fileName: displayName,
      gameDir,
      getCancelButton: () => cancelBtn,
      getStatusElement: () => statusEl,
      requireBackendReady: true,
      logPrefix: 'modpack-drop',
    });

    cancelBtn?.addEventListener('click', async (event) => {
      event.preventDefault();
      event.stopPropagation();
      await cancelController.requestCancel();
    });

    const finishTask = (stage) => {
      if (terminalSettled) return;
      terminalSettled = true;
      if (terminalWatchdog) {
        clearTimeout(terminalWatchdog);
        terminalWatchdog = null;
      }
      cancelController.settle();
      releaseInstallEventName(displayName);
      try {
        if (unlisten) unlisten();
      } catch (err) {
        console.warn('[modpack-drop] 清理进度监听失败:', err);
      }
      resolveTerminal(stage);
    };

    const resetTerminalWatchdog = () => {
      if (terminalWatchdog) clearTimeout(terminalWatchdog);
      terminalWatchdog = setTimeout(async () => {
        if (terminalSettled) return;
        try {
          await cancelController.requestCancel();
        } catch (cancelError) {
          console.warn('[modpack-drop] 超时取消安装失败:', cancelError);
        }
        if (terminalSettled) return;
        const detail = '安装进度长时间没有更新，已结束当前队列等待';
        renderInstallTerminal({ taskId: dlId, stage: 'error', detail, statusElement: statusEl, mainBar: barEl, stagesContainer: stagesData, cancelButton: cancelBtn });
        pill.classList.add('error');
        hidePillLater(dlId, 5000, false);
        finishTask('timeout');
      }, 300000);
    };

    try {
      unlisten = await tauri.event.listen('install-progress', (event) => {
        const { name: evtName, stage, current, total, detail } = event.payload || {};
        // 拖拽导入后端统一用 displayName 发进度，必须精确匹配，避免同名前缀任务串进度。
        if (evtName !== displayName) return;
        resetTerminalWatchdog();
        cancelController.markBackendReady();
        if (stage === 'done') {
          renderInstallTerminal({ taskId: dlId, stage, detail, statusElement: statusEl, mainBar: barEl, stagesContainer: stagesData, cancelButton: cancelBtn });
          pill.classList.add('done');
          if (typeof loadInstalledVersions === 'function') loadInstalledVersions();
          hidePillLater(dlId, 2500, true);
          finishTask(stage);
        } else if (stage === 'error' || stage === 'cancelled') {
          renderInstallTerminal({ taskId: dlId, stage, detail, statusElement: statusEl, mainBar: barEl, stagesContainer: stagesData, cancelButton: cancelBtn });
          pill.classList.add('error');
          hidePillLater(dlId, 3000, false);
          finishTask(stage);
        } else {
          renderInstallProgress({ taskId: dlId, stage, current, total, detail, statusElement: statusEl, mainBar: barEl, stagesContainer: stagesData });
        }
      });

      // 4. 发起安装
      try {
        resetTerminalWatchdog();
        await tauri.core.invoke('import_modpack', {
          zipPath: filePath,
          gameDir,
          javaPath,
          useMirror,
          displayName,
        });
      } catch (err) {
        if (!terminalSettled) {
          renderInstallTerminal({ taskId: dlId, stage: 'error', detail: `导入失败: ${err}`, statusElement: statusEl, mainBar: barEl, stagesContainer: stagesData, cancelButton: cancelBtn });
          pill.classList.add('error');
          console.error('[modpack-drop]', err);
          hidePillLater(dlId, 4000, false);
          finishTask('invoke-error');
        }
      }
    } catch (err) {
      if (!terminalSettled) {
        renderInstallTerminal({ taskId: dlId, stage: 'error', detail: `导入失败: ${err}`, statusElement: statusEl, mainBar: barEl, stagesContainer: stagesData, cancelButton: cancelBtn });
        pill.classList.add('error');
        console.error('[modpack-drop]', err);
        hidePillLater(dlId, 4000, false);
        finishTask('setup-error');
      }
    }
    await terminalPromise;
  }

  async function processDropQueue() {
    if (dropQueueRunning) return;
    dropQueueRunning = true;
    try {
      while (dropQueue.length > 0) {
        const job = dropQueue.shift();
        try {
          await processDroppedModpack(job);
        } catch (err) {
          console.error('[modpack-drop] 处理队列失败:', err);
        }
      }
    } finally {
      dropQueueRunning = false;
      if (dropQueue.length > 0) processDropQueue();
    }
  }

  // 单个浮动条按顺序处理任务，避免多个拖拽任务互相覆盖状态。
  await listenPageEvent('modpack-drop', (evt) => {
    const filePath = evt.payload?.path;
    if (!filePath) {
      hideDropOverlay();
      return;
    }
    const displayName = filePath.split(/[/\\]/).pop() || '整合包';
    const gameDir = localStorage.getItem('gameDir') || '';
    if (!String(gameDir).trim()) {
      // 首次目录尚未确认时绝不把空目录交给后端，避免回退到系统默认目录。
      showDropOverlay('blocked');
      hideDropOverlay(2200);
      showToast('请先选择游戏目录，再导入整合包', 'warn');
      return;
    }
    showDropOverlay('received', displayName);
    hideDropOverlay(900);
    // 排队任务必须保留拖入瞬间的目标目录，切换设置不能改变已入队任务。
    dropQueue.push({
      filePath,
      gameDir,
      javaPath: localStorage.getItem('selectedJavaPath') || '',
      useMirror: (localStorage.getItem('downloadSource') || 'official') === 'bmcl',
    });
    processDropQueue();
  });
}
