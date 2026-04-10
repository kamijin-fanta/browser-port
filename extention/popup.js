// BrowserPort - Popup Script
// Multi-player assignment UI with compact status and overlay settings.

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

const browserPortStatusEl = document.getElementById('browser-port-status');
const browserPortDotEl = document.getElementById('browser-port-dot');
const browserPortTextEl = document.getElementById('browser-port-text');
const extensionVersionEl = document.getElementById('extension-version');
const deckListEl = document.getElementById('players');
const openSettingsEl = document.getElementById('open-settings');
const openLicensesEl = document.getElementById('open-licenses');
const closeSettingsEl = document.getElementById('close-settings');
const settingsOverlayEl = document.getElementById('settings-overlay');
const settingsFormEl = document.getElementById('settings-form');
const wsUrlEl = document.getElementById('ws-url');
const bitrateEl = document.getElementById('bitrate');
const targetFpsEl = document.getElementById('target-fps');
const maxWidthEl = document.getElementById('max-width');
const maxHeightEl = document.getElementById('max-height');
const latencyModeEl = document.getElementById('latency-mode');
const hardwareAccelerationEl = document.getElementById('hardware-acceleration');
const keyframeIntervalEl = document.getElementById('keyframe-interval');
const saveSettingsEl = document.getElementById('save-settings');
const settingsStatusEl = document.getElementById('settings-status');
const presetDefaultEl = document.getElementById('preset-default');
const presetButtons = Array.from(document.querySelectorAll('[data-preset]'));

let currentTab = null;
let assignments = {};
let browserPortConnected = false;
let currentSettings = { ...DEFAULT_SETTINGS };
let pollTimer = null;
let isSettingsOpen = false;
const deckCards = new Map();

(async () => {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  currentTab = tab;
  updateExtensionVersion();

  settingsFormEl.addEventListener('submit', onSettingsSubmit);
  settingsFormEl.addEventListener('input', () => {
    setSettingsStatus('');
  });
  openSettingsEl.addEventListener('click', openSettingsOverlay);
  openLicensesEl?.addEventListener('click', openLicensesPage);
  closeSettingsEl.addEventListener('click', closeSettingsOverlay);
  settingsOverlayEl.addEventListener('click', onOverlayClick);
  presetDefaultEl.addEventListener('click', applyDefaultPreset);
  presetButtons.forEach((button) => {
    button.addEventListener('click', onPresetButtonClick);
  });

  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && isSettingsOpen) {
      closeSettingsOverlay();
    }
  });

  await refreshState();
  pollTimer = setInterval(refreshState, 1000);
})();

function updateExtensionVersion() {
  if (!extensionVersionEl) return;
  const manifest = chrome.runtime.getManifest();
  const version = manifest.version_name || manifest.version || 'N/A';
  extensionVersionEl.textContent = `Extension v${version}`;
}

function openLicensesPage() {
  chrome.tabs.create({ url: chrome.runtime.getURL('licenses.html') });
}

function normalizeSettings(input) {
  const source = input && typeof input === 'object' ? input : {};
  return {
    wsUrl: typeof source.wsUrl === 'string' && source.wsUrl.trim()
      ? source.wsUrl.trim()
      : DEFAULT_SETTINGS.wsUrl,
    bitrate: toIntInRange(source.bitrate, DEFAULT_SETTINGS.bitrate, 100_000, 200_000_000),
    targetFps: toIntInRange(source.targetFps, DEFAULT_SETTINGS.targetFps, 1, 240),
    maxWidth: toIntInRange(source.maxWidth, DEFAULT_SETTINGS.maxWidth, 16, 7680),
    maxHeight: toIntInRange(source.maxHeight, DEFAULT_SETTINGS.maxHeight, 16, 4320),
    latencyMode: source.latencyMode === 'quality' ? 'quality' : 'realtime',
    hardwareAcceleration: normalizeHardwareAcceleration(source.hardwareAcceleration),
    keyframeInterval: toIntInRange(
      source.keyframeInterval,
      DEFAULT_SETTINGS.keyframeInterval,
      1,
      600,
    ),
  };
}

function normalizeHardwareAcceleration(value) {
  if (value === 'prefer-software') return 'prefer-software';
  if (value === 'no-preference') return 'no-preference';
  return 'prefer-hardware';
}

function toIntInRange(value, fallback, min, max) {
  const numeric = Number(value);
  if (!Number.isFinite(numeric)) return fallback;
  const integer = Math.floor(numeric);
  return Math.min(Math.max(integer, min), max);
}

function applySettingsToForm(settings) {
  wsUrlEl.value = settings.wsUrl;
  bitrateEl.value = String(settings.bitrate);
  targetFpsEl.value = String(settings.targetFps);
  maxWidthEl.value = String(settings.maxWidth);
  maxHeightEl.value = String(settings.maxHeight);
  latencyModeEl.value = settings.latencyMode;
  hardwareAccelerationEl.value = settings.hardwareAcceleration;
  keyframeIntervalEl.value = String(settings.keyframeInterval);
}

function updateBrowserPortStatus(connected) {
  browserPortTextEl.textContent = connected ? 'Agent 接続中' : 'Agent 未接続';
  browserPortStatusEl.className = `bp-connection ${connected ? 'is-connected' : 'is-disconnected'}`;
  browserPortDotEl.className = `bp-connection__dot ${connected ? 'is-connected' : 'is-disconnected'}`;
}

function createDeckCard(playerId) {
  const card = document.createElement('article');
  card.className = 'bp-deck-card';

  const header = document.createElement('button');
  header.type = 'button';
  header.className = 'bp-deck-card__header';

  const label = document.createElement('span');
  label.className = 'bp-deck-card__label';
  label.textContent = `プレイヤー ${playerId}`;

  const status = document.createElement('span');
  status.className = 'bp-deck-card__status';

  const dot = document.createElement('span');
  dot.className = 'bp-status-dot is-idle';

  const statusText = document.createElement('span');
  statusText.className = 'bp-deck-card__status-text';
  statusText.textContent = '待機中';

  status.appendChild(dot);
  status.appendChild(statusText);

  const details = document.createElement('div');
  details.className = 'bp-deck-card__details';

  const summary = document.createElement('div');
  summary.className = 'bp-deck-card__summary';
  summary.textContent = 'N/A';

  const title = document.createElement('div');
  title.className = 'bp-deck-card__title';
  title.textContent = 'N/A';

  details.appendChild(summary);
  details.appendChild(title);

  header.appendChild(label);
  header.appendChild(status);
  card.appendChild(header);
  card.appendChild(details);

  header.addEventListener('click', () => {
    const assignment = assignments[playerId] || { tabId: null, status: 'idle', stats: null };
    const isActive = assignment.tabId !== null;
    const isCurrentTab = assignment.tabId === currentTab?.id;
    onPlayerClick(playerId, isActive, isCurrentTab);
  });

  return {
    card,
    status,
    dot,
    statusText,
    summary,
    title,
  };
}

function updateDeckCard(playerId, assignment) {
  const deck = deckCards.get(playerId) || createDeckCard(playerId);
  if (!deckCards.has(playerId)) {
    deckCards.set(playerId, deck);
    deckListEl.appendChild(deck.card);
  }

  const stats = assignment.stats || {};
  const isActive = assignment.tabId !== null;
  const isCurrentTab = assignment.tabId === currentTab?.id;
  const statusInfo = getPlayerStatusLabel(assignment.status, isActive, isCurrentTab);

  deck.card.classList.toggle('is-active', isActive);
  deck.status.className = `bp-deck-card__status ${statusInfo.statusClass}`.trim();
  deck.dot.className = `bp-status-dot ${statusInfo.dotClass}`;
  deck.statusText.textContent = statusInfo.text;

  deck.summary.textContent = [
    stats.codec || 'N/A',
    formatResolution(stats.resolution),
    formatFps(stats.fps, currentSettings.targetFps),
    formatBitrate(stats.bitrate, currentSettings.bitrate),
  ].join(' ・ ');
  deck.title.textContent = stats.tabTitle || 'N/A';
}

function renderPlayers() {
  for (let pid = 1; pid <= 4; pid += 1) {
    const assignment = assignments[pid] || {
      tabId: null,
      status: 'idle',
      stats: null,
    };
    updateDeckCard(pid, assignment);
  }
}

function getPlayerStatusLabel(status, isActive, isCurrentTab) {
  if (!isActive) {
    return { dotClass: 'is-idle', statusClass: '', text: '待機中' };
  }
  if (status === 'error') {
    return { dotClass: 'is-error', statusClass: 'is-error', text: 'エラー' };
  }
  if (status === 'idle') {
    return { dotClass: 'is-idle', statusClass: '', text: '待機中' };
  }
  return {
    dotClass: 'is-capturing',
    statusClass: 'is-capturing',
    text: isCurrentTab ? 'このタブ' : '他のタブで配信中',
  };
}

function formatResolution(resolution) {
  if (!resolution || typeof resolution !== 'object') return 'N/A';
  const width = Number(resolution.width);
  const height = Number(resolution.height);
  if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) {
    return 'N/A';
  }
  return `${Math.round(width)}x${Math.round(height)}`;
}

function formatFps(actualFps, fallbackFps) {
  const fps = Number.isFinite(Number(actualFps)) ? Number(actualFps) : Number(fallbackFps);
  if (!Number.isFinite(fps) || fps <= 0) return 'N/A';
  return `${fps.toFixed(1)} fps`;
}

function formatBitrate(actualBitrate, fallbackBitrate) {
  const bps = Number.isFinite(Number(actualBitrate)) ? Number(actualBitrate) : Number(fallbackBitrate);
  if (!Number.isFinite(bps) || bps <= 0) return 'N/A';
  if (bps >= 1_000_000) {
    return `${(bps / 1_000_000).toFixed(2)} Mbps`;
  }
  if (bps >= 1_000) {
    return `${(bps / 1_000).toFixed(1)} kbps`;
  }
  return `${Math.round(bps)} bps`;
}

function refreshState() {
  return new Promise((resolve) => {
    chrome.runtime.sendMessage({ type: 'GET_STATUS' }, (response) => {
      if (response) {
        assignments = response.assignments || {};
        browserPortConnected = response.browserPortConnected || false;
        currentSettings = normalizeSettings(response.settings || currentSettings);
        if (!isSettingsOpen) {
          applySettingsToForm(currentSettings);
        }
      }
      updateBrowserPortStatus(browserPortConnected);
      renderPlayers();
      resolve();
    });
  });
}

function collectSettingsFromForm() {
  const wsUrl = wsUrlEl.value.trim();
  if (!wsUrl) {
    throw new Error('WS Address を入力してください');
  }

  const settings = normalizeSettings({
    wsUrl,
    bitrate: bitrateEl.value,
    targetFps: targetFpsEl.value,
    maxWidth: maxWidthEl.value,
    maxHeight: maxHeightEl.value,
    latencyMode: latencyModeEl.value,
    hardwareAcceleration: hardwareAccelerationEl.value,
    keyframeInterval: keyframeIntervalEl.value,
  });

  if (settings.wsUrl !== wsUrl) {
    throw new Error('WS Address が不正です');
  }

  return settings;
}

function setSettingsStatus(message, kind = '') {
  settingsStatusEl.textContent = message;
  settingsStatusEl.className = `bp-form-status ${kind ? `is-${kind}` : ''}`.trim();
}

function openSettingsOverlay() {
  isSettingsOpen = true;
  applySettingsToForm(currentSettings);
  setSettingsStatus('');
  settingsOverlayEl.classList.add('is-open');
  settingsOverlayEl.setAttribute('aria-hidden', 'false');
}

function closeSettingsOverlay() {
  isSettingsOpen = false;
  settingsOverlayEl.classList.remove('is-open');
  settingsOverlayEl.setAttribute('aria-hidden', 'true');
}

function onOverlayClick(event) {
  if (event.target === settingsOverlayEl) {
    closeSettingsOverlay();
  }
}

function applyDefaultPreset() {
  applySettingsToForm(DEFAULT_SETTINGS);
  setSettingsStatus('デフォルト値を反映しました（未保存）');
}

function onPresetButtonClick(event) {
  const button = event.currentTarget;
  const preset = button.dataset.preset;
  const value = button.dataset.value;
  const label = (button.textContent || value || '').trim();

  if (preset === 'bitrate' && value) {
    bitrateEl.value = String(Number(value));
    setSettingsStatus(`Bitrateを ${label} に設定（未保存）`);
    return;
  }

  if (preset === 'fps' && value) {
    targetFpsEl.value = String(Number(value));
    setSettingsStatus(`FPSを ${label} に設定（未保存）`);
    return;
  }

  if (preset === 'resolution' && value && value.includes('x')) {
    const [width, height] = value.split('x').map(Number);
    if (Number.isFinite(width) && Number.isFinite(height)) {
      maxWidthEl.value = String(width);
      maxHeightEl.value = String(height);
      setSettingsStatus(`Resolutionを ${value} に設定（未保存）`);
    }
  }
}

async function onSettingsSubmit(event) {
  event.preventDefault();

  let payload;
  try {
    payload = collectSettingsFromForm();
  } catch (err) {
    setSettingsStatus(err.message || '設定値を確認してください', 'error');
    return;
  }

  saveSettingsEl.disabled = true;
  setSettingsStatus('保存中...');

  chrome.runtime.sendMessage({ type: 'SAVE_SETTINGS', settings: payload }, async (response) => {
    saveSettingsEl.disabled = false;
    if (!response?.success) {
      setSettingsStatus(response?.error || '設定保存に失敗しました', 'error');
      return;
    }

    currentSettings = normalizeSettings(response.settings || payload);
    applySettingsToForm(currentSettings);
    setSettingsStatus('設定を保存しました', 'success');
    await refreshState();
  });
}

async function onPlayerClick(playerId, isActive, isCurrentTab) {
  if (isCurrentTab) {
    chrome.runtime.sendMessage(
      { type: 'UNASSIGN_PLAYER', playerId },
      (response) => {
        if (response?.success) refreshState();
      },
    );
    return;
  }

  if (!currentTab) return;

  if (isActive) {
    if (!confirm(`プレイヤー ${playerId} は別のタブで配信中です。\nこのタブに切り替えますか？`)) {
      return;
    }
  }

  chrome.runtime.sendMessage(
    { type: 'ASSIGN_PLAYER', tabId: currentTab.id, playerId },
    (response) => {
      if (response?.success) {
        refreshState();
      } else {
        alert(response?.error || '割り当てに失敗しました');
      }
    },
  );
}
