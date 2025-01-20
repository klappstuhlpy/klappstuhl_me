/* This file is licensed under AGPL-3.0 */
const filterElement = document.getElementById('search-files');
const MIN_SCORE = -1500;

function __score(haystack, query) {
  let result = fuzzysort.single(__normalizeString(query), __normalizeString(haystack));
  return result?.score == null ? MIN_SCORE : result.score;
}

function __normalizeString(s) {
  return s ? s.normalize('NFKD').replace(/[\u0300-\u036f]/g, "") : s;
}

const changeModifiedToRelative = () => {
  document.querySelectorAll('.file-modified').forEach(node => {
    let lastModified = node.parentElement.dataset.lastModified;
    if(/^[0-9]+$/.test(lastModified)) {
      const seconds = parseInt(lastModified, 10);
      node.textContent = formatRelative(seconds);
    } else{
      const date = Date.parse(lastModified);
      node.parentElement.dataset.lastModified = date;
      node.textContent = formatRelative(date / 1000);
    }
  });
}

const parseEntryObjects = () => {
  document.querySelectorAll('.entry[data-extra]').forEach(el => {
    const obj = JSON.parse(el.dataset.extra);
    for (const attr in obj) {
      if (obj[attr] === null) {
        continue;
      }
      el.setAttribute(`data-${attr.replaceAll('_', '-')}`, obj[attr]);
    }
    delete el.dataset.extra;
  });
};
let previousEntryOrder = null;

function resetSearchFilter() {
  if (filterElement.value.length === 0) {
    filterElement.focus();
  }

  let entries = [...document.querySelectorAll('.entry')];
  if (entries.length !== 0) {
    let parentNode = entries[0].parentNode;
    entries.forEach(e => e.classList.remove('hidden'));
    if (previousEntryOrder !== null) {
      previousEntryOrder.forEach(e => parentNode.appendChild(e));
      previousEntryOrder = null;
    }
  }

  filterElement.value = "";
  document.dispatchEvent(new CustomEvent('entries-filtered'));
}

function __scoreByName(el, query) {
  let total = __score(el.dataset.name, query);
  let native = el.dataset.japaneseName;
  if (native !== null) {
    total = Math.max(total, __score(native, query));
  }
  let english = el.dataset.englishName;
  if (english !== null) {
    total = Math.max(total, __score(english, query));
  }
  return total;
}

function filterEntries(query) {
  if (!query) {
    resetSearchFilter();
    return;
  }

  let entries = [...document.querySelectorAll('.entry')];
  // Save the previous file order so it can be reset when we're done filtering
  if (previousEntryOrder === null) {
    previousEntryOrder = entries;
  }

  if (entries.length === 0) {
    return;
  }

  let parentNode = entries[0].parentNode;
  let mapped = entries.map(e => {
    return {
      entry: e,
      score: __scoreByName(e, query),
    };
  })

  mapped.sort((a, b) => b.score - a.score).forEach(el => {
    el.entry.classList.toggle('hidden', el.score <= MIN_SCORE);
    parentNode.appendChild(el.entry);
  });

  document.dispatchEvent(new CustomEvent('entries-filtered'));
}

parseEntryObjects();
changeModifiedToRelative();

document.getElementById('clear-search-filter')?.addEventListener('click', resetSearchFilter);
filterElement?.addEventListener('input', debounced(e => filterEntries(e.target.value)))
