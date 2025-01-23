/* This file is licensed under AGPL-3.0 */
const filterElement = document.getElementById('search-files');
const escapedRegex = /[-\/\\^$*+?.()|[\]{}]/g;
const escapeRegex = (e) => e.replace(escapedRegex, '\\$&');
const MIN_SCORE = -1500;

function __score(haystack, query) {
  let result = fuzzysort.single(__normalizeString(query), __normalizeString(haystack));
  return result?.score == null ? MIN_SCORE : result.score;
}

function __normalizeString(s) {
  return s ? s.normalize('NFKD').replace(/[\u0300-\u036f]/g, "") : s;
}

const changeModifiedToRelative = () => {
  document.querySelectorAll('.file-uploaded').forEach(node => {
    let uploadedAt = node.parentElement.dataset.uploadedAt;

    if (/^[0-9]+$/.test(uploadedAt)) {
      // If it's a numeric timestamp, parse it as seconds
      const seconds = parseInt(uploadedAt, 10);
      node.textContent = formatRelative(seconds);
    } else {
      // Try parsing it as a date string
      const date = Date.parse(uploadedAt);

      if (!isNaN(date)) {
        // Update dataset to store timestamp and format it
        node.parentElement.dataset.uploadedAt = date;
        node.textContent = formatRelative(date / 1000);
      } else {
        // Handle invalid date strings gracefully
        console.warn(`Invalid date: ${uploadedAt}`);
      }
    }
  });
};

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

class TableSorter {
  constructor(parent) {
    this.parent = parent;
    this.parent?.querySelectorAll('.table-header[data-sort-by]').forEach(el => {
      el.addEventListener('click', e => this.sortBy(e, el.dataset.sortBy))
    });
    if(this.parent) {
      let isAscending = initialSortOrder.value === 'ascending';
      this.innerSortBy(`data-${initialSortBy.value}`, isAscending);
      const headers = Array.from(this.parent.querySelectorAll('.table-headers > .table-header'));
      headers.forEach(node => node.classList.remove('sorting-ascending', 'sorting-descending'));
      let element = headers.find(e => e.dataset.sortBy === initialSortBy.value);
      if(element != null) {
        element.classList.add(isAscending ? 'sorting-ascending' : 'sorting-descending');
      }
    }
  }

  innerSortBy(attribute, ascending) {
    let entries = [...this.parent.querySelectorAll('.entry')];
    if (entries.length === 0) {
      return;
    }
    let parent = entries[0].parentElement;
    entries.sort((a, b) => {
      if (attribute === 'data-name') {
        let firstName = a.textContent;
        let secondName = b.textContent;
        return ascending ? firstName.localeCompare(secondName) : secondName.localeCompare(firstName);
      } else {
        // The last two remaining sort options are either e.g. file.size or entry.last_modified
        // Both of these are numbers so they're simple to compare
        let first = parseInt(a.getAttribute(attribute), 10);
        let second = parseInt(b.getAttribute(attribute), 10);
        return ascending ? first - second : second - first;
      }
    });

    entries.forEach(obj => parent.appendChild(obj));
  }

  sortBy(event, attribute) {
    // Check if the element has an descending class tag
    // If it does, then when we're clicking on it we actually want to sort ascending
    let descending = !event.target.classList.contains('sorting-descending');

    // Make sure to toggle everything else off...
    this.parent.querySelectorAll('.table-headers > .table-header').forEach(node => node.classList.remove('sorting-ascending', 'sorting-descending'));

    // Sort the elements by what we requested
    this.innerSortBy(`data-${attribute}`, !descending);

    // Add the element class list depending on the operation we did
    let className = descending ? 'sorting-descending' : 'sorting-ascending';
    event.target.classList.add(className);
  }
}

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
  return __score(el.dataset.name, query);
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

let sorter = new TableSorter(document.querySelector('.files'));
document.getElementById('clear-search-filter')?.addEventListener('click', resetSearchFilter);
filterElement?.addEventListener('input', debounced(e => filterEntries(e.target.value)))
