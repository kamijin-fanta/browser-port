// BrowserPort - Service Worker (background.js)
// Multi-player assignment management (full feature).

const SETTINGS_STORAGE_KEY = 'browserPortSettings';

const DEFAULT_SETTINGS = {
  wsUrl: 'ws://127.0.0.1:1844',
  bitrate: 30_000_000,
  targetFps: 60,
  maxWidth: 1920,
  maxHeight: 1080,
  latencyMode: 'realtime',
  hardwareAcceleration: 'prefer-hardware',
  keyframeInterval: 120,
};

function createIdleAssignment() {
  return {
    tabId: null,
    status: 'idle',
    stats: null,
  };
}

const state = {
  assignments: {
    1: createIdleAssignment(),
    2: createIdleAssignment(),
    3: createIdleAssignment(),
    4: createIdleAssignment(),
  },
  browserPortConnected: false,
  settings: { ...DEFAULT_SETTINGS },
};

const BROWSER_PORT_CHECK_INTERVAL_MS = 1000;
const HEALTH_PING_INTERVAL_MS = 1000;
const HEALTH_HANDSHAKE_TIMEOUT_MS = 5000;
const HEALTH_PONG_TIMEOUT_MS = 5000;
let healthSocket = null;
let healthHelloAcked = false;
let healthHelloSentAt = 0;
let healthLastPingAt = 0;
let healthLastPongAt = 0;

function clampInt(value, fallback, min, max) {
  const numeric = Number(value);
  if (!Number.isFinite(numeric)) return fallback;
  const floored = Math.floor(numeric);
  return Math.min(Math.max(floored, min), max);
}

function normalizeSettings(input) {
  const source = input && typeof input === 'object' ? input : {};
  const wsUrl = typeof source.wsUrl === 'string' ? source.wsUrl.trim() : '';
  const latencyModes = ['realtime', 'quality'];
  const hardwareModes = ['prefer-hardware', 'prefer-software', 'no-preference'];

  return {
    wsUrl: wsUrl || DEFAULT_SETTINGS.wsUrl,
    bitrate: clampInt(source.bitrate, DEFAULT_SETTINGS.bitrate, 100_000, 200_000_000),
    targetFps: clampInt(source.targetFps, DEFAULT_SETTINGS.targetFps, 1, 240),
    maxWidth: clampInt(source.maxWidth, DEFAULT_SETTINGS.maxWidth, 16, 7680),
    maxHeight: clampInt(source.maxHeight, DEFAULT_SETTINGS.maxHeight, 16, 4320),
    latencyMode: latencyModes.includes(source.latencyMode)
      ? source.latencyMode
      : DEFAULT_SETTINGS.latencyMode,
    hardwareAcceleration: hardwareModes.includes(source.hardwareAcceleration)
      ? source.hardwareAcceleration
      : DEFAULT_SETTINGS.hardwareAcceleration,
    keyframeInterval: clampInt(
      source.keyframeInterval,
      DEFAULT_SETTINGS.keyframeInterval,
      1,
      600,
    ),
  };
}

function normalizeText(value) {
  if (typeof value !== 'string') return null;
  const trimmed = value.trim();
  return trimmed || null;
}

function normalizeNumber(value) {
  const numeric = Number(value);
  return Number.isFinite(numeric) ? numeric : null;
}

function normalizeResolution(value) {
  if (!value || typeof value !== 'object') return null;
  const width = clampInt(value.width, 0, 0, 16384);
  const height = clampInt(value.height, 0, 0, 16384);
  if (!width || !height) return null;
  return { width, height };
}

function normalizeStats(stats) {
  if (!stats || typeof stats !== 'object') return null;
  return {
    codec: normalizeText(stats.codec),
    resolution: normalizeResolution(stats.resolution),
    fps: normalizeNumber(stats.fps),
    bitrate: normalizeNumber(stats.bitrate),
    encoder: normalizeText(stats.encoder),
    tabTitle: normalizeText(stats.tabTitle),
  };
}

async function loadSettings() {
  try {
    const loaded = await chrome.storage.local.get(SETTINGS_STORAGE_KEY);
    state.settings = normalizeSettings(loaded[SETTINGS_STORAGE_KEY]);
  } catch (err) {
    console.warn('[BrowserPort] Failed to load settings:', err);
    state.settings = { ...DEFAULT_SETTINGS };
  }
}

const settingsReady = loadSettings();

function getBrowserPortAddress() {
  return state.settings.wsUrl || DEFAULT_SETTINGS.wsUrl;
}

function closeHealthSocket() {
  if (!healthSocket) return;
  try {
    healthSocket.onopen = null;
    healthSocket.onmessage = null;
    healthSocket.onclose = null;
    healthSocket.onerror = null;
    healthSocket.close();
  } catch {
    // ignore
  }
  healthSocket = null;
  healthHelloAcked = false;
  healthHelloSentAt = 0;
  healthLastPingAt = 0;
  healthLastPongAt = 0;
}

function handleHealthMessage(rawMessage) {
  if (typeof rawMessage !== 'string') return;
  let message = null;
  try {
    message = JSON.parse(rawMessage);
  } catch {
    return;
  }
  if (!message || typeof message !== 'object') return;
  if (message.type === 'hello-ack') {
    healthHelloAcked = true;
    healthLastPongAt = Date.now();
    state.browserPortConnected = true;
    return;
  }
  if (message.type === 'pong') {
    healthLastPongAt = Date.now();
    state.browserPortConnected = true;
  }
}

function openHealthSocket() {
  const socket = new WebSocket(getBrowserPortAddress());
  healthSocket = socket;
  healthHelloAcked = false;
  healthHelloSentAt = 0;
  healthLastPingAt = 0;
  healthLastPongAt = 0;

  socket.onopen = () => {
    if (healthSocket !== socket) return;
    try {
      socket.send(JSON.stringify({
        type: 'hello',
        role: 'client',
        protocolVersion: 1,
        capabilities: {
          source: 'browser-port-extension-healthcheck',
        },
      }));
      healthHelloSentAt = Date.now();
    } catch {
      state.browserPortConnected = false;
      closeHealthSocket();
    }
  };

  socket.onmessage = (event) => {
    if (healthSocket !== socket) return;
    handleHealthMessage(event.data);
  };

  socket.onerror = () => {
    if (healthSocket !== socket) return;
    state.browserPortConnected = false;
  };

  socket.onclose = () => {
    if (healthSocket !== socket) return;
    state.browserPortConnected = false;
    closeHealthSocket();
  };
}

function checkBrowserPortConnection() {
  try {
    const now = Date.now();
    if (healthSocket && healthSocket.readyState === WebSocket.OPEN) {
      if (!healthHelloAcked) {
        if (healthHelloSentAt > 0 && now - healthHelloSentAt > HEALTH_HANDSHAKE_TIMEOUT_MS) {
          state.browserPortConnected = false;
          closeHealthSocket();
        }
        return;
      }

      if (healthLastPongAt > 0 && now - healthLastPongAt > HEALTH_PONG_TIMEOUT_MS) {
        state.browserPortConnected = false;
        closeHealthSocket();
        return;
      }

      if (now - healthLastPingAt >= HEALTH_PING_INTERVAL_MS) {
        healthSocket.send(JSON.stringify({ type: 'ping' }));
        healthLastPingAt = now;
      }
      return;
    }

    if (healthSocket && healthSocket.readyState === WebSocket.CONNECTING) {
      return;
    }

    openHealthSocket();
  } catch {
    state.browserPortConnected = false;
    closeHealthSocket();
  }
}

setInterval(checkBrowserPortConnection, BROWSER_PORT_CHECK_INTERVAL_MS);
settingsReady.finally(() => {
  closeHealthSocket();
  checkBrowserPortConnection();
});

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  switch (message.type) {
    case 'GET_STATUS':
      settingsReady
        .catch(() => {})
        .finally(() => {
          sendResponse({
            assignments: state.assignments,
            browserPortConnected: state.browserPortConnected,
            settings: state.settings,
          });
        });
      return true;

    case 'ASSIGN_PLAYER':
      settingsReady
        .then(() => handleAssignPlayer(message.tabId, message.playerId))
        .then(result => sendResponse(result))
        .catch(err => sendResponse({ success: false, error: err.message }));
      return true;

    case 'UNASSIGN_PLAYER':
      handleUnassignPlayer(message.playerId)
        .then(result => sendResponse(result))
        .catch(err => sendResponse({ success: false, error: err.message }));
      return true;

    case 'SAVE_SETTINGS':
      settingsReady
        .then(() => handleSaveSettings(message.settings))
        .then(result => sendResponse(result))
        .catch(err => sendResponse({ success: false, error: err.message }));
      return true;

    case 'STREAM_STATUS':
      handleStreamStatus(message, sender);
      return false;

    default:
      return false;
  }
});

async function handleAssignPlayer(tabId, playerId) {
  for (const [pid, assignment] of Object.entries(state.assignments)) {
    if (assignment.tabId === tabId && Number(pid) !== playerId) {
      await handleUnassignPlayer(Number(pid));
    }
  }

  if (
    state.assignments[playerId].tabId !== null
    && state.assignments[playerId].tabId !== tabId
  ) {
    await handleUnassignPlayer(playerId);
  }

  try {
    await chrome.scripting.executeScript({
      target: { tabId },
      files: ['content.js'],
    });

    await chrome.tabs.sendMessage(tabId, {
      type: 'START_CAPTURE',
      playerId,
      settings: state.settings,
    });

    state.assignments[playerId] = {
      tabId,
      status: 'capturing',
      stats: null,
    };
    return { success: true };
  } catch (err) {
    console.error(`Failed to assign player ${playerId}:`, err);
    return { success: false, error: err.message };
  }
}

async function handleUnassignPlayer(playerId) {
  const assignment = state.assignments[playerId];
  if (!assignment || assignment.tabId === null) {
    state.assignments[playerId] = createIdleAssignment();
    return { success: true };
  }

  try {
    await chrome.tabs.sendMessage(assignment.tabId, { type: 'STOP_CAPTURE' });
  } catch (err) {
    console.warn(`Failed to send STOP_CAPTURE to player ${playerId}:`, err.message);
  }

  state.assignments[playerId] = createIdleAssignment();
  return { success: true };
}

async function handleSaveSettings(rawSettings) {
  const normalized = normalizeSettings(rawSettings);
  state.settings = normalized;

  await chrome.storage.local.set({
    [SETTINGS_STORAGE_KEY]: normalized,
  });

  closeHealthSocket();
  state.browserPortConnected = false;
  checkBrowserPortConnection();

  await Promise.all(
    Object.values(state.assignments)
      .filter(assignment => assignment.tabId !== null)
      .map(async (assignment) => {
        try {
          await chrome.tabs.sendMessage(assignment.tabId, {
            type: 'UPDATE_SETTINGS',
            settings: normalized,
          });
        } catch (err) {
          console.warn('[BrowserPort] Failed to push settings to tab:', assignment.tabId, err);
        }
      }),
  );

  return { success: true, settings: normalized };
}

function handleStreamStatus(message, sender) {
  const { playerId, status, error, stats } = message;
  if (!playerId || !state.assignments[playerId]) return;

  if (stats) {
    state.assignments[playerId].stats = normalizeStats(stats);
  }

  if (status === 'error' || status === 'ended') {
    console.log(`Player ${playerId} stream ${status}:`, error || '');
    state.assignments[playerId].status = status === 'error' ? 'error' : 'idle';
  } else if (status === 'capturing') {
    state.assignments[playerId].status = 'capturing';
  }
}

chrome.tabs.onRemoved.addListener((tabId) => {
  for (const [pid, assignment] of Object.entries(state.assignments)) {
    if (assignment.tabId === tabId) {
      state.assignments[pid] = createIdleAssignment();
      console.log(`Player ${pid}: tab ${tabId} closed, unassigned`);
    }
  }
});

chrome.tabs.onUpdated.addListener((tabId, changeInfo) => {
  if (changeInfo.status !== 'complete') return;
  const entry = Object.entries(state.assignments).find(
    ([, assignment]) => assignment.tabId === tabId,
  );
  if (!entry) return;

  const playerId = Number(entry[0]);
  chrome.scripting.executeScript({
    target: { tabId },
    files: ['content.js'],
  }).then(() => {
    chrome.tabs.sendMessage(tabId, {
      type: 'START_CAPTURE',
      playerId,
      settings: state.settings,
    });
  }).catch((err) => {
    console.warn(`Failed to re-inject content script for player ${playerId}:`, err);
  });
});
