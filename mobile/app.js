"use strict";

const accountsNode = document.querySelector("#accounts");
const noticeNode = document.querySelector("#notice");
const refreshButton = document.querySelector("#refresh");
const connectionsButton = document.querySelector("#connections");
const statusNode = document.querySelector("#live-status");
const updatedNode = document.querySelector("#updated");
const STATE_CACHE_KEY = "agents-usage:last-state:v1";
const expandedOverrides = new Map();
const revealedNames = new Set();
const revealedEmails = new Set();
let latestState = null;
let lastSuccess = 0;
let locallyRefreshing = false;
let requestInFlight = false;
let resumedAt = Date.now();


function endpoint(path) { return new URL(`./${path}`, window.location.href); }
function escapeHtml(value) { return String(value ?? "").replace(/[&<>"']/g, character => ({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"})[character]); }

function validState(value) {
  return value && typeof value === "object" && Array.isArray(value.accounts) && Number.isFinite(value.server_time);
}

function restoreCachedState() {
  try {
    const cached = JSON.parse(localStorage.getItem(STATE_CACHE_KEY) || "null");
    if (!cached || !validState(cached.state) || !Number.isFinite(cached.saved_at)) return false;
    latestState = cached.state;
    lastSuccess = cached.saved_at;
    return true;
  } catch (_) {
    return false;
  }
}

function cacheState(state) {
  try {
    localStorage.setItem(STATE_CACHE_KEY, JSON.stringify({saved_at: Date.now(), state}));
  } catch (_) {
    // A full or disabled WebView storage area must not hide otherwise live data.
  }
}

function clearCachedState() {
  try { localStorage.removeItem(STATE_CACHE_KEY); } catch (_) {}
  latestState = null;
  lastSuccess = null;
  accountsNode.innerHTML = '<div class="empty">This phone is no longer paired.</div>';
}

function currentServerTime() {
  if (!latestState) return Math.floor(Date.now() / 1000);
  return latestState.server_time + (lastSuccess ? Math.max(0, Math.floor((Date.now() - lastSuccess) / 1000)) : 0);
}

function lastUpdateText(prefix) {
  if (!lastSuccess) return prefix;
  const elapsed = Math.max(1, Math.round((Date.now() - lastSuccess) / 1000));
  if (elapsed < 60) return `${prefix} ${elapsed}s ago`;
  const minutes = Math.round(elapsed / 60);
  if (minutes < 60) return `${prefix} ${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${prefix} ${hours}h ago`;
  return `${prefix} ${Math.round(hours / 24)}d ago`;
}

function colorValue(value) {
  if (/^#[0-9a-f]{6}$/i.test(value || "")) return value;
  return ({red:"#ef4444",orange:"#f97316",yellow:"#eab308",green:"#22c55e",cyan:"#27bfce",blue:"#3b82f6",purple:"#8c6dd8",pink:"#ec4899",gray:"#9ca3af",black:"#141414"})[value] || "#27bfce";
}

function relativeLuminance(color) {
  const channels = color.slice(1).match(/.{2}/g).map(value => parseInt(value, 16) / 255);
  const linear = channels.map(value => value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4);
  return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
}

function visibleUsageColor(value) {
  const color = colorValue(value);
  if (relativeLuminance(color) >= 0.24) return color;
  const source = color.slice(1).match(/.{2}/g).map(channel => parseInt(channel, 16));
  let lower = 0;
  let upper = 1;
  let result = source;
  for (let step = 0; step < 10; step += 1) {
    const blend = (lower + upper) / 2;
    result = source.map(channel => Math.round(channel + (255 - channel) * blend));
    const candidate = `#${result.map(channel => channel.toString(16).padStart(2, "0")).join("")}`;
    if (relativeLuminance(candidate) < 0.24) lower = blend;
    else upper = blend;
  }
  result = source.map(channel => Math.round(channel + (255 - channel) * upper));
  return `#${result.map(channel => channel.toString(16).padStart(2, "0")).join("")}`;
}

function sortedWindows(account) {
  const windows = [...account.windows].sort((a, b) => (a.duration_mins ?? Number.MAX_SAFE_INTEGER) - (b.duration_mins ?? Number.MAX_SAFE_INTEGER));
  if (!windows.length) return windows;
  const priority = window => window.label === "Monthly" ? 4 : window.label === "Weekly" ? 3 : 0;
  let primary = -1;
  for (let index = 0; index < windows.length; index += 1) {
    if (primary < 0 || priority(windows[index]) > priority(windows[primary]) ||
        (priority(windows[index]) === priority(windows[primary]) && priority(windows[index]) > 0 && (windows[index].duration_mins || 0) >= (windows[primary].duration_mins || 0))) primary = index;
  }
  if (primary < 0 || priority(windows[primary]) === 0) {
    const timed = windows.map((window, index) => [window, index]).filter(([window]) => window.duration_mins != null);
    primary = timed.length ? timed.reduce((best, current) => current[0].duration_mins >= best[0].duration_mins ? current : best)[1]
      : windows.reduce((best, window, index) => window.used_percent >= windows[best].used_percent ? index : best, 0);
  }
  if (primary >= 0) windows.push(windows.splice(primary, 1)[0]);
  return windows;
}

function windowLabel(window, long) {
  if (window?.label) return window.label;
  const minutes = window?.duration_mins;
  if (minutes === 10080) return "Weekly";
  if (minutes === 300) return "5-hour";
  if (minutes && minutes % 1440 === 0) return `${minutes / 1440}-day`;
  if (minutes && minutes % 60 === 0) return `${minutes / 60}-hour`;
  if (minutes) return `${minutes}-min`;
  return long ? "Usage" : "Short";
}

function remaining(window) { return Math.max(0, Math.min(1, (100 - window.used_percent) / 100)); }
function percent(window) { return `${Math.round(remaining(window) * 100)}%`; }

function countdown(timestamp, now) {
  let seconds = Math.max(0, timestamp - now);
  if (!seconds) return "0m";
  const minutes = Math.ceil(seconds / 60);
  const hours = Math.ceil(seconds / 3600);
  if (hours >= 24) return `${Math.floor(hours / 24)}d ${hours % 24}h`;
  if (minutes >= 60) return `${Math.floor(minutes / 60)}h ${String(minutes % 60).padStart(2, "0")}m`;
  return `${minutes}m`;
}

function timerColor(window, now) {
  const left = Math.max(0, (window.resets_at || now) - now);
  const maximum = (window.duration_mins ?? 10080) * 60;
  const minimum = maximum / 10;
  const redWeight = Math.max(0, Math.min(1, (left - minimum) / (maximum - minimum)));
  return `hsl(${120 * (1 - redWeight)} 78% 58%)`;
}

function barColor(account, window, accountCount) {
  if (latestState.usage_bar_color_mode === "remaining") return `hsl(${120 * remaining(window)} 78% 52%)`;
  if (latestState.usage_bar_color_mode === "custom") return colorValue(latestState.usage_bar_custom_color);
  return accountCount <= 1 ? "#ffffff" : visibleUsageColor(account.color);
}

function limitHtml(account, window, showResetCounter, accountCount) {
  const showReset = showResetCounter && window.resets_at;
  const now = currentServerTime();
  const reset = showReset ? `<span class="reset-text"> • resets in <span class="reset-timer ${latestState.color_reset_timers ? "colored" : ""}" style="--timer-color:${timerColor(window, now)}">${countdown(window.resets_at, now)}</span></span>` : "";
  return `<div class="limit"><span class="limit-label">${escapeHtml(windowLabel(window, false))}</span>${reset}<span class="bar"><span class="bar-fill" style="--remaining:${remaining(window)};--bar-color:${barColor(account, window, accountCount)}"></span></span><span class="percent">${percent(window)}</span></div>`;
}

function expiryText(credit) {
  if (!credit.expires_at) return credit.description || "";
  const date = new Date(credit.expires_at * 1000);
  const dateText = date.toLocaleDateString(undefined, {month:"short", day:"numeric"});
  const timeText = date.toLocaleTimeString(undefined, {hour:"numeric", minute:"2-digit"});
  const days = Math.max(0, Math.ceil((credit.expires_at - currentServerTime()) / 86400));
  const dayText = days === 1 ? " · 1 day" : days > 1 ? ` · ${days} days` : "";
  return `Expires ${dateText} at ${timeText}${dayText}`;
}

function accountHtml(account, accountCount) {
  const windows = sortedWindows(account);
  const shortWindow = windows.length >= 2 ? windows[0] : null;
  const longWindow = windows.at(-1) || null;
  const isExpanded = expandedOverrides.has(account.key) ? expandedOverrides.get(account.key) : account.expanded;
  const alwaysShowResetCounter = latestState.always_show_reset_counter === true;
  const showBankedResets = latestState.show_banked_resets !== false;
  const resetCountValue = Math.max(account.reset_available_count || 0, account.reset_credits.length);
  const pinShort = account.pin_short && !!shortWindow;
  const detailWindows = windows.slice(0, -1).filter((_, index) => !(pinShort && index === 0));
  const hasDetails = detailWindows.length > 0 || !!longWindow?.resets_at || (showBankedResets && resetCountValue > 0);
  const shownName = latestState.blur_names && !revealedNames.has(account.key) ? account.masked_display_name : account.display_name;
  const shownEmail = latestState.blur_emails && !revealedEmails.has(account.key) ? account.masked_email : account.email;
  const mainLimit = longWindow ? limitHtml(account, longWindow, isExpanded || alwaysShowResetCounter, accountCount) : `<div class="checking">${escapeHtml(account.error || "Checking usage…")}</div>`;
  const pinned = pinShort ? limitHtml(account, shortWindow, isExpanded || alwaysShowResetCounter, accountCount) : "";
  const detailLimits = isExpanded ? detailWindows.map(window => limitHtml(account, window, true, accountCount)).join("") : "";
  const resetCount = isExpanded && showBankedResets && resetCountValue ? `<div class="detail-line"><span>Reset credits</span><span>${resetCountValue} available</span></div>` : "";
  const credits = isExpanded && showBankedResets ? account.reset_credits.map(credit => `<div class="credit"><div class="credit-title">${escapeHtml(credit.title)}</div><div class="credit-expiry">${escapeHtml(expiryText(credit))}</div></div>`).join("") : "";
  const error = longWindow && account.error ? `<div class="error">${escapeHtml(account.error)}</div>` : "";
  return `<article class="account" data-key="${account.key}" style="--account-color:${colorValue(account.color)}">
    <div class="account-head">
      <span class="provider-wrap"><img class="provider-mark" src="${endpoint(`provider-icons/${account.provider_id}`)}" alt="">${accountCount > 1 ? '<span class="accent-dot"></span>' : ""}</span>
      <button class="identity-button" data-action="name" type="button">${escapeHtml(shownName)}</button>
      <button class="email-button" data-action="email" type="button">${escapeHtml(shownEmail)}</button>
      ${hasDetails ? `<button class="details-button" data-action="details" type="button" aria-label="${isExpanded ? "Hide" : "Show"} account details" aria-expanded="${isExpanded}"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m6 9 6 6 6-6"></path></svg></button>` : ""}
    </div>
    ${mainLimit}${error}${pinned}<div class="details">${detailLimits}${resetCount}${credits}</div>
  </article>`;
}

function render() {
  if (!latestState) return;
  accountsNode.innerHTML = latestState.accounts.length ? latestState.accounts.map(account => accountHtml(account, latestState.accounts.length)).join("") : '<div class="empty">No enabled agent accounts were found.</div>';
  const busy = latestState.refreshing || locallyRefreshing;
  refreshButton.classList.toggle("busy", busy);
  refreshButton.disabled = busy;
  refreshButton.setAttribute("aria-label", busy ? "Refreshing usage" : "Refresh usage");
}

async function loadState({quiet = false} = {}) {
  if (requestInFlight) return;
  requestInFlight = true;
  try {
    const response = await fetch(endpoint("api/state"), {cache:"no-store", credentials:"same-origin"});
    if (response.status === 401 || response.status === 403) {
      clearCachedState();
      throw new Error("This phone is not paired. Open the private pairing link again.");
    }
    if (!response.ok) throw new Error(`Desktop returned ${response.status}.`);
    const state = await response.json();
    if (!validState(state)) throw new Error("Desktop returned an invalid usage update.");
    latestState = state;
    lastSuccess = Date.now();
    cacheState(latestState);
    if (!latestState.refreshing) locallyRefreshing = false;
    noticeNode.hidden = true;
    statusNode.className = "live-status online";
    statusNode.setAttribute("aria-label", "Connected to desktop");
    updatedNode.textContent = "Connected to desktop · updated just now";
    render();
  } catch (error) {
    statusNode.className = "live-status offline";
    statusNode.setAttribute("aria-label", "Desktop unavailable");
    noticeNode.textContent = error.message || "Desktop unavailable.";
    noticeNode.hidden = false;
    locallyRefreshing = false;
    refreshButton.classList.remove("busy");
    refreshButton.disabled = false;
    if (latestState) updatedNode.textContent = lastUpdateText("Desktop unavailable · last updated");
    if (!quiet && !latestState) accountsNode.innerHTML = '<div class="empty">The desktop companion is unavailable.</div>';
  } finally {
    requestInFlight = false;
  }
}

async function refreshUsage() {
  locallyRefreshing = true;
  render();
  try {
    const response = await fetch(endpoint("api/refresh"), {method:"POST", cache:"no-store", credentials:"same-origin"});
    if (response.status === 401 || response.status === 403) {
      clearCachedState();
      throw new Error("This phone is not paired. Open the private pairing link again.");
    }
    if (!response.ok) throw new Error(`Refresh failed (${response.status}).`);
    setTimeout(() => loadState(), 500);
    setTimeout(() => loadState(), 1600);
  } catch (error) {
    locallyRefreshing = false;
    noticeNode.textContent = error.message;
    noticeNode.hidden = false;
    render();
  }
}

async function refreshUsageIfStale() {
  try {
    await fetch(endpoint("api/refresh-if-stale"), {method:"POST", cache:"no-store", credentials:"same-origin"});
  } catch (_) {
    // The normal state poll owns connection messaging and endpoint failover.
  }
}

accountsNode.addEventListener("click", event => {
  const button = event.target.closest("button[data-action]");
  const accountNode = event.target.closest(".account");
  if (!button || !accountNode || !latestState) return;
  const key = Number(accountNode.dataset.key);
  const account = latestState.accounts.find(item => item.key === key);
  if (!account) return;
  if (button.dataset.action === "details") expandedOverrides.set(key, !(expandedOverrides.has(key) ? expandedOverrides.get(key) : account.expanded));
  if (button.dataset.action === "name" && latestState.blur_names) revealedNames.has(key) ? revealedNames.delete(key) : revealedNames.add(key);
  if (button.dataset.action === "email" && latestState.blur_emails) revealedEmails.has(key) ? revealedEmails.delete(key) : revealedEmails.add(key);
  render();
});

refreshButton.addEventListener("click", refreshUsage);
if (/AgentsUsageAndroid\//.test(navigator.userAgent)) {
  connectionsButton.hidden = false;
  connectionsButton.addEventListener("click", () => { window.location.href = "agents-usage://connections"; });
}
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "hidden") { resumedAt = Date.now(); return; }
  loadState();
  if (Date.now() - resumedAt > 30000) refreshUsageIfStale();
});

setInterval(() => {
  if (!lastSuccess) return;
  if (statusNode.classList.contains("offline")) updatedNode.textContent = lastUpdateText("Desktop unavailable · last updated");
  else updatedNode.textContent = lastUpdateText("Connected to desktop · checked");
}, 1000);
setInterval(() => { if (document.visibilityState === "visible") loadState({quiet:true}); }, 10000);

if (restoreCachedState()) {
  statusNode.className = "live-status offline";
  statusNode.setAttribute("aria-label", "Reconnecting to desktop");
  updatedNode.textContent = lastUpdateText("Last desktop update");
  render();
}
loadState().then(refreshUsageIfStale);
if ("serviceWorker" in navigator && window.isSecureContext) navigator.serviceWorker.register("./sw.js");
