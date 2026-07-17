/* This file is licensed under AGPL-3.0 */
/* Behaviour for the admin-only /account/insights page.

   The server does all the aggregating (see src/site/account/insights.rs) — this
   only picks a range, renders what comes back, and never sees a raw request row.

   `callApi` and `html` come from base.js. */

(() => {
  const range = document.getElementById('insights-range');
  if (!range) return; // not on this page

  const tiles = {
    requests: document.getElementById('stat-requests'),
    users: document.getElementById('stat-users'),
    latency: document.getElementById('stat-latency'),
    success: document.getElementById('stat-success'),
  };
  const lists = {
    routes: document.getElementById('insights-routes'),
    referrers: document.getElementById('insights-referrers'),
    apiRoutes: document.getElementById('insights-api-routes'),
    apiConsumers: document.getElementById('insights-api-consumers'),
  };

  const num = (n) => n.toLocaleString();

  /* An empty list is a real answer ("nothing referred anyone today"), not a
     failure — say so in place rather than leaving the card blank. */
  function fill(list, rows, render, empty) {
    list.replaceChildren();
    if (!rows.length) {
      list.appendChild(html('div.record', html('span.record-sub', empty)));
      return;
    }
    rows.forEach((row) => list.appendChild(render(row)));
  }

  /* Shared by all three ranked lists: a label (linked when the server gave a
     usable href) on the left, a count on the right. */
  const rankedRow = (unit) => (row) =>
    html('div.record',
      html('div.record-main',
        row.href
          ? html('a.record-title', row.label, { href: row.href, rel: 'noopener' })
          : html('span.record-title', row.label)),
      html('span.record-time', `${num(row.count)} ${unit}${row.count === 1 ? '' : 's'}`));

  const consumerRow = (row) =>
    html('div.record',
      html('div.record-main',
        /* A consumer whose account was deleted keeps its counts but has no
           profile to link to. */
        row.name
          ? html('a.record-title', row.name, { href: `/user/${encodeURIComponent(row.name)}` })
          : html('span.record-title', `Deleted account #${row.user_id}`),
        html('span.record-sub', `${num(row.success)} ok · ${num(row.failed)} failed`)),
      html('span.record-time', `${num(row.total)} call${row.total === 1 ? '' : 's'}`));

  async function refresh() {
    const data = await callApi(`/account/insights/data?range=${encodeURIComponent(range.value)}`);
    if (!data) return; // callApi already surfaced the error

    const s = data.summary;
    tiles.requests.textContent = num(s.requests);
    tiles.users.textContent = num(s.active_users);
    tiles.latency.textContent = `${num(s.avg_latency_ms)} ms`;
    /* null means nothing was served in the window — a dash, not "0%", which
       would read as every request having failed. */
    tiles.success.textContent = s.success_rate === null
      ? '—'
      : s.success_rate.toLocaleString(undefined, { style: 'percent', minimumFractionDigits: 2 });

    fill(lists.routes, data.routes, rankedRow('view'), 'No traffic in this range.');
    fill(lists.referrers, data.referrers, rankedRow('view'), 'Nothing linked here in this range.');
    fill(lists.apiRoutes, data.api_routes, rankedRow('call'), 'No API calls in this range.');
    fill(lists.apiConsumers, data.api_consumers, consumerRow, 'No authenticated API calls in this range.');
  }

  range.addEventListener('change', refresh);
  refresh();
})();
