"use strict";

const accountsNode = document.querySelector("#accounts");
const noticeNode = document.querySelector("#notice");
const refreshButton = document.querySelector("#refresh");
const statusNode = document.querySelector("#live-status");
const updatedNode = document.querySelector("#updated");
const expandedOverrides = new Map();
const revealedNames = new Set();
const revealedEmails = new Set();
let latestState = null;
let lastSuccess = 0;
let locallyRefreshing = false;
let requestInFlight = false;
let resumedAt = Date.now();

const openAiPath = "M9.205 8.658v-2.26c0-.19.072-.333.238-.428l4.543-2.616c.619-.357 1.356-.523 2.117-.523 2.854 0 4.662 2.212 4.662 4.566 0 .167 0 .357-.024.547l-4.71-2.759a.797.797 0 00-.856 0l-5.97 3.473zm10.609 8.8V12.06c0-.333-.143-.57-.429-.737l-5.97-3.473 1.95-1.118a.433.433 0 01.476 0l4.543 2.617c1.309.76 2.189 2.378 2.189 3.948 0 1.808-1.07 3.473-2.76 4.163zM7.802 12.703l-1.95-1.142c-.167-.095-.239-.238-.239-.428V5.899c0-2.545 1.95-4.472 4.591-4.472 1 0 1.927.333 2.712.928L8.23 5.067c-.285.166-.428.404-.428.737v6.898zM12 15.128l-2.795-1.57v-3.33L12 8.658l2.795 1.57v3.33L12 15.128zm1.796 7.23c-1 0-1.927-.332-2.712-.927l4.686-2.712c.285-.166.428-.404.428-.737v-6.898l1.974 1.142c.167.095.238.238.238.428v5.233c0 2.545-1.974 4.472-4.614 4.472zm-5.637-5.303l-4.544-2.617c-1.308-.761-2.188-2.378-2.188-3.948A4.482 4.482 0 014.21 6.327v5.423c0 .333.143.571.428.738l5.947 3.449-1.95 1.118a.432.432 0 01-.476 0zm-.262 3.9c-2.688 0-4.662-2.021-4.662-4.519 0-.19.024-.38.047-.57l4.686 2.71c.286.167.571.167.856 0l5.97-3.448v2.26c0 .19-.07.333-.237.428l-4.543 2.616c-.619.357-1.356.523-2.117.523zm5.899 2.83a5.947 5.947 0 005.827-4.756C22.287 18.339 24 15.84 24 13.296c0-1.665-.713-3.282-1.998-4.448.119-.5.19-.999.19-1.498 0-3.401-2.759-5.947-5.946-5.947-.642 0-1.26.095-1.88.31A5.962 5.962 0 0010.205 0a5.947 5.947 0 00-5.827 4.757C1.713 5.447 0 7.945 0 10.49c0 1.666.713 3.283 1.998 4.448-.119.5-.19 1-.19 1.499 0 3.401 2.759 5.947 5.946 5.947.642 0 1.26-.095 1.88-.309a5.96 5.96 0 004.162 1.713z";

function endpoint(path) { return new URL(`./${path}`, window.location.href); }
function escapeHtml(value) { return String(value ?? "").replace(/[&<>"']/g, character => ({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"})[character]); }

function colorValue(value) {
  if (/^#[0-9a-f]{6}$/i.test(value || "")) return value;
  return ({red:"#ef4444",orange:"#f97316",yellow:"#eab308",green:"#22c55e",cyan:"#27bfce",blue:"#3b82f6",purple:"#8c6dd8",pink:"#ec4899",gray:"#9ca3af",black:"#141414"})[value] || "#27bfce";
}

function sortedWindows(account) {
  return [...account.windows].sort((a, b) => (a.duration_mins ?? Number.MAX_SAFE_INTEGER) - (b.duration_mins ?? Number.MAX_SAFE_INTEGER));
}

function windowLabel(window, long) {
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
  const short = (window.duration_mins ?? 10080) <= 300;
  const minimum = short ? 1800 : 43200;
  const maximum = short ? 18000 : 604800;
  const redWeight = Math.max(0, Math.min(1, (left - minimum) / (maximum - minimum)));
  return `hsl(${120 * (1 - redWeight)} 78% 58%)`;
}

function barColor(account, window, accountCount) {
  if (latestState.usage_bar_color_mode === "remaining") return `hsl(${120 * remaining(window)} 78% 52%)`;
  if (latestState.usage_bar_color_mode === "custom") return colorValue(latestState.usage_bar_custom_color);
  return accountCount <= 1 ? "#ffffff" : colorValue(account.color);
}

function limitHtml(account, window, isExpanded, accountCount) {
  const showReset = isExpanded && window.resets_at;
  const reset = showReset ? `<span class="reset-text ${latestState.color_reset_timers ? "colored" : ""}" style="--timer-color:${timerColor(window, latestState.server_time)}">• resets in ${countdown(window.resets_at, latestState.server_time)}</span>` : "";
  return `<div class="limit"><span class="limit-label">${escapeHtml(windowLabel(window, false))}</span>${reset}<span class="bar"><span class="bar-fill" style="--remaining:${remaining(window)};--bar-color:${barColor(account, window, accountCount)}"></span></span><span class="percent">${percent(window)}</span></div>`;
}

function expiryText(credit) {
  if (!credit.expires_at) return credit.description || "";
  const date = new Date(credit.expires_at * 1000);
  const dateText = date.toLocaleDateString(undefined, {month:"short", day:"numeric"});
  const days = Math.max(0, Math.ceil((credit.expires_at - latestState.server_time) / 86400));
  return `Expires ${dateText}${days ? ` · ${days} days` : ""}`;
}

function accountHtml(account, accountCount) {
  const windows = sortedWindows(account);
  const shortWindow = windows.length >= 2 ? windows[0] : null;
  const longWindow = windows.at(-1) || null;
  const isExpanded = expandedOverrides.has(account.key) ? expandedOverrides.get(account.key) : account.expanded;
  const pinShort = (latestState.pin_short_global || account.pin_short) && !!shortWindow;
  const hasDetails = !!shortWindow || !!longWindow?.resets_at || !!shortWindow?.resets_at || account.reset_available_count > 0;
  const shownName = latestState.blur_names && !revealedNames.has(account.key) ? account.masked_display_name : account.display_name;
  const shownEmail = latestState.blur_emails && !revealedEmails.has(account.key) ? account.masked_email : account.email;
  const mainLimit = longWindow ? limitHtml(account, {...longWindow, duration_mins: longWindow.duration_mins}, isExpanded, accountCount).replace(windowLabel(longWindow, false), windowLabel(longWindow, true)) : `<div class="checking">${escapeHtml(account.error || "Checking usage…")}</div>`;
  const pinned = pinShort ? limitHtml(account, shortWindow, isExpanded, accountCount) : "";
  const hiddenShort = isExpanded && shortWindow && !pinShort ? limitHtml(account, shortWindow, true, accountCount) : "";
  const resetCount = isExpanded && account.reset_available_count ? `<div class="detail-line"><span>Reset credits</span><span>${account.reset_available_count} available</span></div>` : "";
  const credits = isExpanded ? account.reset_credits.map(credit => `<div class="credit"><div class="credit-title">${escapeHtml(credit.title)}</div><div class="credit-expiry">${escapeHtml(expiryText(credit))}</div></div>`).join("") : "";
  const error = longWindow && account.error ? `<div class="error">${escapeHtml(account.error)}</div>` : "";
  return `<article class="account" data-key="${account.key}" style="--account-color:${colorValue(account.color)}">
    <div class="account-head">
      <span class="provider-wrap"><span class="provider-mark"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="${openAiPath}"></path></svg></span>${accountCount > 1 ? '<span class="accent-dot"></span>' : ""}</span>
      <button class="identity-button" data-action="name" type="button">${escapeHtml(shownName)}</button>
      <button class="email-button" data-action="email" type="button">${escapeHtml(shownEmail)}</button>
      ${hasDetails ? `<button class="details-button" data-action="details" type="button" aria-label="${isExpanded ? "Hide" : "Show"} account details" aria-expanded="${isExpanded}"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m6 9 6 6 6-6"></path></svg></button>` : ""}
    </div>
    ${mainLimit}${error}${pinned}<div class="details">${hiddenShort}${resetCount}${credits}</div>
  </article>`;
}

function render() {
  if (!latestState) return;
  accountsNode.innerHTML = latestState.accounts.length ? latestState.accounts.map(account => accountHtml(account, latestState.accounts.length)).join("") : '<div class="empty">No enabled Codex accounts were found.</div>';
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
    if (response.status === 401) throw new Error("This phone is not paired. Open the private pairing link again.");
    if (!response.ok) throw new Error(`Desktop returned ${response.status}.`);
    latestState = await response.json();
    lastSuccess = Date.now();
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
    if (response.status === 401) throw new Error("This phone is not paired. Open the private pairing link again.");
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
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "hidden") { resumedAt = Date.now(); return; }
  loadState();
  if (Date.now() - resumedAt > 30000) refreshUsage();
});

setInterval(() => {
  if (lastSuccess) updatedNode.textContent = `Connected to desktop · checked ${Math.max(1, Math.round((Date.now() - lastSuccess) / 1000))}s ago`;
}, 1000);
setInterval(() => { if (document.visibilityState === "visible") loadState({quiet:true}); }, 10000);

loadState().then(refreshUsage);
if ("serviceWorker" in navigator && window.isSecureContext) navigator.serviceWorker.register("./sw.js");
