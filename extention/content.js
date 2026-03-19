// BrowserPort - Content Script (content.js)
// WebCodecs H.264 encoding + WebSocket transport.

if (!window.__BrowserPort_listener) {
  window.__BrowserPort_listener = true;

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

  const CONFIG = {
    WS_URL: DEFAULT_SETTINGS.wsUrl,
  };

  const ENCODE = {
    targetFps: DEFAULT_SETTINGS.targetFps,
    bitrate: DEFAULT_SETTINGS.bitrate,
    keyframeInterval: DEFAULT_SETTINGS.keyframeInterval,
    hardwareAcceleration: DEFAULT_SETTINGS.hardwareAcceleration,
    latencyMode: DEFAULT_SETTINGS.latencyMode,
    maxWidth: DEFAULT_SETTINGS.maxWidth,
    maxHeight: DEFAULT_SETTINGS.maxHeight,
  };

  const PLAYBACK_REPORT_INTERVAL_MS = 250;
  const STATUS_REPORT_INTERVAL_MS = 1000;
  const RECONNECT_INTERVAL_MS = 1000;
  const CONNECT_TIMEOUT_MS = 5000;

  const CHUNK_HEADER_SIZE = 20;
  const CHUNK_TYPE_VIDEO = 1;
  const CHUNK_TYPE_AUDIO = 2;
  const CHUNK_VERSION = 1;
  const CHUNK_FLAG_KEYFRAME = 0x01;
  const AUDIO_META_SIZE = 8;
  const AUDIO_BUFFER_SIZE = 2048;
  const PENDING_SEARCH_KEY = 'browserPort.pendingSearch';

  let videoElement = null;
  let mediaStream = null;
  let videoTrack = null;
  let audioTrack = null;
  let encoder = null;
  let webSocket = null;
  let currentPlayerId = null;
  let frameCallbackHandle = null;
  let trackProcessor = null;
  let trackReader = null;
  let processorRunning = false;
  let lastEncodedTimestampUs = null;
  let frameCounter = 0;
  let lastDecoderConfigSig = null;
  let pendingDecoderConfig = null;
  let activeEncoderConfig = null;
  let statusTimer = null;
  let playbackTimer = null;
  let lastPlaybackPayload = null;
  let playbackListenersAttached = false;
  let reconnectTimer = null;
  let connectTimeoutTimer = null;
  let captureActive = false;
  let forceNextKeyframe = false;
  let suppressReconnect = false;
  let audioContext = null;
  let audioCaptureSourceNode = null;
  let audioSourceStream = null;
  let audioProcessorNode = null;
  let audioCaptureSinkNode = null;
  let audioTrackProcessor = null;
  let audioTrackReader = null;
  let audioTrackProcessorRunning = false;
  let audioCaptureMode = null;
  let audioFallbackTimer = null;
  let lastAudioChunkWallMs = null;
  let audioElementNode = null;
  let audioElementOutputNode = null;
  let audioElementOutputConnected = false;
  let audioElementOutputElement = null;
  const audioElementNodeCache = new WeakMap();
  let captureStreamSupported = false;
  let lastVideoSrc = null;
  let measuredFps = null;
  let measuredBitrate = null;
  let metricsWindowStartMs = null;
  let metricsWindowFrames = 0;
  let metricsWindowBytes = 0;
  let currentCodec = null;
  let currentResolution = null;

  // Listen for messages from background
  chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
    switch (message.type) {
      case 'START_CAPTURE':
        captureActive = true;
        applyRuntimeSettings(message.settings);
        cleanup();
        currentPlayerId = message.playerId;
        connectWebSocket();
        startCapture();
        sendResponse({ ok: true });
        return false;
      case 'UPDATE_SETTINGS':
        applyRuntimeSettings(message.settings);
        if (captureActive && currentPlayerId !== null) {
          cleanup(true, false);
          connectWebSocket();
          startCapture();
        }
        sendResponse({ ok: true });
        return false;
      case 'STOP_CAPTURE':
        captureActive = false;
        cleanup();
        sendResponse({ ok: true });
        return false;
    }
  });

  // --- Auto-recapture: MutationObserver ---
  let recaptureTimer = null;
  function scheduleRecapture() {
    if (!currentPlayerId) return;
    if (recaptureTimer) clearTimeout(recaptureTimer);
    recaptureTimer = setTimeout(() => {
      console.log('[BrowserPort] Recapturing after video change');
      cleanup(true, true); // keepPlayerId = true, keepWebSocket = true
      startCapture();
    }, 500);
  }

  const observer = new MutationObserver((mutations) => {
    for (const mutation of mutations) {
      for (const node of mutation.addedNodes) {
        if (node.nodeName === 'VIDEO' || node.querySelector?.('video')) {
          scheduleRecapture();
        }
      }
    }
  });
  observer.observe(document.body, { childList: true, subtree: true });

  // --- YouTube SPA navigation ---
  document.addEventListener('yt-navigate-finish', () => {
    scheduleRecapture();
    consumePendingSearchRequest();
  });

  function toIntInRange(value, fallback, min, max) {
    const numeric = Number(value);
    if (!Number.isFinite(numeric)) return fallback;
    const floored = Math.floor(numeric);
    return Math.min(Math.max(floored, min), max);
  }

  function normalizeRuntimeSettings(input) {
    const source = input && typeof input === 'object' ? input : {};
    const wsUrl = typeof source.wsUrl === 'string' ? source.wsUrl.trim() : '';

    return {
      wsUrl: wsUrl || DEFAULT_SETTINGS.wsUrl,
      bitrate: toIntInRange(source.bitrate, DEFAULT_SETTINGS.bitrate, 100_000, 200_000_000),
      targetFps: toIntInRange(source.targetFps, DEFAULT_SETTINGS.targetFps, 1, 240),
      maxWidth: toIntInRange(source.maxWidth, DEFAULT_SETTINGS.maxWidth, 16, 7680),
      maxHeight: toIntInRange(source.maxHeight, DEFAULT_SETTINGS.maxHeight, 16, 4320),
      latencyMode: source.latencyMode === 'quality' ? 'quality' : 'realtime',
      hardwareAcceleration: ['prefer-hardware', 'prefer-software', 'no-preference']
        .includes(source.hardwareAcceleration)
        ? source.hardwareAcceleration
        : DEFAULT_SETTINGS.hardwareAcceleration,
      keyframeInterval: toIntInRange(
        source.keyframeInterval,
        DEFAULT_SETTINGS.keyframeInterval,
        1,
        600,
      ),
    };
  }

  function applyRuntimeSettings(input) {
    const settings = normalizeRuntimeSettings(input);
    CONFIG.WS_URL = settings.wsUrl;
    ENCODE.targetFps = settings.targetFps;
    ENCODE.bitrate = settings.bitrate;
    ENCODE.keyframeInterval = settings.keyframeInterval;
    ENCODE.hardwareAcceleration = settings.hardwareAcceleration;
    ENCODE.latencyMode = settings.latencyMode;
    ENCODE.maxWidth = settings.maxWidth;
    ENCODE.maxHeight = settings.maxHeight;
    return settings;
  }
  window.addEventListener('popstate', () => {
    scheduleRecapture();
    consumePendingSearchRequest();
  });

  function findVideo() {
    return document.querySelector('video.html5-main-video')
      || document.querySelector('#movie_player video')
      || document.querySelector('video');
  }

  function getVideoSrc(video) {
    if (!video) return '';
    return video.currentSrc || video.src || '';
  }

  function getAudioContext() {
    const AudioContextCtor = window.AudioContext || window.webkitAudioContext;
    if (!AudioContextCtor) return null;
    if (!audioContext) {
      audioContext = new AudioContextCtor();
    }
    return audioContext;
  }

  function ensureAudioContextRunning() {
    if (!audioContext) return;
    if (audioContext.state === 'suspended') {
      audioContext.resume().catch(() => { });
    }
  }

  function attachElementAudioOutput(element) {
    if (!element) return null;
    const ctx = getAudioContext();
    if (!ctx) return null;

    let elementNode = audioElementNodeCache.get(element);
    if (!elementNode) {
      try {
        elementNode = ctx.createMediaElementSource(element);
        audioElementNodeCache.set(element, elementNode);
      } catch (err) {
        console.warn('[BrowserPort] Failed to bind element audio source:', err);
        return null;
      }
    }

    if (audioElementOutputElement !== element && audioElementOutputConnected) {
      try {
        if (audioElementNode && audioElementOutputNode) {
          audioElementNode.disconnect(audioElementOutputNode);
        }
        audioElementOutputNode.disconnect();
      } catch {
        // ignore
      }
      audioElementOutputConnected = false;
    }

    audioElementNode = elementNode;
    audioElementOutputElement = element;

    if (!audioElementOutputNode) {
      audioElementOutputNode = ctx.createGain();
    }

    if (!audioElementOutputConnected) {
      try {
        audioElementNode.connect(audioElementOutputNode);
        audioElementOutputNode.connect(ctx.destination);
        audioElementOutputConnected = true;
      } catch (err) {
        console.warn('[BrowserPort] Failed to connect element audio output:', err);
      }
    }

    ensureAudioContextRunning();
    return audioElementNode;
  }

  function ensureCaptureForPlayback(reason) {
    if (!captureActive || !currentPlayerId) return;
    if (!videoElement) {
      scheduleRecapture();
      return;
    }

    ensureAudioContextRunning();
    if (!webSocket || webSocket.readyState !== WebSocket.OPEN) {
      connectWebSocket();
    }

    const trackEnded = videoTrack && videoTrack.readyState === 'ended';
    const streamTracks = mediaStream ? mediaStream.getVideoTracks() : [];
    const hasStreamTrack = streamTracks && streamTracks.length > 0;
    const needsStreamRecapture = captureStreamSupported && (!hasStreamTrack || trackEnded);

    if (needsStreamRecapture) {
      console.log('[BrowserPort] Recapturing for playback', reason);
      cleanup(true, true);
      startCapture();
      return;
    }

    if (!encoder || encoder.state === 'closed') {
      maybeStartEncoding();
      return;
    }

    if (videoTrack && 'MediaStreamTrackProcessor' in window) {
      if (!processorRunning) {
        startTrackProcessor();
      }
    } else if (frameCallbackHandle === null) {
      scheduleVideoFrame();
    }
  }

  function startCapture() {
    try {
      const video = findVideo();
      if (!video) {
        console.log('[BrowserPort] Waiting for video element');
        scheduleRecapture();
        return;
      }
      if (video.readyState < 2) {
        // Wait for video to be ready
        video.addEventListener('loadeddata', () => startCapture(), { once: true });
        return;
      }

      if (videoElement !== video) {
        detachVideoListeners();
        stopPlaybackReporter();
      }
      videoElement = video;
      captureStreamSupported = typeof video.captureStream === 'function';
      lastVideoSrc = getVideoSrc(video);
      audioTrack = null;
      attachVideoListeners(videoElement);
      startPlaybackReporter();
      startStatusReporter();
      if (captureStreamSupported) {
        mediaStream = video.captureStream?.(ENCODE.targetFps) || video.captureStream();
        const tracks = mediaStream.getVideoTracks();
        const audioTracks = mediaStream.getAudioTracks();
        audioTrack = audioTracks && audioTracks.length ? audioTracks[0] : null;
        if (!tracks.length) {
          throw new Error('キャプチャしたストリームに映像トラックがありません');
        }
        videoTrack = tracks[0];
        videoTrack.onended = () => {
          console.log('[BrowserPort] Video track ended');
          reportStatus('ended');
          scheduleRecapture();
        };
        if (videoTrack.applyConstraints) {
          videoTrack.applyConstraints({ frameRate: ENCODE.targetFps }).catch(() => { });
        }
      } else {
        console.warn('[BrowserPort] captureStream is not supported; falling back to DOM frames');
      }
      startAudioCapture();
      console.log('[BrowserPort] Video element ready', {
        playerId: currentPlayerId,
        videoWidth: video.videoWidth,
        videoHeight: video.videoHeight,
      });

      connectWebSocket();
      maybeStartEncoding();
    } catch (err) {
      console.error('[BrowserPort] Failed to capture:', err);
      reportStatus('error', err.message);
    }
  }

  function connectWebSocket() {
    if (!shouldReconnect()) {
      return;
    }
    if (webSocket && (webSocket.readyState === WebSocket.OPEN
      || webSocket.readyState === WebSocket.CONNECTING)) {
      return;
    }
    suppressReconnect = false;
    webSocket = new WebSocket(CONFIG.WS_URL);
    webSocket.binaryType = 'arraybuffer';

    if (connectTimeoutTimer) clearTimeout(connectTimeoutTimer);
    connectTimeoutTimer = setTimeout(() => {
      if (webSocket && webSocket.readyState === WebSocket.CONNECTING) {
        console.warn('[BrowserPort] WebSocket connection still pending');
      }
    }, CONNECT_TIMEOUT_MS);

    webSocket.onopen = () => {
      if (connectTimeoutTimer) {
        clearTimeout(connectTimeoutTimer);
        connectTimeoutTimer = null;
      }
      console.log('[BrowserPort] WebSocket connected, registering player', currentPlayerId);
      webSocket.send(JSON.stringify({
        type: 'hello',
        role: 'browser-port-extension',
        protocolVersion: 1,
        capabilities: {
          source: 'browser-port-extension',
        },
      }));
      webSocket.send(JSON.stringify({
        type: 'register',
        playerId: currentPlayerId,
      }));
      stopReconnectTimer();
      forceNextKeyframe = true;
      maybeStartEncoding();
    };

    webSocket.onmessage = (event) => {
      const message = safeJsonParse(event.data);
      if (!message) return;
      if (message.type === 'tier-info') {
        console.log('[BrowserPort] BrowserPort info:', message);
      } else if (message.type === 'hello-ack') {
        console.log('[BrowserPort] BrowserPort hello acknowledged:', message);
      } else if (message.type === 'search') {
        handleSearch(message);
      } else if (message.type === 'control') {
        handleControl(message);
      }
    };

    webSocket.onclose = () => {
      console.log('[BrowserPort] WebSocket closed');
      if (connectTimeoutTimer) {
        clearTimeout(connectTimeoutTimer);
        connectTimeoutTimer = null;
      }
      if (!suppressReconnect) {
        reportStatus('error', 'レシーバーとの接続が切れました。再接続を試みます。');
        scheduleReconnect();
      }
    };

    webSocket.onerror = (event) => {
      console.warn('[BrowserPort] WebSocket error', event);
    };

    webSocket.onerror = (err) => {
      console.error('[BrowserPort] WebSocket error:', err);
      if (connectTimeoutTimer) {
        clearTimeout(connectTimeoutTimer);
        connectTimeoutTimer = null;
      }
      if (!suppressReconnect) {
        reportStatus('error', 'レシーバーに接続できません。起動しているか確認してください。');
        scheduleReconnect();
      }
    };
  }

  function maybeStartEncoding() {
    if (!videoElement || !webSocket || webSocket.readyState !== WebSocket.OPEN) return;
    startAudioCapture();
    if (encoder && encoder.state !== 'closed') {
      sendDecoderConfig();
      forceNextKeyframe = true;
      reportStatus('capturing');
      return;
    }
    startEncoding().catch((err) => {
      console.error('[BrowserPort] Failed to start encoder:', err);
      reportStatus('error', err.message);
    });
  }

  function handleSearch(message) {
    const query = typeof message.query === 'string' ? message.query.trim() : '';
    if (!query) return;
    const autoSelectTop = !!message.autoSelectTop;
    const maxResults = Number.isFinite(Number(message.maxResults))
      ? Math.max(1, Math.min(10, Number(message.maxResults)))
      : 5;
    const pending = {
      query,
      autoSelectTop,
      maxResults,
      requestedAt: Date.now(),
    };
    try {
      sessionStorage.setItem(PENDING_SEARCH_KEY, JSON.stringify(pending));
    } catch {
      // ignore
    }
    const url = `https://www.youtube.com/results?search_query=${encodeURIComponent(query)}`;
    if (window.location.href !== url) {
      window.location.assign(url);
      return;
    }
    processSearchRequest(pending);
  }

  function consumePendingSearchRequest() {
    let raw = null;
    try {
      raw = sessionStorage.getItem(PENDING_SEARCH_KEY);
    } catch {
      raw = null;
    }
    if (!raw) return;
    let pending = null;
    try {
      pending = JSON.parse(raw);
    } catch {
      pending = null;
    }
    if (!pending || typeof pending.query !== 'string' || !pending.query.trim()) {
      try {
        sessionStorage.removeItem(PENDING_SEARCH_KEY);
      } catch {
        // ignore
      }
      return;
    }
    processSearchRequest(pending);
  }

  function processSearchRequest(pending) {
    const deadline = Date.now() + 20_000;
    const waitAndCollect = () => {
      const results = collectSearchResults(pending.maxResults);
      if (results.length === 0 && Date.now() < deadline) {
        setTimeout(waitAndCollect, 350);
        return;
      }
      sendSearchResults(pending.query, results);
      if (pending.autoSelectTop && results.length > 0) {
        selectTopResult(results[0], pending.query);
      }
      try {
        sessionStorage.removeItem(PENDING_SEARCH_KEY);
      } catch {
        // ignore
      }
    };
    waitAndCollect();
  }

  function collectSearchResults(maxResults) {
    const nodes = Array.from(document.querySelectorAll('ytd-video-renderer a#video-title[href]'));
    const results = [];
    for (const node of nodes) {
      const href = typeof node.getAttribute === 'function' ? node.getAttribute('href') : '';
      if (!href || !href.startsWith('/watch')) continue;
      const title = (node.textContent || '').trim();
      const url = `https://www.youtube.com${href}`;
      results.push({ title, url });
      if (results.length >= maxResults) break;
    }
    return results;
  }

  function selectTopResult(topResult, query) {
    if (!topResult || typeof topResult.url !== 'string') return;
    sendJsonMessage({
      type: 'search-selected',
      playerId: currentPlayerId,
      query,
      selected: topResult,
    });
    window.location.assign(topResult.url);
  }

  function sendSearchResults(query, results) {
    sendJsonMessage({
      type: 'search-results',
      playerId: currentPlayerId,
      query,
      results,
    });
  }

  function handleControl(message) {
    if (!videoElement) {
      scheduleRecapture();
      return;
    }
    const action = message.action;
    if (action === 'play') {
      videoElement.play().catch(() => { });
      reportPlayback(true);
      return;
    }
    if (action === 'pause') {
      videoElement.pause();
      reportPlayback(true);
      return;
    }
    if (action === 'toggle') {
      if (videoElement.paused) {
        videoElement.play().catch(() => { });
      } else {
        videoElement.pause();
      }
      reportPlayback(true);
      return;
    }
    if (action === 'seek') {
      const time = Number(message.time);
      if (!Number.isFinite(time)) return;
      const duration = Number.isFinite(videoElement.duration) ? videoElement.duration : null;
      if (duration !== null) {
        videoElement.currentTime = Math.min(Math.max(time, 0), duration);
      } else {
        videoElement.currentTime = Math.max(time, 0);
      }
      reportPlayback(true);
      return;
    }
    if (action === 'sync') {
      reportPlayback(true);
    }
  }

  async function startEncoding() {
    if (!videoElement) return;
    if (!('VideoEncoder' in window)) {
      throw new Error('このブラウザはWebCodecsに対応していません');
    }

    encoder = new VideoEncoder({
      output: handleEncodedChunk,
      error: (err) => {
        console.error('[BrowserPort] Encoder error:', err);
        reportStatus('error', 'エンコーダーでエラーが発生しました');
      },
    });

    const config = await selectEncoderConfig(videoElement);
    encoder.configure(config);
    activeEncoderConfig = config;
    currentCodec = typeof config.codec === 'string' ? config.codec : null;
    currentResolution = Number.isFinite(config.width) && Number.isFinite(config.height)
      ? { width: config.width, height: config.height }
      : null;
    resetMetricsWindow();

    lastEncodedTimestampUs = null;
    frameCounter = 0;
    lastDecoderConfigSig = null;
    pendingDecoderConfig = {
      codec: config.codec,
      codedWidth: config.width,
      codedHeight: config.height,
      description: null,
    };
    sendDecoderConfig();

    reportStatus('capturing');
    if (videoTrack && 'MediaStreamTrackProcessor' in window) {
      startTrackProcessor();
    } else {
      scheduleVideoFrame();
    }
  }

  function clampVideoSize(videoWidth, videoHeight, maxWidth, maxHeight) {
    const rawWidth = Number.isFinite(videoWidth) && videoWidth > 0 ? videoWidth : 16;
    const rawHeight = Number.isFinite(videoHeight) && videoHeight > 0 ? videoHeight : 16;

    const safeMaxWidth = Number.isFinite(maxWidth) && maxWidth > 0 ? maxWidth : rawWidth;
    const safeMaxHeight = Number.isFinite(maxHeight) && maxHeight > 0 ? maxHeight : rawHeight;

    const widthRatio = safeMaxWidth / rawWidth;
    const heightRatio = safeMaxHeight / rawHeight;
    const ratio = Math.min(widthRatio, heightRatio, 1);

    let width = Math.max(16, Math.round(rawWidth * ratio));
    let height = Math.max(16, Math.round(rawHeight * ratio));

    if (width % 2 !== 0) width -= 1;
    if (height % 2 !== 0) height -= 1;

    width = Math.max(16, width);
    height = Math.max(16, height);
    return { width, height };
  }

  async function selectEncoderConfig(video) {
    const { width, height } = clampVideoSize(
      video.videoWidth || 0,
      video.videoHeight || 0,
      ENCODE.maxWidth,
      ENCODE.maxHeight,
    );
    const baseConfigs = [
      {
        width,
        height,
        bitrate: ENCODE.bitrate,
        framerate: ENCODE.targetFps,
        hardwareAcceleration: ENCODE.hardwareAcceleration,
        latencyMode: ENCODE.latencyMode,
      },
      // {
      //   width,
      //   height,
      //   bitrate: ENCODE.bitrate,
      //   framerate: ENCODE.targetFps,
      //   hardwareAcceleration: 'no-preference',
      //   latencyMode: ENCODE.latencyMode,
      // },
      // {
      //   width,
      //   height,
      //   bitrate: ENCODE.bitrate,
      //   framerate: ENCODE.targetFps,
      //   hardwareAcceleration: 'no-preference',
      //   latencyMode: 'quality',
      // },
      // {
      //   width,
      //   height,
      //   bitrate: 5_000_000,
      //   framerate: 30,
      // },
      // {
      //   width,
      //   height,
      // },
    ];

    const h264Candidates = [
      // "avc1.64002A", // High 4.2
      // "avc1.4D002A", // Main 4.2
      "avc1.42002A", // Baseline 4.2
      "avc1.42E02A", // Constrained Baseline 4.2
      // "avc1.640028", // High 4.0
      // "avc1.4D0028", // Main 4.0
      "avc1.420028", // Baseline 4.0
      "avc1.42E028", // Constrained Baseline 4.0
      // "avc1.64001F", // High 3.1
      // "avc1.4D001F", // Main 3.1
      // "avc1.42001F", // Baseline 3.1
      // "avc1.42E01F", // Constrained Baseline 3.1
      // "avc1.4D001E", // Main 3.0
      // "avc1.42001E", // Baseline 3.0
      // "avc1.42E01E", // Constrained Baseline 3.0
    ];

    const failures = [];

    const h264Variants = [
      { avc: { format: 'annexb' } },
      { avc: { format: 'avc' } },
      {},
    ];

    const candidateGroups = [
      { codecs: h264Candidates, variants: h264Variants },
    ];

    for (const baseConfig of baseConfigs) {
      for (const group of candidateGroups) {
        for (const codec of group.codecs) {
          for (const variant of group.variants) {
            const candidate = { ...baseConfig, codec, ...variant };
            try {
              const support = await VideoEncoder.isConfigSupported(candidate);
              if (support.supported) {
                console.log('[BrowserPort] Encoder config selected:', support.config);
                return support.config;
              }
              failures.push({
                codec,
                variant,
                reason: 'not supported',
                reported: support,
              });
            } catch (err) {
              console.warn('[BrowserPort] Encoder config rejected:', candidate, err);
              failures.push({
                codec,
                variant,
                reason: err?.message || String(err),
              });
            }
          }
        }
      }
    }

    console.error('[BrowserPort] H.264 encoder not available. Details:', failures);
    const detail = JSON.stringify(failures);
    throw new Error(`H.264エンコーダーが利用できません: ${detail}`);
  }

  function handleEncodedChunk(chunk, metadata) {
    if (!webSocket || webSocket.readyState !== WebSocket.OPEN) return;

    if (metadata && metadata.decoderConfig) {
      const config = metadata.decoderConfig;
      currentCodec = typeof config.codec === 'string' ? config.codec : currentCodec;
      if (Number.isFinite(config.codedWidth) && Number.isFinite(config.codedHeight)) {
        currentResolution = {
          width: config.codedWidth,
          height: config.codedHeight,
        };
      }
      const signature = `${config.codec}|${config.codedWidth}x${config.codedHeight}|` +
        `${config.description ? config.description.byteLength : 0}`;
      if (signature !== lastDecoderConfigSig) {
        lastDecoderConfigSig = signature;
        pendingDecoderConfig = config;
        sendDecoderConfig();
      }
    }

    sendEncodedChunk(chunk);
  }

  function sendDecoderConfig() {
    if (!pendingDecoderConfig || !webSocket || webSocket.readyState !== WebSocket.OPEN) {
      return;
    }

    const description = pendingDecoderConfig.description
      ? bufferToBase64(pendingDecoderConfig.description)
      : null;

    webSocket.send(JSON.stringify({
      type: 'config',
      playerId: currentPlayerId,
      codec: pendingDecoderConfig.codec,
      codedWidth: pendingDecoderConfig.codedWidth,
      codedHeight: pendingDecoderConfig.codedHeight,
      description,
    }));
  }

  function sendEncodedChunk(chunk) {
    if (!webSocket || webSocket.readyState !== WebSocket.OPEN) return;

    const data = new Uint8Array(chunk.byteLength);
    chunk.copyTo(data);

    const header = new ArrayBuffer(CHUNK_HEADER_SIZE);
    const view = new DataView(header);
    view.setUint8(0, CHUNK_TYPE_VIDEO);
    view.setUint8(1, CHUNK_VERSION);
    view.setUint8(2, chunk.type === 'key' ? CHUNK_FLAG_KEYFRAME : 0);
    view.setUint8(3, 0);
    view.setUint32(4, currentPlayerId ?? 0, true);
    view.setBigUint64(8, BigInt(chunk.timestamp ?? 0), true);
    view.setUint32(16, data.byteLength, true);

    const message = new Uint8Array(CHUNK_HEADER_SIZE + data.byteLength);
    message.set(new Uint8Array(header), 0);
    message.set(data, CHUNK_HEADER_SIZE);
    markVideoChunkEncoded(data.byteLength);
    webSocket.send(message);
  }

  function resetMetricsWindow() {
    measuredFps = null;
    measuredBitrate = null;
    metricsWindowStartMs = null;
    metricsWindowFrames = 0;
    metricsWindowBytes = 0;
  }

  function markVideoChunkEncoded(sizeBytes) {
    const now = performance.now();
    if (metricsWindowStartMs === null) {
      metricsWindowStartMs = now;
    }
    metricsWindowFrames += 1;
    metricsWindowBytes += sizeBytes;

    const elapsedMs = now - metricsWindowStartMs;
    if (elapsedMs < 1000) return;

    const elapsedSec = elapsedMs / 1000;
    measuredFps = metricsWindowFrames / elapsedSec;
    measuredBitrate = (metricsWindowBytes * 8) / elapsedSec;
    metricsWindowStartMs = now;
    metricsWindowFrames = 0;
    metricsWindowBytes = 0;
  }

  function startAudioCapture() {
    if (!videoElement) return;
    if (audioTrack && 'MediaStreamTrackProcessor' in window) {
      if (startAudioTrackProcessor()) {
        scheduleAudioFallbackCheck();
        return;
      }
    }
    startAudioCaptureWebAudio();
  }

  function startAudioCaptureWebAudio() {
    const ctx = getAudioContext();
    if (!ctx) return;
    ensureAudioContextRunning();

    if (typeof videoElement.captureStream === 'function') {
      captureStreamSupported = true;
      if (!mediaStream) {
        return;
      }
    }

    if (audioProcessorNode || audioCaptureSinkNode) {
      return;
    }

    let sourceNode = null;
    if (mediaStream && mediaStream.getAudioTracks && mediaStream.getAudioTracks().length > 0) {
      try {
        sourceNode = ctx.createMediaStreamSource(mediaStream);
        audioSourceStream = mediaStream;
      } catch (err) {
        console.warn('[BrowserPort] Failed to bind stream audio source:', err);
        sourceNode = null;
        audioSourceStream = null;
      }
    }

    if (!sourceNode) {
      sourceNode = attachElementAudioOutput(videoElement);
      if (!sourceNode) return;
      audioSourceStream = null;
    }

    audioCaptureSourceNode = sourceNode;
    audioProcessorNode = ctx.createScriptProcessor(AUDIO_BUFFER_SIZE, 2, 1);
    audioCaptureSinkNode = ctx.createMediaStreamDestination();

    audioCaptureMode = 'webaudio';
    audioProcessorNode.onaudioprocess = onAudioProcess;
    audioCaptureSourceNode.connect(audioProcessorNode);
    audioProcessorNode.connect(audioCaptureSinkNode);
  }

  function startAudioTrackProcessor() {
    if (!audioTrack || !('MediaStreamTrackProcessor' in window)) return false;
    if (audioTrack.readyState === 'ended') return false;
    if (audioTrackProcessorRunning) return true;
    stopAudioTrackProcessor();
    try {
      audioTrackProcessor = new MediaStreamTrackProcessor({ track: audioTrack });
      audioTrackReader = audioTrackProcessor.readable.getReader();
      audioTrackProcessorRunning = true;
      audioCaptureMode = 'track';
      processAudioFrames().catch((err) => {
        console.error('[BrowserPort] Audio track processor error:', err);
      });
      return true;
    } catch (err) {
      console.warn('[BrowserPort] Failed to start audio track processor:', err);
      stopAudioTrackProcessor();
      return false;
    }
  }

  async function processAudioFrames() {
    try {
      while (audioTrackProcessorRunning && audioTrackReader) {
        const result = await audioTrackReader.read();
        if (!audioTrackProcessorRunning || result.done) break;
        const audioData = result.value;
        if (audioData) {
          handleAudioData(audioData);
          audioData.close();
        }
      }
    } catch (err) {
      console.error('[BrowserPort] Audio track read failed:', err);
    }
  }

  function handleAudioData(audioData) {
    if (!webSocket || webSocket.readyState !== WebSocket.OPEN) return;
    if (!videoElement || !currentPlayerId) return;
    if (videoElement.paused) return;

    const playbackRate = Number(videoElement.playbackRate);
    if (!Number.isFinite(playbackRate) || Math.abs(playbackRate - 1.0) > 0.001) {
      return;
    }

    const mono = extractMonoFromAudioData(audioData);
    if (!mono || mono.length === 0) return;

    const currentTime = Number.isFinite(videoElement.currentTime)
      ? videoElement.currentTime
      : 0;
    noteAudioChunk();
    sendAudioChunk(mono, audioData.sampleRate, currentTime);
  }

  function extractMonoFromAudioData(audioData) {
    const frameCount = audioData.numberOfFrames;
    const channelCount = audioData.numberOfChannels;
    if (!frameCount || !channelCount) return null;

    const mono = new Float32Array(frameCount);

    if (channelCount === 1) {
      const buffer = new Float32Array(frameCount);
      if (!copyAudioDataPlanar(audioData, buffer, 0)) {
        if (!copyAudioDataInterleaved(audioData, buffer)) {
          return null;
        }
      }
      mono.set(buffer);
      return mono;
    }

    if (copyAudioDataPlanarChannels(audioData, mono, frameCount, channelCount)) {
      return mono;
    }

    const interleaved = new Float32Array(frameCount * channelCount);
    if (!copyAudioDataInterleaved(audioData, interleaved)) {
      return null;
    }
    for (let i = 0; i < frameCount; i += 1) {
      let sum = 0.0;
      const base = i * channelCount;
      for (let ch = 0; ch < channelCount; ch += 1) {
        sum += interleaved[base + ch];
      }
      mono[i] = sum / channelCount;
    }
    return mono;
  }

  function copyAudioDataPlanar(audioData, buffer, planeIndex) {
    try {
      audioData.copyTo(buffer, { planeIndex, format: 'f32-planar' });
      return true;
    } catch {
      return false;
    }
  }

  function copyAudioDataPlanarChannels(audioData, mono, frameCount, channelCount) {
    const channelBuffer = new Float32Array(frameCount);
    try {
      for (let ch = 0; ch < channelCount; ch += 1) {
        audioData.copyTo(channelBuffer, { planeIndex: ch, format: 'f32-planar' });
        for (let i = 0; i < frameCount; i += 1) {
          mono[i] += channelBuffer[i];
        }
      }
    } catch {
      return false;
    }
    const inv = 1.0 / channelCount;
    for (let i = 0; i < frameCount; i += 1) {
      mono[i] *= inv;
    }
    return true;
  }

  function copyAudioDataInterleaved(audioData, buffer) {
    try {
      audioData.copyTo(buffer, { planeIndex: 0, format: 'f32' });
      return true;
    } catch {
      try {
        audioData.copyTo(buffer);
        return true;
      } catch {
        return false;
      }
    }
  }

  function onAudioProcess(event) {
    if (!webSocket || webSocket.readyState !== WebSocket.OPEN) return;
    if (!videoElement || !currentPlayerId) return;
    if (videoElement.paused) return;

    const playbackRate = Number(videoElement.playbackRate);
    if (!Number.isFinite(playbackRate) || Math.abs(playbackRate - 1.0) > 0.001) {
      return;
    }

    const input = event.inputBuffer;
    const frameCount = input.length;
    const channelCount = input.numberOfChannels;
    if (!frameCount || !channelCount) return;

    const mono = new Float32Array(frameCount);
    for (let ch = 0; ch < channelCount; ch += 1) {
      const channel = input.getChannelData(ch);
      for (let i = 0; i < frameCount; i += 1) {
        mono[i] += channel[i];
      }
    }
    if (channelCount > 1) {
      const inv = 1.0 / channelCount;
      for (let i = 0; i < frameCount; i += 1) {
        mono[i] *= inv;
      }
    }

    const currentTime = Number.isFinite(videoElement.currentTime) ? videoElement.currentTime : 0;
    noteAudioChunk();
    sendAudioChunk(mono, input.sampleRate, currentTime);
  }

  function sendAudioChunk(samples, sampleRate, currentTimeSec) {
    if (!webSocket || webSocket.readyState !== WebSocket.OPEN) return;
    if (!currentPlayerId || !samples || samples.length === 0) return;
    if (!Number.isFinite(sampleRate) || sampleRate <= 0) return;
    if (samples.length > 0xffff) return;

    const payloadBytes = samples.length * 4;
    const fullHeaderSize = CHUNK_HEADER_SIZE + AUDIO_META_SIZE;
    const message = new ArrayBuffer(fullHeaderSize + payloadBytes);
    const view = new DataView(message);

    view.setUint8(0, CHUNK_TYPE_AUDIO);
    view.setUint8(1, CHUNK_VERSION);
    view.setUint8(2, 0);
    view.setUint8(3, 0);
    view.setUint32(4, currentPlayerId ?? 0, true);

    const timestampUs = Number.isFinite(currentTimeSec)
      ? Math.max(0, Math.round(currentTimeSec * 1_000_000))
      : 0;
    view.setBigUint64(8, BigInt(timestampUs), true);
    view.setUint32(16, AUDIO_META_SIZE + payloadBytes, true);
    view.setUint32(CHUNK_HEADER_SIZE, Math.round(sampleRate), true);
    view.setUint16(CHUNK_HEADER_SIZE + 4, 1, true);
    view.setUint16(CHUNK_HEADER_SIZE + 6, samples.length, true);

    const payload = new Float32Array(message, fullHeaderSize, samples.length);
    payload.set(samples);
    webSocket.send(message);
  }

  function noteAudioChunk() {
    lastAudioChunkWallMs = performance.now();
  }

  function scheduleAudioFallbackCheck() {
    if (audioFallbackTimer) {
      clearTimeout(audioFallbackTimer);
    }
    audioFallbackTimer = setTimeout(() => {
      audioFallbackTimer = null;
      if (!videoElement || !captureActive) {
        return;
      }
      if (videoElement.paused) {
        scheduleAudioFallbackCheck();
        return;
      }
      const now = performance.now();
      if (lastAudioChunkWallMs === null || (now - lastAudioChunkWallMs) > 1500) {
        if (audioCaptureMode === 'track') {
          console.warn('[BrowserPort] Audio track stalled; falling back to WebAudio');
          stopAudioTrackProcessor();
          startAudioCaptureWebAudio();
        }
      }
    }, 1500);
  }

  function scheduleVideoFrame() {
    if (!videoElement) return;
    if (typeof videoElement.requestVideoFrameCallback === 'function') {
      frameCallbackHandle = videoElement.requestVideoFrameCallback(onVideoFrame);
    } else {
      frameCallbackHandle = setTimeout(() => onVideoFrame(performance.now(), {}),
        1000 / ENCODE.targetFps);
    }
  }

  function startTrackProcessor() {
    if (!videoTrack || !('MediaStreamTrackProcessor' in window)) {
      scheduleVideoFrame();
      return;
    }
    stopTrackProcessor();
    trackProcessor = new MediaStreamTrackProcessor({ track: videoTrack });
    trackReader = trackProcessor.readable.getReader();
    processorRunning = true;
    processTrackFrames().catch((err) => {
      console.error('[BrowserPort] Track processor error:', err);
      scheduleVideoFrame();
    });
  }

  async function processTrackFrames() {
    try {
      while (processorRunning && trackReader) {
        const result = await trackReader.read();
        if (!processorRunning || result.done) break;
        const frame = result.value;
        if (frame) {
          encodeVideoFrame(frame);
          frame.close();
        }
      }
    } catch (err) {
      console.error('[BrowserPort] Track processor read failed:', err);
    }
  }

  function onVideoFrame(now, metadata) {
    if (!videoElement || !encoder || encoder.state === 'closed') return;

    const mediaTime = Number.isFinite(metadata.mediaTime)
      ? metadata.mediaTime
      : (now / 1000);
    const timestampUs = Math.round(mediaTime * 1_000_000);

    const frame = new VideoFrame(videoElement, { timestamp: timestampUs });
    encodeVideoFrame(frame, timestampUs);
    frame.close();

    scheduleVideoFrame();
  }

  function encodeVideoFrame(frame, forcedTimestampUs) {
    if (!encoder || encoder.state === 'closed') return;
    if (encoder.encodeQueueSize > 2) return;

    let timestampUs = forcedTimestampUs;
    if (!Number.isFinite(timestampUs)) {
      timestampUs = Number.isFinite(frame.timestamp)
        ? frame.timestamp
        : Math.round(performance.now() * 1000);
    }

    const minIntervalUs = 1_000_000 / ENCODE.targetFps;
    const shouldThrottle = lastEncodedTimestampUs !== null
      && (timestampUs - lastEncodedTimestampUs) < minIntervalUs;
    if (shouldThrottle) return;

    const keyFrame = forceNextKeyframe || (frameCounter % ENCODE.keyframeInterval === 0);
    encoder.encode(frame, { keyFrame });
    forceNextKeyframe = false;
    frameCounter += 1;
    lastEncodedTimestampUs = timestampUs;
  }

  function stopTrackProcessor() {
    processorRunning = false;
    if (trackReader) {
      trackReader.cancel().catch(() => { });
      trackReader.releaseLock();
      trackReader = null;
    }
    trackProcessor = null;
  }

  function safeJsonParse(raw) {
    if (typeof raw !== 'string') return null;
    try {
      return JSON.parse(raw);
    } catch {
      return null;
    }
  }

  function bufferToBase64(buffer) {
    const bytes = new Uint8Array(buffer);
    let binary = '';
    for (let i = 0; i < bytes.length; i++) {
      binary += String.fromCharCode(bytes[i]);
    }
    return btoa(binary);
  }

  function startPlaybackReporter() {
    stopPlaybackReporter();
    playbackTimer = setInterval(() => reportPlayback(false), PLAYBACK_REPORT_INTERVAL_MS);
    reportPlayback(true);
  }

  function stopPlaybackReporter() {
    if (playbackTimer) {
      clearInterval(playbackTimer);
      playbackTimer = null;
    }
    lastPlaybackPayload = null;
  }

  function startStatusReporter() {
    stopStatusReporter();
    if (!captureActive || currentPlayerId === null) return;
    statusTimer = setInterval(() => {
      if (!captureActive || currentPlayerId === null) return;
      reportStatus('capturing');
    }, STATUS_REPORT_INTERVAL_MS);
  }

  function stopStatusReporter() {
    if (statusTimer) {
      clearInterval(statusTimer);
      statusTimer = null;
    }
  }

  function roundMetric(value, digits) {
    if (!Number.isFinite(value)) return null;
    const factor = 10 ** digits;
    return Math.round(value * factor) / factor;
  }

  function buildStreamStats() {
    const resolution = currentResolution && Number.isFinite(currentResolution.width)
      && Number.isFinite(currentResolution.height)
      ? {
        width: Math.round(currentResolution.width),
        height: Math.round(currentResolution.height),
      }
      : null;

    return {
      codec: currentCodec || (activeEncoderConfig?.codec ?? null),
      resolution,
      fps: Number.isFinite(measuredFps) ? roundMetric(measuredFps, 2) : ENCODE.targetFps,
      bitrate: Number.isFinite(measuredBitrate) ? Math.round(measuredBitrate) : ENCODE.bitrate,
      encoder: ENCODE.hardwareAcceleration || null,
      tabTitle: typeof document.title === 'string' && document.title.trim()
        ? document.title.trim()
        : null,
    };
  }

  function attachVideoListeners(video) {
    if (playbackListenersAttached && videoElement === video) return;
    detachVideoListeners();
    if (!video) return;
    video.addEventListener('play', onPlaybackEvent);
    video.addEventListener('playing', onPlaybackEvent);
    video.addEventListener('pause', onPlaybackEvent);
    video.addEventListener('timeupdate', onPlaybackEvent);
    video.addEventListener('durationchange', onPlaybackEvent);
    video.addEventListener('seeked', onPlaybackEvent);
    video.addEventListener('seeking', onPlaybackEvent);
    video.addEventListener('ratechange', onPlaybackEvent);
    video.addEventListener('ended', onPlaybackEvent);
    video.addEventListener('loadedmetadata', onPlaybackEvent);
    video.addEventListener('emptied', onPlaybackEvent);
    playbackListenersAttached = true;
  }

  function detachVideoListeners() {
    if (!videoElement || !playbackListenersAttached) return;
    videoElement.removeEventListener('play', onPlaybackEvent);
    videoElement.removeEventListener('playing', onPlaybackEvent);
    videoElement.removeEventListener('pause', onPlaybackEvent);
    videoElement.removeEventListener('timeupdate', onPlaybackEvent);
    videoElement.removeEventListener('durationchange', onPlaybackEvent);
    videoElement.removeEventListener('seeked', onPlaybackEvent);
    videoElement.removeEventListener('seeking', onPlaybackEvent);
    videoElement.removeEventListener('ratechange', onPlaybackEvent);
    videoElement.removeEventListener('ended', onPlaybackEvent);
    videoElement.removeEventListener('loadedmetadata', onPlaybackEvent);
    videoElement.removeEventListener('emptied', onPlaybackEvent);
    playbackListenersAttached = false;
  }

  function onPlaybackEvent(event) {
    const force = event?.type && event.type !== 'timeupdate';
    if (event?.type === 'play' || event?.type === 'playing') {
      ensureCaptureForPlayback(event.type);
    } else if (event?.type === 'ended') {
      reportStatus('ended');
      scheduleRecapture();
    } else if (event?.type === 'loadedmetadata' || event?.type === 'emptied') {
      const src = getVideoSrc(videoElement);
      if (src && src !== lastVideoSrc) {
        lastVideoSrc = src;
        scheduleRecapture();
      }
    }
    reportPlayback(force);
  }

  function reportPlayback(force) {
    if (!videoElement || !webSocket || webSocket.readyState !== WebSocket.OPEN) return;
    if (!currentPlayerId) return;

    const duration = Number.isFinite(videoElement.duration) ? videoElement.duration : null;
    const currentTime = Number.isFinite(videoElement.currentTime)
      ? videoElement.currentTime
      : null;

    const payload = {
      type: 'playback',
      playerId: currentPlayerId,
      currentTime,
      duration,
      paused: videoElement.paused,
      playbackRate: Number.isFinite(videoElement.playbackRate) ? videoElement.playbackRate : null,
      seeking: !!videoElement.seeking,
    };

    if (!force && !shouldSendPlayback(payload)) return;
    lastPlaybackPayload = payload;
    webSocket.send(JSON.stringify(payload));
  }

  function sendJsonMessage(payload) {
    if (!webSocket || webSocket.readyState !== WebSocket.OPEN) return;
    webSocket.send(JSON.stringify(payload));
  }

  function shouldSendPlayback(payload) {
    if (!lastPlaybackPayload) return true;
    if (payload.paused !== lastPlaybackPayload.paused) return true;
    if (payload.playbackRate !== lastPlaybackPayload.playbackRate) return true;
    if (payload.seeking !== lastPlaybackPayload.seeking) return true;
    if (payload.duration !== lastPlaybackPayload.duration) return true;
    if (payload.currentTime === null || lastPlaybackPayload.currentTime === null) return true;
    return Math.abs(payload.currentTime - lastPlaybackPayload.currentTime) >= 0.25;
  }

  function stopAudioCapture() {
    stopAudioTrackProcessor();
    if (audioFallbackTimer) {
      clearTimeout(audioFallbackTimer);
      audioFallbackTimer = null;
    }
    if (audioCaptureSourceNode && audioProcessorNode) {
      try {
        audioCaptureSourceNode.disconnect(audioProcessorNode);
      } catch {
        // ignore
      }
    }
    if (audioProcessorNode) {
      audioProcessorNode.onaudioprocess = null;
      try {
        audioProcessorNode.disconnect();
      } catch {
        // ignore
      }
      audioProcessorNode = null;
    }
    if (audioCaptureSinkNode) {
      try {
        audioCaptureSinkNode.disconnect();
      } catch {
        // ignore
      }
      audioCaptureSinkNode = null;
    }
    audioCaptureSourceNode = null;
    audioSourceStream = null;
    audioCaptureMode = null;
    lastAudioChunkWallMs = null;
  }

  function stopAudioTrackProcessor() {
    audioTrackProcessorRunning = false;
    if (audioTrackReader) {
      audioTrackReader.cancel().catch(() => { });
      audioTrackReader.releaseLock();
      audioTrackReader = null;
    }
    audioTrackProcessor = null;
  }

  function cleanup(keepPlayerId = false, keepWebSocket = false) {
    if (recaptureTimer) {
      clearTimeout(recaptureTimer);
      recaptureTimer = null;
    }
    stopReconnectTimer();
    if (connectTimeoutTimer) {
      clearTimeout(connectTimeoutTimer);
      connectTimeoutTimer = null;
    }

    stopPlaybackReporter();
    stopStatusReporter();
    detachVideoListeners();
    stopTrackProcessor();
    stopAudioCapture();

    if (videoElement && frameCallbackHandle !== null) {
      if (typeof videoElement.cancelVideoFrameCallback === 'function') {
        videoElement.cancelVideoFrameCallback(frameCallbackHandle);
      } else {
        clearTimeout(frameCallbackHandle);
      }
      frameCallbackHandle = null;
    }

    if (encoder) {
      encoder.flush().catch(() => { });
      encoder.close();
      encoder = null;
    }

    if (webSocket && !keepWebSocket) {
      suppressReconnect = true;
      webSocket.onclose = null;
      webSocket.onerror = null;
      webSocket.close();
      webSocket = null;
    }

    if (videoTrack) {
      videoTrack.stop();
      videoTrack = null;
    }
    if (audioTrack) {
      audioTrack.stop();
      audioTrack = null;
    }
    mediaStream = null;
    videoElement = null;
    captureStreamSupported = false;
    lastVideoSrc = null;
    resetMetricsWindow();
    activeEncoderConfig = null;
    currentCodec = null;
    currentResolution = null;
    lastDecoderConfigSig = null;
    pendingDecoderConfig = null;
    lastEncodedTimestampUs = null;
    frameCounter = 0;
    forceNextKeyframe = false;

    if (!keepPlayerId) {
      currentPlayerId = null;
    }
  }

  function shouldReconnect() {
    return captureActive && currentPlayerId !== null;
  }

  function scheduleReconnect() {
    if (!shouldReconnect()) return;
    if (reconnectTimer) return;
    reconnectTimer = setInterval(() => {
      if (!shouldReconnect()) {
        stopReconnectTimer();
        return;
      }
      if (!webSocket || webSocket.readyState === WebSocket.CLOSED) {
        connectWebSocket();
      }
    }, RECONNECT_INTERVAL_MS);
  }

  function stopReconnectTimer() {
    if (reconnectTimer) {
      clearInterval(reconnectTimer);
      reconnectTimer = null;
    }
  }

  function reportStatus(status, error) {
    if (currentPlayerId === null) return;
    chrome.runtime.sendMessage({
      type: 'STREAM_STATUS',
      playerId: currentPlayerId,
      status,
      error,
      stats: buildStreamStats(),
    });
  }

  consumePendingSearchRequest();
}
