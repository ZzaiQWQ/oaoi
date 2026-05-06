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
    overlay.classList.toggle('is-received', mode === 'received');
    overlay.classList.add('is-visible');
    overlay.style.visibility = 'visible';
    overlay.style.opacity = '1';
    if (mode === 'received') {
      setOverlayText('正在导入整合包', fileName || '后台开始处理整合包', '✓');
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
        const modal = document.getElementById('dlDetailModal');
        if (modal) modal.classList.add('hidden');
      }
    }, delay);
  }

  const tauri = await waitForTauri();
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
    showDropOverlay('ready');
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
  await tauri.event.listen('modpack-drag-enter', () => {
    dragDepth = Math.max(1, dragDepth);
    keepDragOverlayVisible();
  });
  await tauri.event.listen('modpack-drag-leave', () => {
    dragDepth = 0;
    scheduleDragOverlayHide(220);
  });

  // 处理文件路径，执行安装
  await tauri.event.listen('modpack-drop', async (evt) => {
    const filePath = evt.payload?.path;
    if (!filePath) {
      hideDropOverlay();
      return;
    }

    const gameDir = localStorage.getItem('gameDir') || '';
    const javaPath = localStorage.getItem('selectedJavaPath') || '';
    const useMirror = (localStorage.getItem('downloadSource') || 'official') === 'bmcl';
    const displayName = filePath.split(/[/\\]/).pop() || '整合包';
    const dlId = 'droppill-' + Date.now();
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

    // 2. 点击胶囊打开详情弹窗（复用下载页的 dlDetailModal）
    pill.onclick = (event) => {
      event.preventDefault();
      event.stopPropagation();
      let modal = document.getElementById('dlDetailModal');
      if (!modal) {
        document.body.insertAdjacentHTML('beforeend', `
          <div class="dl-detail-modal hidden" id="dlDetailModal">
            <div class="dl-detail-content">
              <div class="dl-detail-header">
                <span class="dl-detail-title">安装详情</span>
                <button class="dl-detail-close" id="dlDetailClose">✕</button>
              </div>
              <div class="dl-detail-stages" id="dlDetailStages"></div>
            </div>
          </div>
        `);
        modal = document.getElementById('dlDetailModal');
        document.getElementById('dlDetailClose').addEventListener('click', () => {
          modal.classList.add('hidden');
        });
        modal.addEventListener('click', (e) => {
          if (e.target === modal) modal.classList.add('hidden');
        });
      }
      const title = modal.querySelector('.dl-detail-title');
      if (title) title.textContent = `📦 ${displayName}`;
      const modalStages = document.getElementById('dlDetailStages');
      if (modalStages && stagesData) {
        modalStages.innerHTML = stagesData.innerHTML;
      }
      modal.classList.remove('hidden');
    };

    // 3. 监听安装进度
    const STAGE_LABELS2 = (typeof STAGE_LABELS !== 'undefined') ? STAGE_LABELS : {};
    const statusEl = document.getElementById(`${dlId}-status`);
    const barEl = document.getElementById(`${dlId}-bar`);

    let unlisten = null;
    try {
      unlisten = await tauri.event.listen('install-progress', (event) => {
        const { name: evtName, stage, current, total, detail } = event.payload;
        // 只处理与当前拖拽文件匹配的任务（后端用 display_name 或 inst_name）
        if (evtName && evtName !== displayName && !displayName.startsWith(evtName)) return;
        if (stage === 'done') {
          if (statusEl) statusEl.textContent = '✅ 安装完成！';
          if (barEl) { barEl.style.width = '100%'; barEl.style.opacity = '0.5'; }
          pill.classList.add('done');
          stagesData.innerHTML = '<div class="dl-stage-row"><span class="dl-stage-label">✅ 安装完成！</span></div>';
          // 更新弹窗
          const modalStages = document.getElementById('dlDetailStages');
          if (modalStages) modalStages.innerHTML = stagesData.innerHTML;
          if (typeof loadInstalledVersions === 'function') loadInstalledVersions();
          hidePillLater(dlId, 2500, true);
          if (unlisten) unlisten();
        } else if (stage === 'error' || stage === 'cancelled') {
          const icon = stage === 'cancelled' ? '🚫' : '❌';
          const msg = stage === 'cancelled' ? '已取消' : detail;
          if (statusEl) statusEl.textContent = `${icon} ${msg}`;
          pill.classList.add('error');
          stagesData.innerHTML = `<div class="dl-stage-row"><span class="dl-stage-label" style="color:#ef4444">${icon} ${escapeHtml(msg)}</span></div>`;
          const modalStages = document.getElementById('dlDetailStages');
          if (modalStages) modalStages.innerHTML = stagesData.innerHTML;
          hidePillLater(dlId, 3000, false);
          if (unlisten) unlisten();
        } else {
          // 正常进度更新
          const label = STAGE_LABELS2[stage] || stage;
          if (total > 0) {
            const progressText = formatStageProgress(current, total, stage);
            if (statusEl) statusEl.textContent = `${label} ${progressText}`;
            if (barEl) barEl.style.width = Math.min(100, Math.round((current / total) * 100)) + '%';
          } else {
            if (statusEl) statusEl.textContent = label;
          }

          // 更新隐藏的 stages 容器
          const stageElId = `${dlId}-stage-${stage}`;
          let row = document.getElementById(stageElId);
          if (!row) {
            stagesData.insertAdjacentHTML('beforeend', `
              <div class="dl-stage-row" id="${stageElId}">
                <div class="dl-stage-head">
                  <span class="dl-stage-label">${label}</span>
                  <span class="dl-stage-count" id="${stageElId}-count"></span>
                </div>
                <div class="dl-progress-bar-wrap" style="height:4px;">
                  <div class="dl-progress-bar" id="${stageElId}-bar" style="width:0%"></div>
                </div>
              </div>
            `);
            row = document.getElementById(stageElId);
          }
          const countEl = document.getElementById(`${stageElId}-count`);
          const stageBarEl = document.getElementById(`${stageElId}-bar`);
          if (total > 0) {
            const pct = Math.min(100, Math.round((current / total) * 100));
            if (countEl) {
              countEl.textContent = formatStageProgress(current, total, stage);
            }
            if (stageBarEl) stageBarEl.style.width = pct + '%';
            if (pct >= 100 && stageBarEl) stageBarEl.style.opacity = '0.5';
          } else {
            if (countEl) countEl.textContent = detail;
          }

          // 如果弹窗打开着，实时同步
          const modal = document.getElementById('dlDetailModal');
          if (modal && !modal.classList.contains('hidden')) {
            const modalStages = document.getElementById('dlDetailStages');
            if (modalStages) modalStages.innerHTML = stagesData.innerHTML;
          }
        }
      });

      // 4. 发起安装
      await tauri.core.invoke('import_modpack', {
        zipPath: filePath,
        gameDir,
        javaPath,
        useMirror,
        displayName,
      });
    } catch (err) {
      if (unlisten) unlisten();
      if (statusEl) statusEl.textContent = `❌ 导入失败: ${err}`;
      pill.classList.add('error');
      console.error('[modpack-drop]', err);
      hidePillLater(dlId, 4000, false);
    }
  });
}
