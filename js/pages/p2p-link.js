/* ============ 联机工具 - P2P 客户端逻辑 ============ */

function initP2PLink() {
  const root = document.querySelector('[data-server-panel="p2p"]');
  if (!root || root.dataset.p2pReady === 'true') return;
  root.dataset.p2pReady = 'true';

  const SIGNAL_SERVER_URL = window.MC_P2P_CONFIG?.signalServerUrl || 'wss://xl.oaoi.cn';
  const MAX_RECONNECT_ATTEMPTS = 10;

  let ws = null;
  let currentRole = null;
  let reconnectAttempts = 0;
  let reconnectTimer = null;
  let pendingOnOpen = null;
  let logUnlisten = null;

  let peerNegotiations = new Map();
  let activeHostPeers = new Set();
  let step1Done = false;
  let pendingPeerIp = null;
  let myStunIp = null;
  let guestAutoPortAttempt = 0;

  const state = {
    signal: 'disconnected',
    lastLog: '等待操作',
    host: {
      port: '25565',
      roomCode: '',
      status: 'idle',
      busy: false,
      connectedGuestCount: 0,
    },
    guest: {
      roomCode: '',
      localPort: '',
      hostPort: '',
      assignedPort: '',
      status: 'idle',
      busy: false,
    },
  };

  const els = {
    statusBadge: document.getElementById('p2p-signal-status'),
    btnCreate: document.getElementById('p2p-create-room'),
    btnJoin: document.getElementById('p2p-join-room'),
    btnReset: document.getElementById('p2p-reset-connection'),
    btnDetectPort: document.getElementById('p2p-detect-port'),
    hostPort: document.getElementById('p2p-host-port'),
    guestCode: document.getElementById('p2p-guest-room-code'),
    guestLocalPort: document.getElementById('p2p-guest-local-port'),
    hostRoomCode: document.getElementById('p2p-host-room-code'),
    hostGuestCount: document.getElementById('p2p-host-guest-count'),
    logOutput: document.getElementById('p2p-log-output'),
  };

  if (!els.btnCreate || !els.btnJoin || !els.logOutput) return;

  async function invoke(command, args = {}) {
    const tauri = await waitForTauri();
    return tauri.core.invoke(command, args);
  }

  async function listen(eventName, handler) {
    const tauri = await waitForTauri();
    return tauri.event.listen(eventName, handler);
  }

  function parseValidPort(value) {
    const port = parseInt(value, 10);
    return Number.isInteger(port) && port >= 1 && port <= 65535 ? port : 0;
  }

  function getGuestAutoPort(hostPort) {
    const candidates = [];
    const pushPort = (port) => {
      if (port && !candidates.includes(port)) candidates.push(port);
    };

    pushPort(hostPort);
    pushPort(25565);
    for (let port = 25566; port <= 25575; port += 1) {
      pushPort(port);
    }

    return candidates[Math.min(guestAutoPortAttempt, candidates.length - 1)] || 25565;
  }

  function setRoleState(role, patch) {
    Object.assign(state[role], patch);
    render();
  }

  function setSignal(status) {
    state.signal = status;
    render();
  }

  function log(msg, type = '') {
    if (!els.logOutput) return;

    if (els.logOutput.childElementCount === 1 && els.logOutput.textContent.trim() === '等待操作') {
      els.logOutput.innerHTML = '';
    }

    const div = document.createElement('div');
    div.className = `p2p-log-line${type ? ` p2p-log-${type}` : ''}`;
    const time = new Date().toLocaleTimeString();
    div.textContent = `[${time}] ${msg}`;
    els.logOutput.appendChild(div);

    while (els.logOutput.childElementCount > 500) {
      els.logOutput.removeChild(els.logOutput.firstChild);
    }

    els.logOutput.scrollTop = els.logOutput.scrollHeight;
    state.lastLog = msg;
  }

  async function copyRoomCode(roomCode) {
    const code = String(roomCode || '').trim();
    if (!code) return false;

    try {
      await navigator.clipboard.writeText(code);
      return true;
    } catch {
      try {
        const input = document.createElement('input');
        input.value = code;
        input.style.position = 'fixed';
        input.style.left = '-9999px';
        input.style.opacity = '0';
        document.body.appendChild(input);
        input.focus();
        input.select();
        const ok = document.execCommand('copy');
        input.remove();
        return ok;
      } catch {
        return false;
      }
    }
  }

  function render() {
    if (els.statusBadge) {
      const connected = state.signal === 'connected';
      els.statusBadge.textContent = connected ? '信令已连接' : '未连接';
      els.statusBadge.classList.toggle('connected', connected);
      els.statusBadge.classList.toggle('disconnected', !connected);
    }

    if (els.hostPort) els.hostPort.value = state.host.port;
    if (els.guestCode) els.guestCode.value = state.guest.roomCode;
    if (els.guestLocalPort) els.guestLocalPort.value = state.guest.localPort;
    if (els.hostRoomCode) els.hostRoomCode.textContent = state.host.roomCode || '------';
    if (els.hostGuestCount) els.hostGuestCount.textContent = `访客 ${state.host.connectedGuestCount}/7`;

    els.btnCreate.disabled = state.host.busy;
    els.btnCreate.textContent = state.host.roomCode
      ? '重新创建'
      : (state.host.busy ? '创建中...' : '创建房间');

    els.btnJoin.disabled = state.guest.busy;
    if (state.guest.status === 'pairing') {
      els.btnJoin.textContent = '配对中...';
    } else if (state.guest.status === 'ready') {
      els.btnJoin.textContent = '重新连接';
    } else {
      els.btnJoin.textContent = state.guest.busy ? '连接中...' : '加入房间';
    }
  }

  function sendCreateRoomRequest() {
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    ws.send(JSON.stringify({ type: 'create_room' }));
  }

  function sendJoinRoomRequest(roomCode) {
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    ws.send(JSON.stringify({ type: 'join_room', roomCode }));
  }

  function buildSignalReconnectAction() {
    if (currentRole === 'host') {
      return () => {
        peerNegotiations.clear();
        activeHostPeers.clear();
        setRoleState('host', {
          roomCode: '',
          status: 'connecting',
          busy: true,
          connectedGuestCount: 0,
        });
        log('信令已重连，正在重新创建房间...', 'info');
        sendCreateRoomRequest();
      };
    }

    if (currentRole === 'guest') {
      const roomCode = state.guest.roomCode.trim().toUpperCase();
      if (roomCode.length !== 6) return null;

      return () => {
        step1Done = false;
        pendingPeerIp = null;
        myStunIp = null;
        setRoleState('guest', {
          roomCode,
          hostPort: '',
          assignedPort: '',
          status: 'connecting',
          busy: true,
        });
        log(`信令已重连，正在重新加入房间 ${roomCode}...`, 'info');
        sendJoinRoomRequest(roomCode);
      };
    }

    return null;
  }

  function scheduleReconnect() {
    if (reconnectAttempts >= MAX_RECONNECT_ATTEMPTS) {
      log(`已尝试 ${MAX_RECONNECT_ATTEMPTS} 次重连，放弃。请检查网络后手动重试。`, 'error');
      if (currentRole === 'host') setRoleState('host', { busy: false, status: 'error' });
      if (currentRole === 'guest') setRoleState('guest', { busy: false, status: 'error' });
      return;
    }

    const delay = Math.min(1000 * Math.pow(2, reconnectAttempts), 16000);
    reconnectAttempts++;
    log(`${(delay / 1000).toFixed(0)} 秒后第 ${reconnectAttempts} 次重连...`, 'info');
    reconnectTimer = setTimeout(() => {
      connectSignalServer(pendingOnOpen || (() => {}));
    }, delay);
  }

  function connectSignalServer(onOpen) {
    if (ws && ws.readyState === WebSocket.OPEN) {
      onOpen();
      return;
    }

    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }

    pendingOnOpen = onOpen;
    log(`连接信令服务器 (${SIGNAL_SERVER_URL})...`);

    try {
      ws = new WebSocket(SIGNAL_SERVER_URL);
    } catch (err) {
      log(`WebSocket 创建失败: ${err.message}`, 'error');
      scheduleReconnect();
      return;
    }

    const socket = ws;

    socket.onopen = () => {
      if (ws !== socket) return;
      reconnectAttempts = 0;
      setSignal('connected');
      log('信令服务器已连接', 'success');
      if (pendingOnOpen) {
        pendingOnOpen();
        pendingOnOpen = null;
      }
    };

    socket.onclose = (event) => {
      if (ws !== socket) return;
      setSignal('disconnected');
      if (event.code !== 1000) {
        pendingOnOpen = buildSignalReconnectAction();
        log('信令连接断开，准备自动重连并恢复房间状态...', 'error');
        scheduleReconnect();
      } else {
        log('信令连接已关闭', 'info');
      }
    };

    socket.onerror = () => {
      if (ws !== socket) return;
      log('信令服务器连接错误', 'error');
    };

    socket.onmessage = async (event) => {
      if (ws !== socket) return;
      let msg;
      try {
        msg = JSON.parse(event.data);
      } catch {
        log('收到非 JSON 信令数据，已忽略', 'error');
        return;
      }

      switch (msg.type) {
        case 'room_created':
          activeHostPeers.clear();
          setRoleState('host', {
            roomCode: msg.roomCode,
            status: 'waiting',
            busy: false,
            connectedGuestCount: 0,
          });
          if (await copyRoomCode(msg.roomCode)) {
            log(`房间已创建 ${msg.roomCode}，已自动复制`, 'success');
          } else {
            log(`房间已创建 ${msg.roomCode}，但自动复制失败`, 'error');
          }
          break;

        case 'room_joined':
          setRoleState('guest', {
            roomCode: msg.roomCode,
            status: 'pairing',
            busy: true,
          });
          log(`成功加入房间 ${msg.roomCode}，等待配对`, 'success');
          break;

        case 'peer_ready':
          await handlePeerReady(msg.peerId, msg.roomCode);
          break;

        case 'signal':
          await handleSignal(msg);
          break;

        case 'peer_disconnected':
          log(`${msg.peerId || '对方'} 已断线`, 'error');
          if (currentRole === 'host') {
            if (msg.peerId && msg.peerId !== 'host') {
              activeHostPeers.delete(msg.peerId);
              peerNegotiations.delete(msg.peerId);
            }
            setRoleState('host', {
              status: 'waiting',
              connectedGuestCount: activeHostPeers.size,
            });
          }
          if (currentRole === 'guest') setRoleState('guest', { status: 'error', busy: false });
          break;

        case 'error':
          log(`Server Error: ${msg.message}`, 'error');
          if (currentRole === 'host') setRoleState('host', { status: 'error', busy: false });
          if (currentRole === 'guest') setRoleState('guest', { status: 'error', busy: false });
          break;

        default:
          log(`未知信令类型: ${msg.type}`, 'info');
      }
    };
  }

  async function handlePeerReady(peerId, roomCode) {
    log(`与 ${peerId} 开始配对，交换打洞 IP...`, 'success');

    if (currentRole === 'host') {
      setRoleState('host', { status: 'pairing' });
      peerNegotiations.set(peerId, { step1Done: false, pendingPeerIp: null });
      await startP2PNegotiationForPeer(peerId);
      return;
    }

    setRoleState('guest', {
      roomCode: roomCode || state.guest.roomCode,
      status: 'pairing',
      busy: true,
    });
    step1Done = false;
    pendingPeerIp = null;
    await startP2PNegotiation();
  }

  async function handleSignal(msg) {
    if (!msg.data || !msg.data.ip) {
      log('收到无效的 signal 数据，已忽略', 'error');
      return;
    }

    const peerIp = msg.data.ip;
    const peerId = msg.peerId;
    log(`收到 ${peerId} 的打洞地址: ${peerIp}`);

    if (currentRole === 'host') {
      const peerState = peerNegotiations.get(peerId);
      if (peerState && peerState.step1Done) {
        await executeStep2(peerIp, peerId);
      } else if (peerState) {
        peerState.pendingPeerIp = peerIp;
        log(`等待与 ${peerId} 的 STUN 探测完成...`, 'info');
      }
      return;
    }

    const hostPort = parseValidPort(msg.data.hostPort);
    if (hostPort) {
      const shouldLog = state.guest.hostPort !== String(hostPort);
      setRoleState('guest', { hostPort: String(hostPort) });
      if (shouldLog && !state.guest.localPort) {
        log(`收到房主 MC 端口: ${hostPort}，连接端将优先使用同端口`, 'info');
      }
    }

    if (step1Done) {
      await executeStep2(peerIp, peerId);
    } else {
      pendingPeerIp = peerIp;
      log('等待自身 STUN 探测完成...', 'info');
    }
  }

  async function executeStep2(peerIp, peerId) {
    if (currentRole === 'host') {
      try {
        const port = parseInt(state.host.port, 10);
        if (Number.isNaN(port) || port < 1 || port > 65535) {
          log('请输入有效的端口号（1-65535）', 'error');
          setRoleState('host', { status: 'error', busy: false });
          return;
        }

        const peerState = peerNegotiations.get(peerId);
        const stunAddr = peerState ? peerState.myStunIp : '';
        if (!stunAddr) {
          log(`[${peerId}] 无法找到 STUN 地址，跳过`, 'error');
          setRoleState('host', { status: 'error', busy: false });
          return;
        }

        log(`[${peerId}] 正在打洞并建立隧道...`);
        await invoke('host_step2_connect', { guestIp: peerIp, mcPort: port, stunAddr });
        activeHostPeers.add(peerId);
        setRoleState('host', {
          status: 'waiting',
          busy: false,
          connectedGuestCount: activeHostPeers.size,
        });
        log(`访客 ${peerId} 隧道已建立，当前在线 ${activeHostPeers.size}/7`, 'success');
      } catch (err) {
        log(`[${peerId}] 打洞失败: ${err}`, 'error');
        setRoleState('host', { status: 'error', busy: false });
      }
      return;
    }

    try {
      log('正在打洞并建立本地代理...');
      const manualPort = parseValidPort(state.guest.localPort);
      const hostPort = parseValidPort(state.guest.hostPort);
      const localPort = manualPort || getGuestAutoPort(hostPort);
      if (!manualPort) {
        const autoLabel = guestAutoPortAttempt === 0 && hostPort
          ? '跟随房主端口'
          : `自动候选 ${guestAutoPortAttempt + 1}`;
        log(`连接端代理端口选择: ${localPort} (${autoLabel})`, 'info');
      }
      await invoke('guest_step2_connect', { hostIp: peerIp, localPort, stunAddr: myStunIp });
      setRoleState('guest', { status: 'ready', busy: false });
      log('隧道已建立，可以进入 Minecraft。', 'success');
    } catch (err) {
      log(`打洞失败: ${err}`, 'error');
      setRoleState('guest', { status: 'error', busy: false });
    }
  }

  async function startP2PNegotiationForPeer(peerId) {
    try {
      log(`[${peerId}] 调用 Rust 底层探测自身公网 IP...`);
      const peerStunIp = await invoke('step1_get_ip');
      log(`[${peerId}] 自身探测到的公网地址: ${peerStunIp}`);

      const peerState = peerNegotiations.get(peerId);
      if (peerState) {
        peerState.step1Done = true;
        peerState.myStunIp = peerStunIp;
      }

      const hostPort = parseValidPort(state.host.port);
      ws.send(JSON.stringify({
        type: 'signal',
        targetPeerId: peerId,
        data: { ip: peerStunIp, hostPort },
      }));

      if (peerState && peerState.pendingPeerIp) {
        const ip = peerState.pendingPeerIp;
        peerState.pendingPeerIp = null;
        log(`[${peerId}] 检测到缓存的对方 IP，开始打洞...`, 'success');
        await executeStep2(ip, peerId);
      }
    } catch (err) {
      log(`[${peerId}] 获取 IP 失败: ${err}`, 'error');
      setRoleState('host', { status: 'error', busy: false });
    }
  }

  async function startP2PNegotiation() {
    try {
      log('调用 Rust 底层探测自身公网 IP...');
      myStunIp = await invoke('step1_get_ip');
      log(`自身探测到的公网地址: ${myStunIp}`);

      step1Done = true;

      ws.send(JSON.stringify({
        type: 'signal',
        data: { ip: myStunIp },
      }));

      if (pendingPeerIp) {
        const ip = pendingPeerIp;
        pendingPeerIp = null;
        log('检测到缓存的对方 IP，开始打洞...', 'success');
        await executeStep2(ip, 'host');
      }
    } catch (err) {
      log(`获取 IP 失败: ${err}`, 'error');
      setRoleState('guest', { status: 'error', busy: false });
    }
  }

  async function resetConnections() {
    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }

    if (ws) {
      ws.close(1000);
      ws = null;
    }

    pendingOnOpen = null;
    reconnectAttempts = 0;
    currentRole = null;
    peerNegotiations.clear();
    activeHostPeers.clear();
    step1Done = false;
    pendingPeerIp = null;
    myStunIp = null;
    guestAutoPortAttempt = 0;

    setSignal('disconnected');
    setRoleState('host', {
      roomCode: '',
      status: 'idle',
      busy: false,
      connectedGuestCount: 0,
    });
    setRoleState('guest', {
      assignedPort: '',
      status: 'idle',
      busy: false,
    });

    await invoke('reset_connections').catch(() => {});
    log('连接状态已重置', 'info');
  }

  async function closeCurrentSessionForNewAttempt() {
    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }

    if (ws) {
      ws.close(1000);
      ws = null;
    }

    pendingOnOpen = null;
    reconnectAttempts = 0;
    peerNegotiations.clear();
    activeHostPeers.clear();
    step1Done = false;
    pendingPeerIp = null;
    myStunIp = null;
    setSignal('disconnected');
    await invoke('reset_connections').catch(() => {});
  }

  els.hostPort?.addEventListener('input', () => {
    state.host.port = els.hostPort.value;
  });

  els.guestCode?.addEventListener('input', () => {
    state.guest.roomCode = els.guestCode.value.trim().toUpperCase();
    els.guestCode.value = state.guest.roomCode;
  });

  els.guestLocalPort?.addEventListener('input', () => {
    state.guest.localPort = els.guestLocalPort.value;
  });

  els.btnDetectPort?.addEventListener('click', async () => {
    els.btnDetectPort.disabled = true;
    els.btnDetectPort.textContent = '侦测中...';
    try {
      const port = await invoke('detect_mc_port');
      setRoleState('host', { port: String(port) });
      log(`自动侦测到 MC 局域网端口: ${port}`, 'success');
    } catch (err) {
      log(`侦测失败: ${err}`, 'error');
    } finally {
      els.btnDetectPort.disabled = false;
      els.btnDetectPort.textContent = '侦测端口';
    }
  });

  els.btnCreate.addEventListener('click', async () => {
    await closeCurrentSessionForNewAttempt();
    currentRole = 'host';
    peerNegotiations.clear();
    activeHostPeers.clear();
    setRoleState('host', {
      roomCode: '',
      status: 'connecting',
      busy: true,
      connectedGuestCount: 0,
    });

    connectSignalServer(() => {
      sendCreateRoomRequest();
    });
  });

  els.btnJoin.addEventListener('click', async () => {
    const code = state.guest.roomCode.trim().toUpperCase();
    if (code.length !== 6) {
      log('请输入正确的 6 位房间码', 'error');
      setRoleState('guest', { status: 'error', busy: false });
      return;
    }

    const useAutoPort = !parseValidPort(state.guest.localPort);
    const retrySameRoom = currentRole === 'guest' && state.guest.roomCode === code;
    if (useAutoPort && retrySameRoom) {
      guestAutoPortAttempt += 1;
      log(`自动端口切换到下一个候选（第 ${guestAutoPortAttempt + 1} 个）`, 'info');
    } else if (!retrySameRoom) {
      guestAutoPortAttempt = 0;
    }

    await closeCurrentSessionForNewAttempt();
    currentRole = 'guest';
    step1Done = false;
    pendingPeerIp = null;
    myStunIp = null;
    setRoleState('guest', {
      roomCode: code,
      hostPort: '',
      assignedPort: '',
      status: 'connecting',
      busy: true,
    });

    connectSignalServer(() => {
      sendJoinRoomRequest(code);
    });
  });

  els.btnReset?.addEventListener('click', () => {
    resetConnections();
  });

  listen('log', (event) => {
    const msg = String(event.payload || '');
    log(`[Rust] ${msg}`, 'info');

    const portMatch = msg.match(/本地代理端口:\s*(\d+)/);
    if (portMatch) {
      setRoleState('guest', {
        assignedPort: portMatch[1],
        status: 'ready',
        busy: false,
      });
    }

    if (msg.includes('联机准备完毕')) {
      setRoleState('guest', {
        status: 'ready',
        busy: false,
      });
      log('隧道已建立，可以进入 Minecraft。', 'success');
    }
  }).then((unlisten) => {
    logUnlisten = unlisten;
  }).catch((err) => {
    log(`监听底层日志失败: ${err}`, 'error');
  });

  window.addEventListener('beforeunload', () => {
    if (logUnlisten) logUnlisten();
    invoke('reset_connections').catch(() => {});
  });

  render();
}
