/* This file is licensed under AGPL-3.0 */

const logSelect = document.getElementById('log-select');
// Statistics
const requestCount = document.getElementById('request-count');
const activeUsers = document.getElementById('active-users');
const averageResponseTime = document.getElementById('average-response-time');
const percentSuccess = document.getElementById('percent-success');
const registeredUsers = document.getElementById('registered-users');
// Tables
const referringSites = document.getElementById('referring-sites');
const popularRoutes = document.getElementById('popular-routes');
const recentLogs = document.getElementById('recent-logs');
const popularApiRoutes = document.getElementById('popular-api-routes');
const popularApiUsers = document.getElementById('popular-api-users');

let baseUrl = `${window.location.protocol}//${window.location.host}`;
const styles = getComputedStyle(document.documentElement);
let logs = [];

const isMultiDay = () => logSelect.value.endsWith('-days');
const nonStaticHttpRequests = () => logs.filter(data => data.path.startsWith('/static/') === false);
const logToUrl = (data) => new URL(data.path, baseUrl);

async function getLogs(value) {
  let endpoint = '/admin/logs';
  if (value.endsWith('-days')) {
    endpoint = `/admin/logs?days=${value.substring(0, value.lastIndexOf('-'))}`;
  } else if (value.indexOf(';') !== -1) {
    const [begin, end] = value.split(';');
    endpoint = `/admin/logs?begin=${begin}&end=${end}`;
  }

  logs = await callApi(endpoint);
}

function clearTable(table) {
  table.querySelector('tbody').innerHTML = '';
}

function getActiveUsers(requests) {
  let unique = new Set(requests.map(data => data.user_id).filter(e => typeof e == 'number'));
  return unique.size;
}

const stripPrefix = (s, prefix) => s.startsWith(prefix) ? s.slice(prefix.length) : s;

function getAverageResponseTime(requests) {
  let responseTimes = requests.map(data => data.latency * 1000.0);
  return Math.round(responseTimes.reduce((a, b) => a + b, 0) / responseTimes.length);
}

const isSpanSuccess = (data) => {
  let code = data.status_code;
  return code >= 200 && code < 400;
}

const spanToUserData = (data) => {
  return { user_id: data.user_id, success: isSpanSuccess(data), url: logToUrl(data) };
}

function getSuccessRate(requests) {
  let successes = requests.reduce((a, data) => a + isSpanSuccess(data), 0);
  return successes / requests.length;
}

function getAppropriateTimeScales() {
  let unit = isMultiDay() ? 'day' : 'hour';
  return {
    x: {
      type: 'time',
      time: {
        unit,
        tooltipFormat: 'DD T',
      },
      title: {
        display: true,
        text: unit === 'day' ? 'Date' : 'Time',
      }
    },
    y: {
      title: {
        display: true,
        text: 'Total', // to override
      },
      ticks: {
        precision: 0,
      }
    }
  };
}

function getSearchEngine(url) {
  try {
    if (url.host.startsWith('google')) {
      return 'Google';
    } else if (url.host === 'bing.com') {
      return 'Bing';
    } else if (url.host === 'duckduckgo.com') {
      return 'DuckDuckGo';
    }
    return null;
  }
  catch (e) {
    return null;
  }
}

function getReferringSites(requests) {
  let counter = requests.map(d => d.referrer || "").filter(r => r.indexOf(window.location.hostname) === -1 && r.length !== 0).reduce((count, referrer) => {
    if (count.hasOwnProperty(referrer)) {
      count[referrer] += 1;
    } else {
      count[referrer] = 1;
    }
    return count;
  }, {});

  let tbody = referringSites.querySelector('tbody');
  tbody.innerHTML = '';
  for (const [referrer, count] of Object.entries(counter).sort(([, a], [, b]) => b - a).slice(0, 25)) {
    let tr = document.createElement('tr');
    let f = document.createElement('td');
    f.setAttribute('data-th', 'Site')
    if (referrer.startsWith('http')) {
      let url = new URL(referrer);
      let searchEngine = getSearchEngine(url);
      if (searchEngine === null) {
        let a = document.createElement('a');
        a.href = referrer;
        a.textContent = url.host;
        f.appendChild(a);
      } else {
        f.textContent = searchEngine;
      }
    } else {
      f.textContent = referrer;
    }
    let c = document.createElement('td');
    c.setAttribute('data-th', 'Views');
    c.className = 'numeric';
    c.textContent = count.toLocaleString();
    tr.appendChild(f);
    tr.appendChild(c);
    tbody.appendChild(tr);
  }
}

function getPopularRoutes(requests) {
  let counter = requests.map(logToUrl).filter(url => url !== null).reduce((count, url) => {
    route = url.pathname;
    if (count.hasOwnProperty(route)) {
      count[route] += 1;
    } else {
      count[route] = 1;
    }
    return count;
  }, {});

  let tbody = popularRoutes.querySelector('tbody');
  tbody.innerHTML = '';
  for (const [route, count] of Object.entries(counter).sort(([, a], [, b]) => b - a).slice(0, 25)) {
    let tr = document.createElement('tr');
    let f = document.createElement('td');
    f.setAttribute('data-th', 'Route')
    let a = document.createElement('a');
    a.href = route;
    a.textContent = route;
    f.appendChild(a);
    let c = document.createElement('td');
    c.setAttribute('data-th', 'Views');
    c.className = 'numeric';
    c.textContent = count.toLocaleString();
    tr.appendChild(f);
    tr.appendChild(c);
    tbody.appendChild(tr);
  }
}

function getPopularApiRoutes(requests) {
  let counter = requests.map(logToUrl).filter(url => url !== null && url.pathname.startsWith('/api/')).reduce((count, url) => {
    route = url.pathname;
    if (count.hasOwnProperty(route)) {
      count[route] += 1;
    } else {
      count[route] = 1;
    }
    return count;
  }, {});

  let tbody = popularApiRoutes.querySelector('tbody');
  tbody.innerHTML = '';
  for (const [route, count] of Object.entries(counter).sort(([, a], [, b]) => b - a).slice(0, 25)) {
    tbody.appendChild(html('tr',
      html('td', route, { dataset: { th: 'Route' } }),
      html('td', count.toLocaleString(), { dataset: { th: 'Calls' }, className: 'numeric' })
    ));
  }
}

function getTopApiUsers(requests) {
  let counter = requests.map(spanToUserData)
    .filter(data => data.user_id != null && data.url !== null && data.url.pathname.startsWith('/api/'))
    .reduce((count, data) => {
      let key = data.user_id;
      if (count.hasOwnProperty(key)) {
        let subkey = data.success ? 'success' : 'failed';
        count[key][subkey] += 1;
      } else {
        count[key] = { success: data.success, failed: !data.success };
      }
      return count;
    }, {});

  let tbody = popularApiUsers.querySelector('tbody');
  tbody.innerHTML = '';
  for (const [user_id, counts] of Object.entries(counter).sort(([, a], [, b]) => (b.success + b.failed) - (a.success + a.failed)).slice(0, 25)) {
    tbody.appendChild(html('tr',
      html('td', html('a', user_id, { href: `/admin/user/${user_id}` }), { dataset: { th: 'User ID' } }),
      html('td', counts.success + counts.failed, { dataset: { th: 'Total' }, className: 'numeric' }),
      html('td', counts.success, { dataset: { th: 'Success' }, className: 'numeric' }),
      html('td', counts.failed || '0', { dataset: { th: 'Failed' }, className: 'numeric' }),
    ));
  }
}

// ── Recent admin activity ────────────────────────────────────────────
// Fetches the last 10 /admin/audit entries and renders them into the
// dashboard's #recent-audit table. Mirrors the cell layout in
// static/admin_audit.js so the action-pill classes from admin_audit.css
// (auth, ssh, docker, …) reuse without duplication.

function dashAuditEscape(s) {
  if (s == null) return "";
  return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function dashAuditFmtRelative(iso) {
  if (!iso) return "—";
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return "—";
  const diff = Math.max(0, Math.floor((Date.now() - t) / 1000));
  if (diff < 60)    return diff + "s ago";
  if (diff < 3600)  return Math.floor(diff / 60) + "m ago";
  if (diff < 86400) return Math.floor(diff / 3600) + "h ago";
  return Math.floor(diff / 86400) + "d ago";
}

// Same prefix → class map used on /admin/audit so the pill colours
// match. Kept duplicated (rather than importing) because admin.js
// runs on the dashboard and admin_audit.js on the audit page — they
// never co-load.
function dashAuditActionClass(action) {
  if (!action) return "";
  if (action.startsWith("auth."))     return "auth";
  if (action.startsWith("ssh."))      return "ssh";
  if (action.startsWith("docker."))   return "docker";
  if (action.startsWith("admin."))    return "admin";
  if (action.startsWith("image."))    return "image";
  if (action.startsWith("invite."))   return "invite";
  if (action.startsWith("secret."))   return "secret";
  if (action.startsWith("postgres.")) return "postgres";
  if (action.startsWith("sanitizer."))return "sanitizer";
  return "";
}

async function getRecentAuditEvents() {
  const tbody = document.querySelector("#recent-audit tbody");
  let data;
  try {
    const res = await fetch("/admin/audit/data?limit=10");
    if (!res.ok) {
      tbody.innerHTML = `<tr><td colspan="5" class="muted">Failed to load — HTTP ${res.status}.</td></tr>`;
      return;
    }
    data = await res.json();
  } catch (e) {
    tbody.innerHTML = `<tr><td colspan="5" class="muted">Failed to load — ${dashAuditEscape(e.message || String(e))}.</td></tr>`;
    return;
  }
  const entries = (data && data.entries) || [];
  if (entries.length === 0) {
    tbody.innerHTML = `<tr><td colspan="5" class="muted">No audit events yet.</td></tr>`;
    return;
  }
  tbody.innerHTML = entries.map((r) => {
    const cls = dashAuditActionClass(r.action);
    const target = r.target ? `<code>${dashAuditEscape(r.target)}</code>` : "";
    return `<tr>
      <td data-th="When"><span class="audit-when" title="${dashAuditEscape(r.ts)}">${dashAuditFmtRelative(r.ts)}</span></td>
      <td data-th="Actor">${dashAuditEscape(r.actor_label)}</td>
      <td data-th="Action"><a href="/admin/audit?action=${encodeURIComponent(r.action)}"><span class="action-pill ${cls}">${dashAuditEscape(r.action)}</span></a></td>
      <td data-th="Target">${target}</td>
      <td data-th="IP"><code>${dashAuditEscape(r.ip || "")}</code></td>
    </tr>`;
  }).join("");
}

async function getRecentServerLogs() {
  const formatValue = (x) => typeof x === 'string' ? JSON.stringify(x) : x.toString();

  let serverLogs = await callApi('/admin/logs/server');
  let filtered = serverLogs.reverse().slice(0, 25);
  let tbody = recentLogs.querySelector('tbody');
  tbody.innerHTML = '';
  for (const log of filtered) {
    let tr = document.createElement('tr');
    let ts = document.createElement('td');
    ts.setAttribute('data-th', 'Timestamp');
    ts.setAttribute('title', log.timestamp);
    ts.textContent = formatRelative(Math.floor(Date.parse(log.timestamp) / 1000));
    let level = document.createElement('td');
    level.setAttribute('data-th', 'Level');
    level.textContent = log.level;
    level.classList.add(log.level.toLowerCase());
    let target = document.createElement('td');
    target.setAttribute('data-th', 'Target');
    target.textContent = log.target;
    let message = document.createElement('td');
    message.setAttribute('data-th', 'Message');
    message.textContent = log.fields?.message ?? "Nothing";
    let fields = document.createElement('td');
    fields.setAttribute('data-th', 'Fields');
    fields.textContent = Object.entries(log.fields).filter(([name, _]) => name !== 'message').map(([name, value]) => `${name}=${formatValue(value)}`).join(", ");
    tr.appendChild(ts);
    tr.appendChild(level);
    tr.appendChild(target);
    tr.appendChild(message);
    tr.appendChild(fields);
    tbody.appendChild(tr);
  }
}

function updateGraphs() {
  let requests = nonStaticHttpRequests();
  requestCount.textContent = requests.length.toLocaleString();
  activeUsers.textContent = getActiveUsers(requests);
  averageResponseTime.textContent = `${getAverageResponseTime(requests)} ms`;
  percentSuccess.textContent = getSuccessRate(requests).toLocaleString(undefined, { style: 'percent', minimumFractionDigits: 2 });
  getReferringSites(requests);
  getPopularRoutes(requests);
  getPopularApiRoutes(requests);
  getTopApiUsers(requests);
  getRecentServerLogs();
  getRecentAuditEvents();
}

function backfillLogSearch() {
  const now = new Date();
  const formatter = new Intl.DateTimeFormat(undefined, {
    year: 'numeric',
    month: 'numeric',
    day: 'numeric',
  });
  // Prefill with last ~45 days
  for (let day = 0; day <= 45; ++day) {
    const logDate = new Date(now.valueOf());
    logDate.setDate(logDate.getDate() - day);
    let el = document.createElement('option');
    el.textContent = formatter.format(logDate);
    logDate.setHours(0, 0, 0, 0);
    let begin = logDate.getTime();
    logDate.setHours(23, 59, 59, 999);
    let end = logDate.getTime();
    el.value = `${begin};${end}`;
    logSelect.appendChild(el);
  }
}

backfillLogSearch();
logSelect.addEventListener('change', async () => {
  await getLogs(logSelect.value);
  updateGraphs();
});

getLogs(logSelect.value).then(updateGraphs);