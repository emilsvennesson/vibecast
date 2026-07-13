(() => {
  const app = {
    ws: null,
    reconnectTimer: null,
    activeSessionId: null,
    player: null,
    playerId: null,
    lastStateKey: "",
    autoplayMuted: false,
    connected: false,
    settingsReady: false,
    settingsApps: [],
    pendingSettings: new Map(),
    settingsRequestSequence: 0,
  };

  const connectionEl = document.getElementById("connection");
  const sessionEl = document.getElementById("session");
  const stateEl = document.getElementById("state");
  const titleEl = document.getElementById("title");
  const subtitleEl = document.getElementById("subtitle");
  const logEl = document.getElementById("log");
  const videoEl = document.getElementById("video");
  const trackControlsEl = document.getElementById("track-controls");
  const qualityControlEl = document.getElementById("quality-control");
  const qualitySelectEl = document.getElementById("quality-select");
  const audioControlEl = document.getElementById("audio-control");
  const audioSelectEl = document.getElementById("audio-select");
  const textControlEl = document.getElementById("text-control");
  const textSelectEl = document.getElementById("text-select");
  const btnCopy = document.getElementById("btn-copy");
  const btnClear = document.getElementById("btn-clear");
  const settingsEl = document.getElementById("settings-apps");
  const settingsStateEl = document.getElementById("settings-state");

  // ---------------------------------------------------------------------------
  // Shaka error code lookup
  // ---------------------------------------------------------------------------

  const SHAKA_ERROR_NAMES = {
    1001: "BAD_HTTP_STATUS",
    1002: "HTTP_ERROR",
    1003: "TIMEOUT",
    6001: "REQUESTED_KEY_SYSTEM_CONFIG_UNAVAILABLE",
    6002: "FAILED_TO_CREATE_CDM",
    6003: "FAILED_TO_ATTACH_TO_VIDEO",
    6004: "INVALID_SERVER_CERTIFICATE",
    6005: "FAILED_TO_CREATE_SESSION",
    6006: "FAILED_TO_GENERATE_LICENSE_REQUEST",
    6007: "LICENSE_REQUEST_FAILED",
    6008: "LICENSE_RESPONSE_REJECTED",
    6010: "ENCRYPTED_CONTENT_WITHOUT_DRM_INFO",
    6012: "NO_LICENSE_SERVER_GIVEN",
    6013: "OFFLINE_SESSION_REMOVED",
    6014: "EXPIRED",
    6015: "SERVER_CERTIFICATE_REQUEST_FAILED",
    6016: "INIT_DATA_TRANSFORM_ERROR",
    6017: "SERVER_CERTIFICATE_REQUIRED",
  };

  const SHAKA_CATEGORY_NAMES = {
    1: "NETWORK", 2: "TEXT", 3: "MEDIA", 4: "MANIFEST",
    5: "STREAMING", 6: "DRM", 7: "PLAYER", 8: "CAST",
    9: "STORAGE", 10: "ADS",
  };

  function formatShakaError(detail) {
    if (!detail || typeof detail !== "object") return String(detail);
    const code = detail.code;
    const category = detail.category;
    const codeName = SHAKA_ERROR_NAMES[code] || "UNKNOWN";
    const catName = SHAKA_CATEGORY_NAMES[category] || "CAT_" + category;
    let msg = catName + "." + codeName + " (" + code + ")";
    if (Array.isArray(detail.data) && detail.data.length > 0) {
      msg += " data=" + JSON.stringify(detail.data);
    }
    return msg;
  }

  // ---------------------------------------------------------------------------
  // Logging
  // ---------------------------------------------------------------------------

  const MAX_LOG_LINES = 500;

  function ts() {
    return new Date().toLocaleTimeString("en-GB", { hour12: false });
  }

  /**
   * Append a log entry.
   *  kind: "info" | "ws-send" | "ws-recv" | "net" | "err"
   *  message: short summary text
   *  detail: optional object to render as formatted JSON block
   */
  function pushLog(kind, message, detail) {
    const entry = document.createElement("div");
    entry.className = "log-entry " + kind;

    const timestamp = document.createElement("span");
    timestamp.className = "ts";
    timestamp.textContent = ts() + "  ";
    entry.appendChild(timestamp);

    entry.appendChild(document.createTextNode(message));

    if (detail !== undefined && detail !== null) {
      const json = document.createElement("span");
      json.className = "log-json";
      try {
        json.textContent = JSON.stringify(detail, null, 2);
      } catch {
        json.textContent = String(detail);
      }
      entry.appendChild(json);
    }

    logEl.prepend(entry);
    while (logEl.childElementCount > MAX_LOG_LINES) {
      logEl.removeChild(logEl.lastChild);
    }
  }

  // Filter toggles.
  document.querySelectorAll(".log-filter").forEach((btn) => {
    btn.addEventListener("click", () => {
      const kind = btn.dataset.kind;
      btn.classList.toggle("active");
      logEl.classList.toggle("hide-" + kind);
    });
  });

  // Copy / clear buttons.
  const copyIconSvg = btnCopy.innerHTML;
  const checkIconSvg = '<svg viewBox="0 0 24 24"><path d="M20 6L9 17l-5-5"/></svg>';

  btnCopy.addEventListener("click", () => {
    const lines = [];
    for (const el of logEl.children) {
      // Only copy visible entries.
      const isVisible =
        el.offsetParent !== null && window.getComputedStyle(el).display !== "none";
      if (isVisible) {
        lines.push(el.textContent);
      }
    }
    navigator.clipboard.writeText(lines.reverse().join("\n")).then(() => {
      btnCopy.innerHTML = checkIconSvg;
      btnCopy.classList.add("copied");
      setTimeout(() => {
        btnCopy.innerHTML = copyIconSvg;
        btnCopy.classList.remove("copied");
      }, 1500);
    });
  });

  btnClear.addEventListener("click", () => {
    logEl.innerHTML = "";
    pushLog("info", "Log cleared");
  });

  function setConnected(connected) {
    app.connected = connected;
    if (!connected) {
      app.settingsReady = false;
      app.pendingSettings.clear();
    }
    connectionEl.dataset.connected = String(connected);
    connectionEl.textContent = connected ? "connected" : "disconnected";
    renderSettings();
  }

  function wsUrl() {
    const wsProtocol = window.location.protocol === "https:" ? "wss" : "ws";
    return wsProtocol + "://" + window.location.host + "/player";
  }

  // ---------------------------------------------------------------------------
  // Registration: announce this player's identity + capabilities on connect.
  // ---------------------------------------------------------------------------

  function playerId() {
    if (app.playerId) return app.playerId;

    let id = null;
    try {
      id = window.localStorage.getItem("vibecast_player_id");
    } catch {
      id = null;
    }
    if (!id) {
      id =
        window.crypto && window.crypto.randomUUID
          ? window.crypto.randomUUID()
          : "browser-" + Math.random().toString(16).slice(2);
      try {
        window.localStorage.setItem("vibecast_player_id", id);
      } catch {
        /* storage unavailable: fall back to an ephemeral id */
      }
    }
    app.playerId = id;
    return id;
  }

  function detectVideoCodecs() {
    const checks = [
      ["h264", 'video/mp4; codecs="avc1.640028"'],
      ["hevc", 'video/mp4; codecs="hvc1.1.6.L93.B0"'],
      ["vp9", 'video/webm; codecs="vp9"'],
      ["av1", 'video/mp4; codecs="av01.0.05M.08"'],
    ];
    const ms = window.MediaSource;
    const out = [];
    for (const [name, mime] of checks) {
      if (ms && ms.isTypeSupported && ms.isTypeSupported(mime)) out.push(name);
    }
    return out.length ? out : ["h264"];
  }

  function buildRegisterFrame() {
    const screen = window.screen || {};
    return {
      type: "register",
      player: {
        playerId: playerId(),
        name: "Browser",
        capabilities: {
          platform: "browser",
          drm: [{ system: "com.widevine.alpha" }, { system: "org.w3.clearkey" }],
          videoCodecs: detectVideoCodecs(),
          audioCodecs: ["aac", "opus"],
          maxResolution: {
            width: screen.width || 1920,
            height: screen.height || 1080,
          },
          hdrFormats: [],
          frameRates: [24, 25, 30, 50, 60],
          subtitleFormats: ["vtt", "ttml"],
        },
      },
    };
  }

  // ---------------------------------------------------------------------------
  // DRM key system helpers
  // ---------------------------------------------------------------------------

  function toKeySystem(system) {
    const normalized = (system || "").trim().toLowerCase();
    if (!normalized) return null;
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

  // ---------------------------------------------------------------------------
  // WebSocket send helpers
  // ---------------------------------------------------------------------------

  function canSend() {
    return app.ws !== null && app.ws.readyState === WebSocket.OPEN;
  }

  function wsSend(payload) {
    const json = JSON.stringify(payload);
    app.ws.send(json);
    const logPayload =
      payload.type === "settingsUpdate"
        ? {
            type: payload.type,
            requestId: payload.requestId,
            appKey: payload.appKey,
            expectedRevision: payload.expectedRevision,
            changedKeys: Object.keys(payload.changes),
          }
        : payload;
    pushLog("ws-send", ">> " + payload.type, logPayload);
  }

  function sendError(code, message) {
    if (!canSend() || app.activeSessionId === null) return;
    wsSend({
      type: "error",
      sessionId: app.activeSessionId,
      code,
      message,
    });
  }

  function sendStateReport(sessionId, playerState, idleReason = null, force = false) {
    if (!canSend()) return;

    const currentTime = Number.isFinite(videoEl.currentTime) ? videoEl.currentTime : 0;
    const duration = Number.isFinite(videoEl.duration) ? videoEl.duration : null;
    const roundedTime = Math.round(currentTime * 10) / 10;
    const roundedDuration = duration === null ? "none" : String(Math.round(duration * 10) / 10);
    const key = [sessionId, playerState, idleReason || "none", roundedTime, roundedDuration].join("|");

    if (!force && key === app.lastStateKey) return;

    app.lastStateKey = key;
    const payload = {
      type: "state",
      sessionId,
      playerState,
      currentTime,
    };
    if (duration !== null) payload.duration = duration;
    if (idleReason !== null) payload.idleReason = idleReason;

    wsSend(payload);
    stateEl.textContent =
      idleReason === null ? playerState : playerState + " (" + idleReason + ")";
  }

  function sendCurrentState(force = false) {
    if (app.activeSessionId === null) return;

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

  // ---------------------------------------------------------------------------
  // UI helpers
  // ---------------------------------------------------------------------------

  function qualityKey(track) {
    return [track.height || 0, track.frameRate || 0, track.videoCodec || ""].join("|");
  }

  function audioKey(track) {
    return JSON.stringify([
      track.language || "und",
      track.label || "",
      Array.isArray(track.roles) ? track.roles : [],
    ]);
  }

  function setOptions(select, options, selectedValue) {
    select.replaceChildren();
    for (const option of options) {
      const element = document.createElement("option");
      element.value = option.value;
      element.textContent = option.label;
      select.appendChild(element);
    }
    if (options.some((option) => option.value === selectedValue)) {
      select.value = selectedValue;
    }
  }

  function settingsRequestId() {
    if (window.crypto && window.crypto.randomUUID) return window.crypto.randomUUID();
    app.settingsRequestSequence += 1;
    return "browser-settings-" + Date.now() + "-" + app.settingsRequestSequence;
  }

  function settingControl(setting, disabled) {
    let control;
    if (setting.kind === "choice") {
      control = document.createElement("select");
      for (const option of Array.isArray(setting.options) ? setting.options : []) {
        const optionEl = document.createElement("option");
        optionEl.value = option.value;
        optionEl.textContent = option.label;
        control.appendChild(optionEl);
      }
      control.value = String(setting.value);
    } else if (setting.kind === "boolean") {
      control = document.createElement("input");
      control.type = "checkbox";
      control.checked = setting.value === true;
      control.className = "settings-toggle-input";
    } else {
      control = document.createElement("input");
      control.type = setting.kind === "string" ? "text" : "number";
      if (setting.kind === "integer") control.step = "1";
      if (setting.kind === "number") control.step = "any";
      control.value = setting.value === null || setting.value === undefined ? "" : setting.value;
    }
    control.disabled = disabled;
    return control;
  }

  function settingValue(setting, control) {
    switch (setting.kind) {
      case "choice":
      case "string":
        return control.value;
      case "boolean":
        return control.checked;
      case "integer":
        return Number.parseInt(control.value, 10);
      case "number":
        return Number(control.value);
      default:
        return undefined;
    }
  }

  function submitSetting(appSettings, setting, value) {
    if (
      setting.writable === false ||
      !app.connected ||
      !app.settingsReady ||
      !canSend() ||
      app.pendingSettings.has(appSettings.appKey)
    ) return;
    if (value === undefined || (typeof value === "number" && !Number.isFinite(value))) return;

    const requestId = settingsRequestId();
    app.pendingSettings.set(appSettings.appKey, requestId);
    wsSend({
      type: "settingsUpdate",
      requestId,
      appKey: appSettings.appKey,
      expectedRevision: appSettings.revision,
      changes: { [setting.key]: value },
    });
    renderSettings();
  }

  function renderSetting(appSettings, setting, pending) {
    const row = document.createElement("form");
    row.className = "setting-row";

    const copy = document.createElement("div");
    copy.className = "setting-copy";
    const label = document.createElement("label");
    label.textContent = setting.label;
    copy.appendChild(label);
    if (setting.description) {
      const description = document.createElement("p");
      description.textContent = setting.description;
      copy.appendChild(description);
    }

    const actions = document.createElement("div");
    actions.className = "setting-actions";
    const supported = ["choice", "boolean", "string", "integer", "number"].includes(
      setting.kind,
    );
    if (!supported) {
      const unsupported = document.createElement("span");
      unsupported.className = "setting-unsupported";
      unsupported.textContent = "Unsupported type: " + setting.kind;
      actions.appendChild(unsupported);
    } else {
      const readOnly = setting.writable === false;
      const control = settingControl(
        setting,
        readOnly || !app.connected || !app.settingsReady || pending,
      );
      const controlId = "setting-" + appSettings.appKey + "-" + setting.key;
      control.id = controlId;
      label.htmlFor = controlId;

      if (setting.kind === "boolean") {
        const toggle = document.createElement("label");
        toggle.className = "settings-toggle";
        toggle.htmlFor = controlId;
        toggle.appendChild(control);
        const track = document.createElement("span");
        track.className = "settings-toggle-track";
        toggle.appendChild(track);
        actions.appendChild(toggle);
      } else {
        actions.appendChild(control);
      }

      const apply = document.createElement("button");
      apply.type = "submit";
      apply.className = "setting-apply";
      apply.textContent = "Apply";
      apply.disabled = readOnly || !app.connected || !app.settingsReady || pending;
      actions.appendChild(apply);

      const reset = document.createElement("button");
      reset.type = "button";
      reset.className = "setting-reset";
      reset.textContent = "Reset";
      reset.title = "Reset to " + String(setting.default);
      reset.disabled = readOnly || !app.connected || !app.settingsReady || pending;
      reset.addEventListener("click", () => submitSetting(appSettings, setting, null));
      actions.appendChild(reset);

      row.addEventListener("submit", (event) => {
        event.preventDefault();
        if (!row.reportValidity()) return;
        submitSetting(appSettings, setting, settingValue(setting, control));
      });
    }

    row.append(copy, actions);
    return row;
  }

  function renderSettings() {
    if (!settingsEl || !settingsStateEl) return;
    settingsStateEl.textContent = app.settingsReady
      ? "live"
      : app.connected
        ? "synchronizing"
        : "read only";
    settingsStateEl.dataset.connected = String(app.connected && app.settingsReady);
    settingsEl.replaceChildren();

    const configurableApps = app.settingsApps.filter(
      (appSettings) => Array.isArray(appSettings.settings) && appSettings.settings.length > 0,
    );
    if (configurableApps.length === 0) {
      const empty = document.createElement("p");
      empty.className = "settings-empty";
      if (!app.connected) empty.textContent = "Settings appear when the player connects.";
      else if (!app.settingsReady) empty.textContent = "Waiting for the settings catalog...";
      else empty.textContent = "No app settings are available for this player.";
      settingsEl.appendChild(empty);
      return;
    }

    for (const appSettings of configurableApps) {
      const pending = app.pendingSettings.has(appSettings.appKey);
      const section = document.createElement("section");
      section.className = "settings-app";

      const header = document.createElement("header");
      const heading = document.createElement("h3");
      heading.textContent = appSettings.displayName;
      const revision = document.createElement("span");
      revision.className = "settings-revision";
      revision.textContent = pending ? "updating" : "rev " + appSettings.revision;
      header.append(heading, revision);
      section.appendChild(header);

      for (const setting of Array.isArray(appSettings.settings) ? appSettings.settings : []) {
        section.appendChild(renderSetting(appSettings, setting, pending));
      }
      settingsEl.appendChild(section);
    }
  }

  function replaceSettingsSnapshot(message) {
    if (!Array.isArray(message.apps)) throw new Error("settings snapshot apps must be an array");
    app.settingsApps = message.apps;
    app.settingsReady = true;
    renderSettings();
  }

  function applySettingsResult(message) {
    const replacement = message.app;
    if (!replacement || typeof replacement.appKey !== "string") {
      throw new Error("settings result must include an app");
    }
    if (app.pendingSettings.get(replacement.appKey) !== message.requestId) {
      throw new Error("settings result does not match a pending request");
    }

    const index = app.settingsApps.findIndex((candidate) => candidate.appKey === replacement.appKey);
    if (index === -1) app.settingsApps.push(replacement);
    else app.settingsApps[index] = replacement;
    app.pendingSettings.delete(replacement.appKey);
    renderSettings();
  }

  function audioLabel(track) {
    if (track.label) return track.label;
    const language = track.language && track.language !== "und" ? track.language : "Unknown";
    const roles = Array.isArray(track.roles) ? track.roles.filter((role) => role !== "main") : [];
    return roles.length ? language + " (" + roles.join(", ") + ")" : language;
  }

  function refreshTrackControls() {
    const player = app.player;
    if (player === null) return;

    const variants = player.getVariantTracks();
    const activeVariant = variants.find((track) => track.active) || null;
    const qualities = new Map();
    for (const track of variants) {
      if (!track.height) continue;
      const key = qualityKey(track);
      if (!qualities.has(key)) qualities.set(key, track);
    }
    const qualityOptions = Array.from(qualities.entries())
      .sort((left, right) => {
        const a = left[1];
        const b = right[1];
        return b.height - a.height || (b.frameRate || 0) - (a.frameRate || 0);
      })
      .map(([key, track]) => {
        const fps = track.frameRate ? " " + Math.round(track.frameRate) + "fps" : "";
        const codec = track.videoCodec ? " " + track.videoCodec.split(".")[0].toUpperCase() : "";
        return { value: key, label: track.height + "p" + fps + codec };
      });
    qualityOptions.unshift({ value: "auto", label: "Auto" });
    const abrEnabled = player.getConfiguration().abr.enabled;
    setOptions(
      qualitySelectEl,
      qualityOptions,
      abrEnabled || activeVariant === null ? "auto" : qualityKey(activeVariant),
    );
    qualityControlEl.hidden = qualities.size <= 1;

    const audioTracks = new Map();
    for (const track of variants) {
      const key = audioKey(track);
      if (!audioTracks.has(key)) audioTracks.set(key, track);
    }
    const audioOptions = Array.from(audioTracks.entries())
      .map(([key, track]) => ({ value: key, label: audioLabel(track) }))
      .sort((left, right) => left.label.localeCompare(right.label));
    setOptions(audioSelectEl, audioOptions, activeVariant ? audioKey(activeVariant) : "");
    audioControlEl.hidden = audioTracks.size <= 1;

    const textTracks = player.getTextTracks();
    const activeText = textTracks.find((track) => track.active) || null;
    const textOptions = [{ value: "off", label: "Off" }];
    for (const track of textTracks) {
      const generated = Array.isArray(track.roles) && track.roles.includes("caption");
      textOptions.push({
        value: String(track.id),
        label: track.label || (track.language || "Unknown") + (generated ? " (captions)" : ""),
      });
    }
    setOptions(
      textSelectEl,
      textOptions,
      player.isTextTrackVisible() && activeText ? String(activeText.id) : "off",
    );
    textControlEl.hidden = textTracks.length === 0;

    trackControlsEl.hidden =
      qualityControlEl.hidden && audioControlEl.hidden && textControlEl.hidden;
  }

  function resetTrackControls() {
    qualitySelectEl.replaceChildren();
    audioSelectEl.replaceChildren();
    textSelectEl.replaceChildren();
    qualityControlEl.hidden = true;
    audioControlEl.hidden = true;
    textControlEl.hidden = true;
    trackControlsEl.hidden = true;
  }

  qualitySelectEl.addEventListener("change", () => {
    const player = app.player;
    if (player === null) return;
    if (qualitySelectEl.value === "auto") {
      player.configure({ abr: { enabled: true } });
      refreshTrackControls();
      return;
    }

    const tracks = player.getVariantTracks();
    const active = tracks.find((track) => track.active) || null;
    const currentAudio = active ? audioKey(active) : null;
    const target =
      tracks.find(
        (track) => qualityKey(track) === qualitySelectEl.value && audioKey(track) === currentAudio,
      ) || tracks.find((track) => qualityKey(track) === qualitySelectEl.value);
    if (!target) return;
    player.configure({ abr: { enabled: false } });
    player.selectVariantTrack(target, true, 2);
    refreshTrackControls();
  });

  audioSelectEl.addEventListener("change", () => {
    const player = app.player;
    if (player === null) return;
    const tracks = player.getVariantTracks();
    const active = tracks.find((track) => track.active) || null;
    const currentQuality = active ? qualityKey(active) : null;
    const target =
      tracks.find(
        (track) => audioKey(track) === audioSelectEl.value && qualityKey(track) === currentQuality,
      ) || tracks.find((track) => audioKey(track) === audioSelectEl.value);
    if (!target) return;
    const role = Array.isArray(target.roles) ? target.roles[0] || "" : "";
    player.selectAudioLanguage(target.language || "und", role);
    player.selectVariantTrack(target, true, 2);
    refreshTrackControls();
  });

  textSelectEl.addEventListener("change", () => {
    const player = app.player;
    if (player === null) return;
    if (textSelectEl.value === "off") {
      player.setTextTrackVisibility(false);
      refreshTrackControls();
      return;
    }
    const track = player
      .getTextTracks()
      .find((candidate) => String(candidate.id) === textSelectEl.value);
    if (!track) return;
    player.selectTextTrack(track);
    player.setTextTrackVisibility(true);
    refreshTrackControls();
  });

  function resetSessionUi() {
    videoEl.controls = false;
    app.autoplayMuted = false;
    sessionEl.textContent = "-";
    titleEl.textContent = "Waiting for LOAD";
    subtitleEl.textContent =
      "Open this page on your playback device. It registers over /player.";
    stateEl.textContent = "IDLE";
    resetTrackControls();
  }

  function isAutoplayBlocked(error) {
    if (error && typeof error === "object" && "name" in error) {
      if (String(error.name) === "NotAllowedError") return true;
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
          pushLog("info", "Autoplay blocked with sound; resumed muted");
          return true;
        } catch {
          videoEl.muted = originalMuted;
        }
      }
      const message = error instanceof Error ? error.message : String(error);
      pushLog("err", "Playback start blocked: " + message);
      sendError("PLAYBACK_PLAY_FAILED", message);
      return false;
    }
  }

  // ---------------------------------------------------------------------------
  // DRM configuration
  // ---------------------------------------------------------------------------

  function configureDrm(player, drm) {
    player.configure({
      drm: { servers: {}, advanced: {}, clearKeys: {} },
      manifest: { dash: { keySystemsByURI: {} } },
    });

    if (!drm || !drm.licenseUrl) {
      pushLog("info", "  drm: none (clear content)");
      return true;
    }

    const keySystem = toKeySystem(drm.system);
    if (!keySystem) {
      pushLog("err", "  drm: unsupported key system: " + String(drm.system));
      return false;
    }

    pushLog("info", "  drm: " + keySystem + " -> " + drm.licenseUrl);

    const servers = {};
    servers[keySystem] = drm.licenseUrl;
    const keySystemsByURI = {};
    if (keySystem === "org.w3.clearkey") {
      keySystemsByURI["urn:uuid:e2719d58-a985-b3c9-781a-b030af78d30e"] = "org.w3.clearkey";
    }

    player.configure({
      drm: { servers, advanced: {}, clearKeys: {} },
      manifest: { dash: { keySystemsByURI } },
    });
    return true;
  }

  // ---------------------------------------------------------------------------
  // Network request/response filters
  // ---------------------------------------------------------------------------

  function installNetworkFilters(player) {
    const net = player.getNetworkingEngine();
    if (!net) return;

    const RequestType = shaka.net.NetworkingEngine.RequestType;
    const TYPE_NAMES = {};
    TYPE_NAMES[RequestType.MANIFEST] = "MANIFEST";
    TYPE_NAMES[RequestType.SEGMENT] = "SEGMENT";
    TYPE_NAMES[RequestType.LICENSE] = "LICENSE";
    TYPE_NAMES[RequestType.APP] = "APP";
    TYPE_NAMES[RequestType.TIMING] = "TIMING";
    TYPE_NAMES[RequestType.SERVER_CERTIFICATE] = "SERVER_CERT";
    TYPE_NAMES[RequestType.KEY] = "KEY";
    TYPE_NAMES[RequestType.ADS] = "ADS";
    TYPE_NAMES[RequestType.CONTENT_STEERING] = "STEERING";

    net.registerRequestFilter(function (type, request) {
      if (type === RequestType.SEGMENT) return;
      const typeName = TYPE_NAMES[type] || String(type);
      const uri = Array.isArray(request.uris) ? request.uris[0] : "?";
      const bodyLen = request.body ? request.body.byteLength : 0;
      const headers = {};
      if (request.headers) {
        for (const [k, v] of Object.entries(request.headers)) {
          headers[k] = v;
        }
      }
      pushLog("net", ">> " + typeName + " " + uri + " (" + bodyLen + "B)", {
        method: request.method || "GET",
        headers: Object.keys(headers).length > 0 ? headers : undefined,
        bodyLength: bodyLen,
      });
    });

    net.registerResponseFilter(function (type, response) {
      if (type === RequestType.SEGMENT) return;
      const typeName = TYPE_NAMES[type] || String(type);
      const dataLen = response.data ? response.data.byteLength : 0;
      const redirected = response.originalUri !== response.uri;
      pushLog("net", "<< " + typeName + " " + (response.status || "?") + " " + response.uri + " (" + dataLen + "B)" + (redirected ? " (redirect)" : ""));
    });
  }

  // ---------------------------------------------------------------------------
  // Player lifecycle
  // ---------------------------------------------------------------------------

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
    resetTrackControls();

    if (sessionId !== null) {
      sendStateReport(sessionId, "IDLE", idleReason, true);
    }
  }

  async function ensurePlayer() {
    if (app.player !== null) return app.player;

    shaka.polyfill.installAll();
    if (!shaka.Player.isBrowserSupported()) {
      throw new Error("This browser does not support required media APIs.");
    }

    const player = new shaka.Player();
    await player.attach(videoEl);

    player.addEventListener("error", (event) => {
      const detail = event && event.detail ? event.detail : null;
      const formatted = formatShakaError(detail);
      pushLog("err", "Shaka error: " + formatted, detail);
      const code = detail && typeof detail.code === "number" ? detail.code : "unknown";
      const message = detail && detail.message ? detail.message : formatted;
      sendError("SHAKA_" + String(code), message);
    });
    for (const eventName of [
      "trackschanged",
      "variantchanged",
      "textchanged",
      "texttrackvisibility",
    ]) {
      player.addEventListener(eventName, refreshTrackControls);
    }

    installNetworkFilters(player);
    app.player = player;
    return player;
  }

  // ---------------------------------------------------------------------------
  // Load handler
  // ---------------------------------------------------------------------------

  async function handleLoad(command) {
    const media = command.media;
    const streams = media && Array.isArray(media.streams) ? media.streams : [];
    if (streams.length === 0) {
      pushLog("err", "Load rejected: no streams in command", command);
      sendError("PLAYBACK_INVALID_LOAD", "Missing streams in load command.");
      return;
    }

    const firstStream = streams[0];
    const firstUrl =
      firstStream && typeof firstStream.url === "string" ? firstStream.url : "";
    if (!firstUrl) {
      pushLog("err", "Load rejected: first stream has no URL", command);
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

      pushLog("info", "Loading " + streams.length + " stream(s), startTime=" + startTime);

      let loaded = false;
      let lastErrorMessage = "No stream candidates could be loaded.";

      for (let i = 0; i < streams.length; i += 1) {
        const stream = streams[i];
        const streamUrl = stream && typeof stream.url === "string" ? stream.url : "";
        if (!streamUrl) continue;

        const streamType =
          stream && typeof stream.contentType === "string" ? stream.contentType : "";
        const drm = stream && typeof stream === "object" ? stream.drm || null : null;

        pushLog("info", "Stream " + (i + 1) + "/" + streams.length + ": " + (streamType || "(no mime)"), {
          contentType: streamType,
          drmSystem: drm && typeof drm.system === "string" ? drm.system : null,
        });

        if (!configureDrm(player, drm)) {
          lastErrorMessage = "Unsupported DRM key system for stream " + (i + 1);
          pushLog("err", lastErrorMessage);
          continue;
        }

        try {
          await player.load(streamUrl, startTime, streamType || undefined);
          pushLog("info", "Stream " + (i + 1) + " loaded OK");
          refreshTrackControls();
          loaded = true;
          break;
        } catch (error) {
          const detail = error && typeof error === "object" && "code" in error ? error : null;
          if (detail) {
            lastErrorMessage = formatShakaError(detail);
            pushLog("err", "Stream " + (i + 1) + " failed: " + lastErrorMessage, detail);
          } else {
            lastErrorMessage = error instanceof Error ? error.message : String(error);
            pushLog("err", "Stream " + (i + 1) + " failed: " + lastErrorMessage);
          }
        }
      }

      if (!loaded) throw new Error(lastErrorMessage);

      if (media.autoplay === false) {
        videoEl.pause();
        sendStateReport(command.sessionId, "PAUSED", null, true);
        return;
      }

      const started = await safePlay({ allowMutedFallback: true });
      sendStateReport(command.sessionId, started ? "PLAYING" : "PAUSED", null, true);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      pushLog("err", "Load failed: " + message);
      sendError("PLAYBACK_LOAD_FAILED", message);
    }
  }

  // ---------------------------------------------------------------------------
  // Command dispatch
  // ---------------------------------------------------------------------------

  async function handleCommand(command) {
    if (!command || typeof command.type !== "string") return;

    switch (command.type) {
      case "settingsSnapshot":
        pushLog(
          "ws-recv",
          "<< settingsSnapshot (" + (Array.isArray(command.apps) ? command.apps.length : 0) + " apps)",
        );
        try {
          replaceSettingsSnapshot(command);
        } catch (error) {
          pushLog("err", error instanceof Error ? error.message : String(error));
        }
        break;
      case "settingsUpdateResult":
        pushLog(
          "ws-recv",
          "<< settingsUpdateResult " + String(command.status || "unknown"),
        );
        try {
          applySettingsResult(command);
        } catch (error) {
          pushLog("err", error instanceof Error ? error.message : String(error));
        }
        break;
      case "load":
        pushLog("ws-recv", "<< load", {
          type: command.type,
          sessionId: command.sessionId,
          streamCount:
            command.media && Array.isArray(command.media.streams) ? command.media.streams.length : 0,
        });
        await handleLoad(command);
        break;
      case "play":
        pushLog("ws-recv", "<< play", command);
        if (command.sessionId !== app.activeSessionId) return;
        if (await safePlay({ allowMutedFallback: true })) {
          sendStateReport(command.sessionId, "PLAYING", null, true);
        }
        break;
      case "pause":
        pushLog("ws-recv", "<< pause", command);
        if (command.sessionId !== app.activeSessionId) return;
        videoEl.pause();
        sendStateReport(command.sessionId, "PAUSED", null, true);
        break;
      case "seek":
        pushLog("ws-recv", "<< seek position=" + command.position, command);
        if (command.sessionId !== app.activeSessionId) return;
        if (Number.isFinite(command.position)) {
          videoEl.currentTime = command.position;
        }
        sendCurrentState(true);
        break;
      case "stop":
        pushLog("ws-recv", "<< stop", command);
        if (command.sessionId !== app.activeSessionId) return;
        {
          const sessionId = app.activeSessionId;
          await stopPlayback(sessionId, "CANCELLED");
          app.activeSessionId = null;
          resetSessionUi();
        }
        break;
      case "volume":
        pushLog("ws-recv", "<< volume level=" + command.level + " muted=" + command.muted, command);
        if (command.sessionId !== app.activeSessionId) return;
        if (Number.isFinite(command.level)) {
          videoEl.volume = Math.max(0, Math.min(1, command.level));
        }
        videoEl.muted = Boolean(command.muted);
        if (!videoEl.muted) app.autoplayMuted = false;
        sendCurrentState(true);
        break;
      default:
        pushLog("ws-recv", "<< " + command.type + " (unhandled)", command);
        break;
    }
  }

  // ---------------------------------------------------------------------------
  // WebSocket connection
  // ---------------------------------------------------------------------------

  function connectWebSocket() {
    const url = wsUrl();
    const socket = new WebSocket(url);
    app.ws = socket;
    setConnected(false);

    socket.addEventListener("open", () => {
      pushLog("info", "WebSocket connected: " + url);
      try {
        socket.send(JSON.stringify(buildRegisterFrame()));
      } catch (err) {
        pushLog("err", "Failed to send register frame: " + err);
      }
      setConnected(true);
    });

    socket.addEventListener("message", (event) => {
      if (typeof event.data !== "string") return;
      try {
        const command = JSON.parse(event.data);
        void handleCommand(command).catch((error) => {
          pushLog("err", "Command failed: " + (error instanceof Error ? error.message : String(error)));
        });
      } catch {
        pushLog("err", "Malformed WS payload: " + event.data.slice(0, 200));
      }
    });

    socket.addEventListener("error", () => {
      socket.close();
    });

    socket.addEventListener("close", () => {
      if (app.ws === socket) app.ws = null;
      setConnected(false);
      pushLog("info", "WebSocket disconnected, reconnecting in 1.5s...");

      if (app.reconnectTimer !== null) return;
      app.reconnectTimer = window.setTimeout(() => {
        app.reconnectTimer = null;
        connectWebSocket();
      }, 1500);
    });
  }

  // ---------------------------------------------------------------------------
  // Video element events
  // ---------------------------------------------------------------------------

  function bindVideoEvents() {
    videoEl.addEventListener("playing", () => sendCurrentState(true));
    videoEl.addEventListener("pause", () => sendCurrentState(true));
    videoEl.addEventListener("waiting", () => sendCurrentState(true));
    videoEl.addEventListener("seeking", () => sendCurrentState(true));
    videoEl.addEventListener("seeked", () => sendCurrentState(true));
    videoEl.addEventListener("ended", () => {
      if (app.activeSessionId === null) return;
      sendStateReport(app.activeSessionId, "IDLE", "FINISHED", true);
    });
  }

  // ---------------------------------------------------------------------------
  // Initialization
  // ---------------------------------------------------------------------------

  async function init() {
    resetSessionUi();
    bindVideoEvents();

    const isSecure = window.isSecureContext;
    pushLog("info", "Origin: " + window.location.origin + " | secure=" + isSecure + " | " + navigator.userAgent);
    if (!isSecure) {
      pushLog("err", "NOT a secure context -- EME/DRM requires HTTPS or localhost/127.0.0.1");
    }

    try {
      await ensurePlayer();
      pushLog("info", "Shaka " + shaka.Player.version + " initialized");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      titleEl.textContent = "Player initialization failed";
      subtitleEl.textContent = message;
      pushLog("err", message);
      return;
    }

    try {
      const support = await shaka.Player.probeSupport();
      const drm = support.drm || {};
      const systems = Object.keys(drm);
      if (systems.length === 0) {
        pushLog("err", "DRM support: NONE");
      } else {
        const summary = systems
          .filter(function (ks) { return drm[ks]; })
          .join(", ");
        const unsupported = systems
          .filter(function (ks) { return !drm[ks]; })
          .join(", ");
        pushLog("info", "DRM supported: " + (summary || "none"));
        if (unsupported) pushLog("info", "DRM unavailable: " + unsupported);
      }
    } catch (error) {
      pushLog("err", "DRM probe failed: " + (error instanceof Error ? error.message : String(error)));
    }

    connectWebSocket();
    window.setInterval(() => sendCurrentState(false), 1000);
  }

  void init();
})();
