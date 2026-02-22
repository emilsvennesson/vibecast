(() => {
  const app = {
    ws: null,
    reconnectTimer: null,
    activeSessionId: null,
    player: null,
    lastStateKey: "",
    autoplayMuted: false,
  };

  const connectionEl = document.getElementById("connection");
  const sessionEl = document.getElementById("session");
  const stateEl = document.getElementById("state");
  const titleEl = document.getElementById("title");
  const subtitleEl = document.getElementById("subtitle");
  const logEl = document.getElementById("log");
  const videoEl = document.getElementById("video");

  function pushLog(message) {
    const line = document.createElement("div");
    line.textContent = new Date().toLocaleTimeString() + "  " + message;
    logEl.prepend(line);
    while (logEl.childElementCount > 10) {
      logEl.removeChild(logEl.lastChild);
    }
  }

  function setConnected(connected) {
    connectionEl.dataset.connected = String(connected);
    connectionEl.textContent = connected ? "connected" : "disconnected";
  }

  function wsUrl() {
    const wsProtocol = window.location.protocol === "https:" ? "wss" : "ws";
    return wsProtocol + "://" + window.location.host + "/player?role=primary";
  }

  function toKeySystem(system) {
    const normalized = (system || "").trim().toLowerCase();
    if (!normalized) {
      return null;
    }
    if (normalized === "widevine" || normalized === "com.widevine.alpha") {
      return "com.widevine.alpha";
    }
    if (
      normalized === "clearkey" ||
      normalized === "org.w3.clearkey" ||
      normalized === "com.w3.clearkey"
    ) {
      return "org.w3.clearkey";
    }
    if (normalized === "playready" || normalized === "com.microsoft.playready") {
      return "com.microsoft.playready";
    }
    return system;
  }

  function canSend() {
    return app.ws !== null && app.ws.readyState === WebSocket.OPEN;
  }

  function sendError(code, message) {
    if (!canSend() || app.activeSessionId === null) {
      return;
    }
    app.ws.send(
      JSON.stringify({
        type: "error",
        sessionId: app.activeSessionId,
        code,
        message,
      })
    );
  }

  function sendStateReport(sessionId, playerState, idleReason = null, force = false) {
    if (!canSend()) {
      return;
    }

    const currentTime = Number.isFinite(videoEl.currentTime) ? videoEl.currentTime : 0;
    const duration = Number.isFinite(videoEl.duration) ? videoEl.duration : null;
    const roundedTime = Math.round(currentTime * 10) / 10;
    const roundedDuration = duration === null ? "none" : String(Math.round(duration * 10) / 10);
    const key = [sessionId, playerState, idleReason || "none", roundedTime, roundedDuration].join("|");

    if (!force && key === app.lastStateKey) {
      return;
    }

    app.lastStateKey = key;
    const payload = {
      type: "state",
      sessionId,
      playerState,
      currentTime,
    };
    if (duration !== null) {
      payload.duration = duration;
    }
    if (idleReason !== null) {
      payload.idleReason = idleReason;
    }

    app.ws.send(JSON.stringify(payload));
    stateEl.textContent =
      idleReason === null ? playerState : playerState + " (" + idleReason + ")";
  }

  function sendCurrentState(force = false) {
    if (app.activeSessionId === null) {
      return;
    }

    if (videoEl.ended) {
      sendStateReport(app.activeSessionId, "IDLE", "FINISHED", force);
      return;
    }
    if (videoEl.seeking) {
      sendStateReport(app.activeSessionId, "BUFFERING", null, force);
      return;
    }
    if (!videoEl.paused && videoEl.readyState < HTMLMediaElement.HAVE_FUTURE_DATA) {
      sendStateReport(app.activeSessionId, "BUFFERING", null, force);
      return;
    }

    const state = videoEl.paused ? "PAUSED" : "PLAYING";
    sendStateReport(app.activeSessionId, state, null, force);
  }

  function resetSessionUi() {
    videoEl.controls = false;
    app.autoplayMuted = false;
    sessionEl.textContent = "-";
    titleEl.textContent = "Waiting for LOAD";
    subtitleEl.textContent =
      "Open this page on your playback device. It listens on /player?role=primary.";
    stateEl.textContent = "IDLE";
  }

  function isAutoplayBlocked(error) {
    if (error && typeof error === "object" && "name" in error) {
      if (String(error.name) === "NotAllowedError") {
        return true;
      }
    }

    const message = error instanceof Error ? error.message : String(error);
    const normalized = message.toLowerCase();
    return normalized.includes("not allowed") || normalized.includes("permission");
  }

  async function safePlay(options = {}) {
    const allowMutedFallback = Boolean(options.allowMutedFallback);

    try {
      await videoEl.play();
      return true;
    } catch (error) {
      if (allowMutedFallback && isAutoplayBlocked(error) && !videoEl.muted) {
        const originalMuted = videoEl.muted;
        videoEl.muted = true;
        try {
          await videoEl.play();
          app.autoplayMuted = true;
          pushLog("Autoplay blocked with sound; resumed muted");
          return true;
        } catch {
          videoEl.muted = originalMuted;
        }
      }

      const message = error instanceof Error ? error.message : String(error);
      pushLog("Playback start blocked: " + message);
      sendError("PLAYBACK_PLAY_FAILED", message);
      return false;
    }
  }

  function configureDrm(player, drm) {
    player.configure({
      drm: { servers: {}, advanced: {}, clearKeys: {} },
      manifest: { dash: { keySystemsByURI: {} } },
    });
    if (!drm || !drm.licenseUrl) {
      return true;
    }

    const keySystem = toKeySystem(drm.system);
    if (!keySystem) {
      return false;
    }

    const headers = drm.headers && typeof drm.headers === "object" ? drm.headers : {};
    const servers = {};
    servers[keySystem] = drm.licenseUrl;
    const advanced = {};
    advanced[keySystem] = { headers };
    const keySystemsByURI = {};
    if (keySystem === "org.w3.clearkey") {
      keySystemsByURI["urn:uuid:e2719d58-a985-b3c9-781a-b030af78d30e"] = "org.w3.clearkey";
    }

    player.configure({
      drm: { servers, advanced, clearKeys: {} },
      manifest: { dash: { keySystemsByURI } },
    });
    return true;
  }

  async function stopPlayback(sessionId, idleReason) {
    if (app.player !== null) {
      try {
        await app.player.unload();
      } catch {
        // ignore unload errors during teardown
      }
    }

    videoEl.pause();
    videoEl.controls = false;
    app.autoplayMuted = false;
    videoEl.removeAttribute("src");
    videoEl.load();
    app.lastStateKey = "";

    if (sessionId !== null) {
      sendStateReport(sessionId, "IDLE", idleReason, true);
    }
  }

  async function ensurePlayer() {
    if (app.player !== null) {
      return app.player;
    }

    shaka.polyfill.installAll();
    if (!shaka.Player.isBrowserSupported()) {
      throw new Error("This browser does not support required media APIs.");
    }

    const player = new shaka.Player();
    await player.attach(videoEl);
    player.addEventListener("error", (event) => {
      const detail = event && event.detail ? event.detail : null;
      const code = detail && typeof detail.code === "number" ? detail.code : "unknown";
      const message = detail && detail.message ? detail.message : "Shaka playback error";
      pushLog("Shaka error " + String(code) + ": " + message);
      sendError("SHAKA_" + String(code), message);
    });
    app.player = player;
    return player;
  }

  async function handleLoad(command) {
    const media = command.media;
    const streams = media && Array.isArray(media.streams) ? media.streams : [];
    if (streams.length === 0) {
      sendError("PLAYBACK_INVALID_LOAD", "Missing streams in load command.");
      return;
    }

    const firstStream = streams[0];
    const firstUrl =
      firstStream && typeof firstStream.url === "string" ? firstStream.url : "";
    if (!firstUrl) {
      sendError("PLAYBACK_INVALID_LOAD", "First stream has no URL.");
      return;
    }

    const previousSessionId = app.activeSessionId;
    if (previousSessionId !== null && previousSessionId !== command.sessionId) {
      await stopPlayback(previousSessionId, "INTERRUPTED");
    }

    app.activeSessionId = command.sessionId;
    app.lastStateKey = "";
    app.autoplayMuted = false;
    videoEl.controls = true;
    videoEl.muted = false;
    sessionEl.textContent = command.sessionId;
    titleEl.textContent = media.title || firstUrl;
    subtitleEl.textContent = media.subtitle || firstStream.contentType || firstUrl;

    try {
      const player = await ensurePlayer();
      sendStateReport(command.sessionId, "BUFFERING", null, true);
      const startTime = Number.isFinite(media.startTime) ? media.startTime : 0;

      let loaded = false;
      let lastErrorMessage = "No stream candidates could be loaded.";
      pushLog("Loading media for session " + command.sessionId);

      for (let i = 0; i < streams.length; i += 1) {
        const stream = streams[i];
        const streamUrl = stream && typeof stream.url === "string" ? stream.url : "";
        if (!streamUrl) {
          continue;
        }

        const streamType =
          stream && typeof stream.contentType === "string" ? stream.contentType : "";
        const drm = stream && typeof stream === "object" ? stream.drm || null : null;
        if (!configureDrm(player, drm)) {
          lastErrorMessage = "Unsupported DRM key system for stream " + streamUrl;
          pushLog(lastErrorMessage);
          continue;
        }

        try {
          pushLog("Trying stream " + String(i + 1) + "/" + String(streams.length));
          await player.load(streamUrl, startTime, streamType || undefined);
          loaded = true;
          break;
        } catch (error) {
          lastErrorMessage = error instanceof Error ? error.message : String(error);
          pushLog(
            "Stream " + String(i + 1) + " failed: " + lastErrorMessage
          );
        }
      }

      if (!loaded) {
        throw new Error(lastErrorMessage);
      }

      if (media.autoplay === false) {
        videoEl.pause();
        sendStateReport(command.sessionId, "PAUSED", null, true);
        return;
      }

      const started = await safePlay({ allowMutedFallback: true });
      const state = started ? "PLAYING" : "PAUSED";
      sendStateReport(command.sessionId, state, null, true);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      pushLog("Load failed: " + message);
      sendError("PLAYBACK_LOAD_FAILED", message);
    }
  }

  async function handleCommand(command) {
    if (!command || typeof command.type !== "string") {
      return;
    }

    switch (command.type) {
      case "load":
        await handleLoad(command);
        break;
      case "play":
        if (command.sessionId !== app.activeSessionId) {
          return;
        }
        if (await safePlay({ allowMutedFallback: true })) {
          sendStateReport(command.sessionId, "PLAYING", null, true);
        }
        break;
      case "pause":
        if (command.sessionId !== app.activeSessionId) {
          return;
        }
        videoEl.pause();
        sendStateReport(command.sessionId, "PAUSED", null, true);
        break;
      case "seek":
        if (command.sessionId !== app.activeSessionId) {
          return;
        }
        if (Number.isFinite(command.position)) {
          videoEl.currentTime = command.position;
        }
        sendCurrentState(true);
        break;
      case "stop": {
        if (command.sessionId !== app.activeSessionId) {
          return;
        }
        const sessionId = app.activeSessionId;
        await stopPlayback(sessionId, "CANCELLED");
        app.activeSessionId = null;
        resetSessionUi();
        break;
      }
      case "volume":
        if (command.sessionId !== app.activeSessionId) {
          return;
        }
        if (Number.isFinite(command.level)) {
          videoEl.volume = Math.max(0, Math.min(1, command.level));
        }
        videoEl.muted = Boolean(command.muted);
        if (!videoEl.muted) {
          app.autoplayMuted = false;
        }
        sendCurrentState(true);
        break;
      default:
        break;
    }
  }

  function connectWebSocket() {
    const socket = new WebSocket(wsUrl());
    app.ws = socket;
    setConnected(false);

    socket.addEventListener("open", () => {
      pushLog("Connected to " + wsUrl());
      setConnected(true);
    });

    socket.addEventListener("message", (event) => {
      if (typeof event.data !== "string") {
        return;
      }

      try {
        const command = JSON.parse(event.data);
        void handleCommand(command);
      } catch {
        pushLog("Ignoring malformed command payload");
      }
    });

    socket.addEventListener("error", () => {
      socket.close();
    });

    socket.addEventListener("close", () => {
      if (app.ws === socket) {
        app.ws = null;
      }
      setConnected(false);
      pushLog("Disconnected from player server. Reconnecting...");

      if (app.reconnectTimer !== null) {
        return;
      }

      app.reconnectTimer = window.setTimeout(() => {
        app.reconnectTimer = null;
        connectWebSocket();
      }, 1500);
    });
  }

  function bindVideoEvents() {
    videoEl.addEventListener("playing", () => {
      sendCurrentState(true);
    });
    videoEl.addEventListener("pause", () => {
      sendCurrentState(true);
    });
    videoEl.addEventListener("waiting", () => {
      sendCurrentState(true);
    });
    videoEl.addEventListener("seeking", () => {
      sendCurrentState(true);
    });
    videoEl.addEventListener("seeked", () => {
      sendCurrentState(true);
    });
    videoEl.addEventListener("ended", () => {
      if (app.activeSessionId === null) {
        return;
      }
      sendStateReport(app.activeSessionId, "IDLE", "FINISHED", true);
    });
  }

  async function init() {
    resetSessionUi();
    bindVideoEvents();

    try {
      await ensurePlayer();
      pushLog("Shaka initialized and waiting for commands");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      titleEl.textContent = "Player initialization failed";
      subtitleEl.textContent = message;
      pushLog(message);
      return;
    }

    connectWebSocket();
    window.setInterval(() => {
      sendCurrentState(false);
    }, 1000);
  }

  void init();
})();
