// ============ 整合包拖拽导入（全局） ============
// 替换旧版简单文字状态，改用浮动胶囊 + 详情弹窗
async function initModpackDrop() {
  const overlay = document.getElementById('modpackDropOverlay');
  const pill = document.getElementById('modpackDropPill');
  if (!overlay || !pill) return;

  const tauri = await waitForTauri();

  // 显示/隐藏拖拽蒙层（由 Rust 端的 DragDropEvent 触发）
  await tauri.event.listen('modpack-drag-enter', () => {
    overlay.style.visibility = 'visible';
    overlay.style.opacity = '1';
  });
  await tauri.event.listen('modpack-drag-leave', () => {
    overlay.style.visibility = 'hidden';
    overlay.style.opacity = '0';
  });

  // 接收文件路径，执行安装
  await tauri.event.listen('modpack-drop', async (evt) => {
    overlay.style.visibility = 'hidden';
    overlay.style.opacity = '0';

    const filePath = evt.payload?.path;
    if (!filePath) return;

    const gameDir = localStorage.getItem('gameDir') || '';
    const javaPath = localStorage.getItem('selectedJavaPath') || '';
    const useMirror = (localStorage.getItem('downloadSource') || 'official') === 'bmcl';
    const displayName = filePath.split(/[/\\]/).pop() || '整合包';
    const dlId = 'droppill-' + Date.now();

    // 1. 渲染浮动胶囊
    pill.className = 'drop-pill';
    pill.style.display = 'block';
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
    pill.onclick = () => {
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
          setTimeout(() => {
            pill.style.animation = 'dropPillSlideOut 0.3s ease forwards';
            setTimeout(() => { pill.style.display = 'none'; pill.innerHTML = ''; }, 300);
            const modal = document.getElementById('dlDetailModal');
            if (modal) modal.classList.add('hidden');
          }, 2500);
          if (unlisten) unlisten();
        } else if (stage === 'error' || stage === 'cancelled') {
          const icon = stage === 'cancelled' ? '🚫' : '❌';
          const msg = stage === 'cancelled' ? '已取消' : detail;
          if (statusEl) statusEl.textContent = `${icon} ${msg}`;
          pill.classList.add('error');
          stagesData.innerHTML = `<div class="dl-stage-row"><span class="dl-stage-label" style="color:#ef4444">${icon} ${msg}</span></div>`;
          const modalStages = document.getElementById('dlDetailStages');
          if (modalStages) modalStages.innerHTML = stagesData.innerHTML;
          setTimeout(() => {
            pill.style.animation = 'dropPillSlideOut 0.3s ease forwards';
            setTimeout(() => { pill.style.display = 'none'; pill.innerHTML = ''; }, 300);
          }, 3000);
          if (unlisten) unlisten();
        } else {
          // 正常进度更新
          const label = STAGE_LABELS2[stage] || stage;
          if (total > 0) {
            let progressText;
            if (stage === 'downloading') {
              progressText = `${(current / 1048576).toFixed(1)}MB / ${(total / 1048576).toFixed(1)}MB`;
            } else {
              progressText = `${current}/${total}`;
            }
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
            if (countEl) countEl.textContent = `${current}/${total}`;
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
      });
    } catch (err) {
      if (unlisten) unlisten();
      if (statusEl) statusEl.textContent = `❌ 导入失败: ${err}`;
      pill.classList.add('error');
      console.error('[modpack-drop]', err);
      setTimeout(() => {
        pill.style.animation = 'dropPillSlideOut 0.3s ease forwards';
        setTimeout(() => { pill.style.display = 'none'; pill.innerHTML = ''; }, 300);
      }, 4000);
    }
  });
}
