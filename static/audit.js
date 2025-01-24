/* This file is licensed under AGPL-3.0 */

const loadingElement = document.getElementById('loading');
const loadMore = document.getElementById('load-more');
const auditLogEntries = document.getElementById('audit-log-entries');
const dtFormat = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'full',
  timeStyle: 'medium',
});

const userLink = (account_id, info) => {
  let name = info.users[account_id];
  if (name) {
    return html('a', name, {href: `/user/${name}`});
  }
  return html('span.fallback', account_id != null ? `User ID ${account_id}` : 'Unknown user');
}

const imageLink = (image_id, info, fallback) => {
  let entry = info.entries[image_id];
  if (entry) {
    let id = entry.id || fallback;
    return html('a.entry-name', id, {
      href: `/gallery/${image_id}`,
      dataset: { id: entry.id }
    });
  }
  return html('span.fallback', fallback != null ? fallback : image_id);
}

const simplePlural = (c, s) => c === 1 ? `${c} ${s}` : `${c} ${s}s`;
const fileToElement = (op) => html('li.file', op.name, {class: op.failed ? 'failed' : 'success'});

function auditLogEntry(id, title, contents) {
  const isEmpty = (e) => e == null || (Array.isArray(e) && e.length === 0);
  return html('details.audit-log-entry',
      isEmpty(contents) ? {class: 'empty'} : {},
      html('summary',
          html('.description',
              html('span.title', title),
              html('span.date', formatRelative(Math.floor(id / 1000)), {title: dtFormat.format(new Date(id))})
          ),
      ),
      html('.content', contents)
  );
}

const auditLogTypes = Object.freeze({
  upload: (data, log, info) => {
    let title = [
      data.api ? "[API] " : "",
      userLink(log.account_id, info),
      " uploaded ",
      simplePlural(data.files.length, 'file'),
    ];
    let files = data.files.map(fileToElement);
    return auditLogEntry(log.id, title, html('ul', files));
  },
  delete_files: (data, log, info) => {
    let title = [
      userLink(log.account_id, info),
      " deleted ",
      simplePlural(data.files.length, 'image'),
    ];
    let contents = [];

    contents.push(html('ul', data.files.map(fileToElement)));
    return auditLogEntry(log.id, title, contents);
  },
  delete_image: (data, log, info) => {
    let title = [
      data.api ? "[API] " : "",
      userLink(log.account_id, info),
      " deleted image with id ",
      imageLink(log.image_id, info),
    ];

    return auditLogEntry(log.id, title);
  }
});


async function processData(info) {
  for (const log of info.logs) {
    let data = log.data;
    let parser = auditLogTypes[data.type];
    if (parser) {
      let node = parser(data, log, info);
      auditLogEntries.appendChild(node);
    }
  }

  loadMore.classList.remove("hidden");
  auditLogEntries.classList.remove('hidden');
  loadingElement.classList.add("hidden");
}

async function getAuditLogs(before) {
  loadMore.textContent = "Loading...";
  loadMore.disabled = true;

  let params = new URL(document.location).searchParams;
  if (before) params.append('before', before);
  let response = await fetch('/audit-logs?' + params);
  if (response.status !== 200) {
    showAlert({level: 'error', content: `Server responded with ${response.status}`});
    loadingElement.classList.add('hidden');
    return;
  }

  let data = await response.json();
  await processData(data);
  if (data.logs.length !== 100) {
    if (before) {
      loadMore.disabled = true;
      loadMore.textContent = "No more entries";
    } else {
      loadMore.classList.add('hidden');
      if (data.logs.length === 0) {
        auditLogEntries.appendChild(html('p', 'No entries!'));
      }
    }
  } else {
    loadMore.textContent = "Load more";
    loadMore.dataset.lastId = data.logs[data.logs.length - 1].id;
    loadMore.disabled = false;
  }
}

document.addEventListener('DOMContentLoaded', () => getAuditLogs());
loadMore.addEventListener('click', () => getAuditLogs(loadMore.dataset.lastId))
