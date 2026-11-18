// Browse page: faceted sidebar.
//
// The facet panel (counts per rating, label, type, format, volume, tag,
// year, ...) for the current filter, and the clicks that turn a facet
// into a filter. Loaded by browse.html from /static/browse/facets.js
// after the calendar and map sections; exposes window.damFacets. Reaches
// the filter bar only through window.* (onFilterChange, toggleQueryTerm,
// hasQueryTerm, get/setRatingFilter, get/setLabelFilter,
// setVolumeFilterById, getVolumeFilterId, toggleFormatFilter,
// getFormatFilters, damTags), looked up when a facet is rendered or used.
// Every row is a toggle, and rows the filter currently selects are marked.
//
// Plain ES5, no template expressions.
window.damFacets = (function() {
    var sidebar = document.getElementById('facet-sidebar');
    var content = document.getElementById('facet-content');
    var fetchController = null;
    // The facet row the user last activated, with its distance from the top
    // of the sidebar's viewport: the next render scrolls it back to the
    // same place. Used once, then dropped.
    var anchor = null;
    // The facet counts last rendered: expanding or collapsing a tag
    // re-renders from these without asking the server again.
    var lastData = null;
    // Tag-tree state. A tag with children is open when the user expanded
    // it (`openTags`, remembered per browser) or when the filter selects it
    // or one of its descendants (so the next level is there to stroll
    // into) -- unless the user collapsed it since (`closedTags`, this page
    // only; dropped again when the tag is clicked as a filter).
    var openTags = loadOpenTags();
    var closedTags = {};

    function isOpen() {
        return document.documentElement.getAttribute('data-sidebar') === 'open';
    }

    function init() {
        var pref = localStorage.getItem('maki-sidebar');
        if (pref === 'open') {
            document.documentElement.setAttribute('data-sidebar', 'open');
            fetchFacets();
        }
        document.getElementById('sidebar-close').addEventListener('click', toggle);
        // Click-to-narrow on facet rows. Event delegation on the content
        // container so re-renders (which replace innerHTML) don't drop the
        // listener. Each facet type maps to a different filter-bar setter
        // for consistency with the existing widgets.
        content.addEventListener('click', handleFacetClick);
        content.addEventListener('keydown', function(e) {
            var row = e.target.closest && e.target.closest('[data-facet-type]');
            if (!row) return;
            // Keys used on a focused facet row stay in the sidebar: the
            // page's own handler would otherwise also open the lightbox,
            // toggle the focused card's selection or move the grid focus.
            if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                e.stopPropagation();
                handleFacetClick(e);
            } else if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') {
                e.preventDefault();
                e.stopPropagation();
                if (!row.hasAttribute('aria-expanded')) return;
                // Tree convention: right opens, left closes.
                var open = row.getAttribute('aria-expanded') === 'true';
                if (open !== (e.key === 'ArrowRight')) toggleTag(row);
            }
        });
        // Restore section collapse states
        restoreCollapseStates();
    }

    function handleFacetClick(e) {
        var row = e.target.closest('[data-facet-type]');
        if (!row) return;
        // The chevron expands or collapses; the rest of the row filters.
        if (e.target.closest('.facet-tag-toggle')) {
            toggleTag(row);
            return;
        }
        var type = row.getAttribute('data-facet-type');
        var value = row.getAttribute('data-facet-value');
        anchor = {
            type: type,
            value: value,
            offset: row.getBoundingClientRect().top - sidebar.getBoundingClientRect().top,
            focus: document.activeElement === row,
        };
        // Every row is a toggle: a click on a selected row takes that
        // filter out again (see isSelected for what counts as selected).
        var on = isSelected(type, value);
        switch (type) {
            case 'tag':
                delete closedTags[value];
                if (window.damTags && window.damTags.toggleTagAndSearch) {
                    window.damTags.toggleTagAndSearch(value);
                }
                break;
            case 'rating':
                if (window.setRatingFilter) {
                    // Star widget uses '+' for "≥N", but a row click sets
                    // the exact value. Empty string clears.
                    window.setRatingFilter(on ? '' : value);
                    if (window.onFilterChange) window.onFilterChange();
                }
                break;
            case 'label':
                if (window.setLabelFilter) {
                    window.setLabelFilter(on ? '' : value);
                    if (window.onFilterChange) window.onFilterChange();
                }
                break;
            case 'format':
                if (window.toggleFormatFilter) window.toggleFormatFilter(value);
                break;
            case 'volume':
                if (window.setVolumeFilterById) window.setVolumeFilterById(on ? '' : value);
                break;
            case 'year':
                if (window.toggleQueryTerm) window.toggleQueryTerm('date:' + value);
                break;
            case 'geotagged':
                if (window.toggleQueryTerm) window.toggleQueryTerm('geo:any');
                break;
        }
    }

    // Whether the filter bar currently filters by exactly this facet row.
    // Read from the filter bar's own state, the same state a click changes.
    // A broader filter that merely covers the row (rating "3+", a parent
    // tag including its descendants) does not select it.
    function isSelected(type, value) {
        switch (type) {
            case 'tag':
                return !!(window.damTags && window.damTags.includedTagNames) &&
                    window.damTags.includedTagNames().indexOf(value) !== -1;
            case 'rating':
                return !!window.getFilterRating && window.getFilterRating() === value;
            case 'label':
                return !!window.getFilterLabel && window.getFilterLabel() === value;
            case 'format':
                return !!window.getFormatFilters && window.getFormatFilters().indexOf(value) !== -1;
            case 'volume':
                return !!window.getVolumeFilterId && window.getVolumeFilterId() === value;
            case 'year':
                return !!window.hasQueryTerm && window.hasQueryTerm('date:' + value);
            case 'geotagged':
                return !!window.hasQueryTerm && window.hasQueryTerm('geo:any');
        }
        return false;
    }

    // Opening attributes shared by every facet row: type, value, the
    // selected state and a title saying what a click will do.
    function rowAttrs(type, value, addTitle, extraClass) {
        var on = isSelected(type, value);
        return '<div class="facet-item facet-clickable' + (extraClass ? ' ' + extraClass : '') +
            (on ? ' facet-selected' : '') + '" ' +
            'data-facet-type="' + type + '" ' +
            'data-facet-value="' + MAKI.escAttr(value) + '" ' +
            'role="button" tabindex="0" aria-pressed="' + (on ? 'true' : 'false') + '" ' +
            'title="' + MAKI.escAttr(on ? 'Selected — click to remove this filter' : addTitle) + '"';
    }

    function toggle() {
        if (isOpen()) {
            document.documentElement.removeAttribute('data-sidebar');
            localStorage.setItem('maki-sidebar', 'closed');
            updateToggleBtn(false);
        } else {
            document.documentElement.setAttribute('data-sidebar', 'open');
            localStorage.setItem('maki-sidebar', 'open');
            updateToggleBtn(true);
            fetchFacets();
        }
    }

    function updateToggleBtn(active) {
        var btn = document.querySelector('.sidebar-toggle-btn');
        if (btn) btn.classList.toggle('active', active);
    }

    function getFilterParams() {
        var params = {};
        var urlParams = new URLSearchParams(window.location.search);
        ['q', 'type', 'tag', 'format', 'volume', 'rating', 'label', 'collection', 'path', 'person', 'stacks', 'nodefault'].forEach(function(k) {
            var v = urlParams.get(k);
            if (v) params[k] = v;
        });
        return params;
    }

    function fetchFacets() {
        if (!isOpen()) return;
        if (fetchController) fetchController.abort();
        fetchController = new AbortController();
        var params = getFilterParams();
        var qs = Object.keys(params).map(function(k) { return k + '=' + encodeURIComponent(params[k]); }).join('&');
        // Keep the current list (dimmed) while the new counts load, so the
        // sidebar neither jumps to the top nor flashes on every refresh.
        if (content.querySelector('.facet-section')) {
            content.classList.add('facet-refreshing');
        } else {
            content.innerHTML = '<div class="facet-loading">Loading\u2026</div>';
        }
        MAKI.api.get('/api/facets?' + qs, { signal: fetchController.signal })
            .then(function(data) { render(data); })
            .catch(function(e) {
                if (e.name === 'AbortError') return;
                content.classList.remove('facet-refreshing');
                content.innerHTML = '<div class="facet-loading">Error loading facets</div>';
            });
    }

    var labelColors = MAKI.labelColors;

    // Build a tree from the catalog's flat (auto-expanded) tag list.
    // Input: [["event", 47], ["event|festival", 47], ["event|festival|H...|2024", 12], ...]
    // Output: nested {name, fullPath, count, children: []} ordered alphabetically
    // at each depth. Auto-expansion guarantees every parent path is present in
    // the input, so we can rely on lookup-or-create to wire children to parents.
    function buildTagTree(flat) {
        var root = { children: {}, _order: [] };
        flat.forEach(function(entry) {
            var path = entry[0];
            var count = entry[1];
            var parts = path.split('|');
            var node = root;
            var acc = '';
            for (var i = 0; i < parts.length; i++) {
                acc = i === 0 ? parts[i] : (acc + '|' + parts[i]);
                if (!node.children[parts[i]]) {
                    node.children[parts[i]] = {
                        name: parts[i],
                        fullPath: acc,
                        count: 0,
                        children: {},
                        _order: [],
                    };
                    node._order.push(parts[i]);
                }
                node = node.children[parts[i]];
            }
            node.count = count;
        });
        function sortRec(n) {
            n._order.sort(function(a, b) { return a.toLowerCase().localeCompare(b.toLowerCase()); });
            n._order.forEach(function(k) { sortRec(n.children[k]); });
        }
        sortRec(root);
        return root;
    }

    function loadOpenTags() {
        try { return JSON.parse(localStorage.getItem('maki-facet-tags-open') || '{}') || {}; }
        catch(e) { return {}; }
    }

    function saveOpenTags() {
        try { localStorage.setItem('maki-facet-tags-open', JSON.stringify(openTags)); } catch(e) {}
    }

    // Paths the filter opens by itself: every included tag and its ancestors.
    function autoOpenTags() {
        var auto = {};
        var names = (window.damTags && window.damTags.includedTagNames) ? window.damTags.includedTagNames() : [];
        names.forEach(function(name) {
            var parts = name.split('|');
            var acc = '';
            for (var i = 0; i < parts.length; i++) {
                acc = i === 0 ? parts[i] : (acc + '|' + parts[i]);
                auto[acc] = true;
            }
        });
        return auto;
    }

    // Expand or collapse the tag row and re-render from the counts in hand.
    function toggleTag(row) {
        var path = row.getAttribute('data-facet-value');
        var open = row.getAttribute('aria-expanded') === 'true';
        if (open) {
            delete openTags[path];
            closedTags[path] = true;
        } else {
            openTags[path] = 1;
            delete closedTags[path];
        }
        saveOpenTags();
        anchor = {
            type: 'tag',
            value: path,
            offset: row.getBoundingClientRect().top - sidebar.getBoundingClientRect().top,
            focus: document.activeElement === row,
        };
        if (lastData) render(lastData);
    }

    // Only the rows of open branches are rendered: a collapsed tag stands
    // for its whole subtree (its count already covers every descendant).
    function renderTagTree(root) {
        var h = '';
        var auto = autoOpenTags();
        function walk(node, depth) {
            node._order.forEach(function(k) {
                var c = node.children[k];
                var hasChildren = c._order.length > 0;
                var open = hasChildren && !closedTags[c.fullPath] && (!!openTags[c.fullPath] || !!auto[c.fullPath]);
                // A collapsed branch that hides a selected tag says so.
                var hidesSelected = hasChildren && !open && !!auto[c.fullPath] && !isSelected('tag', c.fullPath);
                h += rowAttrs('tag', c.fullPath, c.fullPath + ' — click to narrow',
                              'facet-tag-row' + (hidesSelected ? ' facet-hides-selected' : '')) +
                     (hasChildren ? ' aria-expanded="' + (open ? 'true' : 'false') + '"' : '') +
                     ' style="--depth: ' + depth + '">';
                h += hasChildren
                    ? '<span class="facet-tag-toggle" title="' +
                      (open ? 'Collapse' : (hidesSelected ? 'Expand — a selected tag is inside' : 'Expand')) + '"></span>'
                    : '<span class="facet-tag-toggle-spacer"></span>';
                h += '<span class="facet-label">' + MAKI.escHtml(c.name) + '</span>';
                h += '<span class="facet-count">' + c.count + '</span>';
                h += '</div>';
                if (open) walk(c, depth + 1);
            });
        }
        walk(root, 0);
        return h;
    }

    // Section ordering reflects how often the user filters by each axis:
    // tags (the curated taxonomy) above formats/volumes (which most users
    // touch rarely after initial setup). Ratings/labels stay near the top
    // because the star/dot UI in the filter row makes them headline filters.
    function render(data) {
        lastData = data;
        var html = '';

        // Rating section — clicking a row mirrors the star widget.
        html += facetSection('ratings', 'Ratings', function() {
            var h = '';
            var sorted = (data.ratings || []).slice().sort(function(a, b) {
                if (a[0] === null) return 1;
                if (b[0] === null) return -1;
                return b[0] - a[0];
            });
            var maxCount = Math.max.apply(null, sorted.map(function(r) { return r[1]; }).concat([1]));
            for (var i = 0; i < sorted.length; i++) {
                var val = sorted[i][0];
                var cnt = sorted[i][1];
                var stars = '';
                if (val !== null) {
                    for (var s = 1; s <= 5; s++) stars += s <= val ? '\u2605' : '\u2606';
                } else {
                    stars = 'Unrated';
                }
                var pct = Math.round((cnt / maxCount) * 100);
                var ratingValue = val === null ? '0' : String(val);
                h += rowAttrs('rating', ratingValue, 'Click to filter by this rating') + '>';
                h += '<span class="facet-label">' + stars + '</span>';
                h += '<span class="facet-bar-wrap"><span class="facet-bar" style="width:' + pct + '%"></span></span>';
                h += '<span class="facet-count">' + cnt + '</span>';
                h += '</div>';
            }
            return h;
        });

        // Label section — clicking a row mirrors the label-dot widget.
        html += facetSection('labels', 'Labels', function() {
            var h = '';
            var sorted = (data.labels || []).slice().sort(function(a, b) { return b[1] - a[1]; });
            for (var i = 0; i < sorted.length; i++) {
                var val = sorted[i][0];
                var cnt = sorted[i][1];
                var labelValue = val === null ? 'none' : val;
                var dot = val && labelColors[val] ? '<span class="facet-label-dot" style="background:' + labelColors[val] + '"></span>' : '';
                h += rowAttrs('label', labelValue, 'Click to filter by this label') + '>';
                h += '<span class="facet-label">' + dot + MAKI.escHtml(val || 'No label') + '</span>';
                h += '<span class="facet-count">' + cnt + '</span>';
                h += '</div>';
            }
            return h;
        });

        // Tags section — hierarchical render. Auto-expansion on storage means
        // every ancestor path is already in the flat list with its own count,
        // so we can simply reshape into a tree and indent. Click-to-narrow
        // applies the FULL path (so picking "2024" under "Holzkirchner..."
        // adds the disambiguated tag, not the bare leaf).
        if (data.tags && data.tags.length > 0) {
            html += facetSection('tags', 'Tags', function() {
                return renderTagTree(buildTagTree(data.tags));
            });
        }

        // Years section — clicking appends `date:YYYY` to the q box.
        if (data.years && data.years.length > 0) {
            html += facetSection('years', 'Years', function() {
                var h = '';
                var maxCount = Math.max.apply(null, data.years.map(function(y) { return y[1]; }).concat([1]));
                for (var i = 0; i < data.years.length; i++) {
                    var val = data.years[i][0];
                    var cnt = data.years[i][1];
                    var pct = Math.round((cnt / maxCount) * 100);
                    h += rowAttrs('year', String(val), 'Click to filter by this year') + '>';
                    h += '<span class="facet-label">' + MAKI.escHtml(val) + '</span>';
                    h += '<span class="facet-bar-wrap"><span class="facet-bar" style="width:' + pct + '%"></span></span>';
                    h += '<span class="facet-count">' + cnt + '</span>';
                    h += '</div>';
                }
                return h;
            });
        }

        // Format section — clicking toggles the format checkbox.
        html += facetSection('formats', 'Formats', function() {
            var h = '';
            for (var i = 0; i < (data.formats || []).length; i++) {
                var val = data.formats[i][0];
                var cnt = data.formats[i][1];
                h += rowAttrs('format', val, 'Click to filter by this format') + '>';
                h += '<span class="facet-label">' + MAKI.escHtml(val) + '</span>';
                h += '<span class="facet-count">' + cnt + '</span>';
                h += '</div>';
            }
            return h;
        });

        // Volume section — clicking sets the volume select (single-select).
        if (data.volumes && data.volumes.length > 0) {
            html += facetSection('volumes', 'Volumes', function() {
                var h = '';
                for (var i = 0; i < data.volumes.length; i++) {
                    var vid = data.volumes[i][0];
                    var vlabel = data.volumes[i][1];
                    var cnt = data.volumes[i][2];
                    h += rowAttrs('volume', vid, 'Click to filter by this volume') + '>';
                    h += '<span class="facet-label">' + MAKI.escHtml(vlabel) + '</span>';
                    h += '<span class="facet-count">' + cnt + '</span>';
                    h += '</div>';
                }
                return h;
            });
        }

        // Geotagged — clicking toggles `geo:any` in the q box.
        if (data.geotagged > 0) {
            html += '<div class="facet-section facet-geo">';
            html += rowAttrs('geotagged', '1', 'Click to filter to geotagged assets') + '>';
            html += '<span class="facet-label">\uD83D\uDCCD Geotagged</span>';
            html += '<span class="facet-count">' + data.geotagged + '</span>';
            html += '</div></div>';
        }

        var scrollTop = sidebar.scrollTop;
        content.innerHTML = html;
        content.classList.remove('facet-refreshing');
        restoreCollapseStates();
        restoreScroll(scrollTop);
    }

    // Put the sidebar back where it was. When the row the user just
    // activated is still in the list, it lands at the same height as
    // before, even if rows above it disappeared with the narrower result.
    function restoreScroll(scrollTop) {
        sidebar.scrollTop = scrollTop;
        var a = anchor;
        anchor = null;
        if (!a) return;
        var row = null;
        content.querySelectorAll('[data-facet-type="' + a.type + '"]').forEach(function(el) {
            if (!row && el.getAttribute('data-facet-value') === a.value) row = el;
        });
        if (!row) return;
        var offset = row.getBoundingClientRect().top - sidebar.getBoundingClientRect().top;
        sidebar.scrollTop += offset - a.offset;
        if (a.focus) row.focus({ preventScroll: true });
    }

    function facetSection(key, title, contentFn) {
        var wasOpen = getSectionState(key);
        return '<details class="facet-section" data-section="' + key + '"' + (wasOpen ? ' open' : '') + '>' +
            '<summary>' + title + '</summary>' +
            '<div class="facet-section-body">' + contentFn() + '</div></details>';
    }

    function getSectionState(key) {
        try {
            var states = JSON.parse(localStorage.getItem('maki-facet-sections') || '{}');
            return states[key] !== false; // default open
        } catch(e) { return true; }
    }

    function restoreCollapseStates() {
        var sections = content.querySelectorAll('.facet-section[data-section]');
        sections.forEach(function(det) {
            det.addEventListener('toggle', function() {
                saveSectionState(det.getAttribute('data-section'), det.open);
            });
        });
    }

    function saveSectionState(key, open) {
        try {
            var states = JSON.parse(localStorage.getItem('maki-facet-sections') || '{}');
            states[key] = open;
            localStorage.setItem('maki-facet-sections', JSON.stringify(states));
        } catch(e) {}
    }

    // Re-fetch facets after results update
    document.body.addEventListener('htmx:afterSwap', function(e) {
        if (e.detail.target && e.detail.target.id === 'results') {
            updateToggleBtn(isOpen());
            fetchFacets();
        }
    });

    init();
    return { toggle: toggle, fetchFacets: fetchFacets, isOpen: isOpen };
})();
