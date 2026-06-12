// ============ 离线模式地区策略 ============

const OAOI_OFFLINE_REGION_MESSAGE = '当前地区暂不开放离线模式，请先登录正版账号后再启动游戏。';
const OAOI_OFFLINE_POLICY_CACHE_KEY = 'offlinePolicyCache';
const OAOI_OFFLINE_POLICY_CACHE_TTL = 15 * 24 * 60 * 60 * 1000;

function readOfflinePolicyCache() {
  try {
    const cached = JSON.parse(localStorage.getItem(OAOI_OFFLINE_POLICY_CACHE_KEY) || 'null');
    if (!cached || !cached.policy || !cached.savedAt) return null;
    if (Date.now() - Number(cached.savedAt) > OAOI_OFFLINE_POLICY_CACHE_TTL) return null;
    return cached.policy;
  } catch {
    return null;
  }
}

function writeOfflinePolicyCache(policy) {
  try {
    localStorage.setItem(OAOI_OFFLINE_POLICY_CACHE_KEY, JSON.stringify({
      savedAt: Date.now(),
      policy,
    }));
  } catch (e) {
    console.warn('[offline-policy] 保存缓存失败:', e);
  }
}

async function getOfflinePolicy(forceRefresh = false) {
  if (!forceRefresh) {
    const cached = readOfflinePolicyCache();
    if (cached) return cached;
  }

  const tauri = await waitForTauri();
  const policy = await tauri.core.invoke('get_offline_policy');
  writeOfflinePolicyCache(policy);
  return policy;
}

function hasOnlineAccountLogin() {
  try {
    const accounts = JSON.parse(localStorage.getItem('msAccounts') || '[]');
    const activeIdx = parseInt(localStorage.getItem('activeAccountIdx') || '0', 10) || 0;
    const account = Array.isArray(accounts) ? accounts[activeIdx] : null;
    return !!(account && account.access_token && account.uuid);
  } catch {
    return false;
  }
}

async function ensureOfflineModeAllowed(options = {}) {
  // 已经登录正版账号时，离线模式不再检查地区策略。
  if (hasOnlineAccountLogin()) return true;

  const silent = !!options.silent;
  try {
    const policy = await getOfflinePolicy(false);
    if (policy && policy.offline_allowed) return true;
  } catch (e) {
    console.warn('[offline-policy] 读取离线策略失败:', e);
  }

  if (!silent && typeof showToast === 'function') {
    showToast(OAOI_OFFLINE_REGION_MESSAGE, 'warn', 9000);
  }
  return false;
}

async function refreshOfflineModeAvailability() {
  const offlineBtn = document.getElementById('modeOffline');
  if (!offlineBtn) return;
  try {
    const policy = await getOfflinePolicy(false);
    offlineBtn.title = policy.offline_allowed ? '使用离线玩家名启动' : OAOI_OFFLINE_REGION_MESSAGE;
    offlineBtn.classList.toggle('restricted', !policy.offline_allowed);
  } catch {
    offlineBtn.title = OAOI_OFFLINE_REGION_MESSAGE;
  }
}

window.OAOI_OFFLINE_REGION_MESSAGE = OAOI_OFFLINE_REGION_MESSAGE;
window.getOfflinePolicy = getOfflinePolicy;
window.hasOnlineAccountLogin = hasOnlineAccountLogin;
window.ensureOfflineModeAllowed = ensureOfflineModeAllowed;
window.refreshOfflineModeAvailability = refreshOfflineModeAvailability;
