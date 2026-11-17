// Browse page: batch selection.
//
// The selection set (kept across pages in sessionStorage), the batch
// toolbar and every batch operation started from it. Loaded by
// browse.html from /static/browse/batch.js, after the page's first inline
// block and before the ones that follow (grid density, stacks, drag and
// drop, lightbox bridge, keyboard navigation), which reach it through
// window.damBatch.
//
// Plain ES5, no template expressions: anything the server has to supply
// comes from the DOM or from window.MAKI (maki-core.js).
(function() {
    // Set modifier key label based on platform
    var isMac = /Mac|iPhone|iPad|iPod/.test(navigator.platform || navigator.userAgent);
    document.querySelectorAll('.mod-key').forEach(function(el) {
        el.textContent = isMac ? '\u2318' : 'Ctrl+';
    });

    var selected = new Set();
    var batchBusy = false;
    // Span hint shown next to the count when the selection covers more
    // than the visible page (set by Shift-Cmd-A "select all matching").
    // null means "all selected IDs are on the current page".
    var selectionPageSpan = null;

    // Restore selection from sessionStorage — but clear on forced reload.
    // Track whether the restore came from a navigation that left the
    // browse page entirely (e.g. visit to /tags, /stats) as opposed to
    // a detail-page-and-back round trip — selection forgotten across
    // a non-detail navigation is the case that bit the user, so we
    // flag it for visible feedback below.
    var restoredFromAway = false;
    try {
        var savedSel = sessionStorage.getItem('maki-browse-selection');
        if (savedSel) {
            sessionStorage.removeItem('maki-browse-selection');
            var navType = (performance.getEntriesByType('navigation')[0] || {}).type;
            if (navType !== 'reload') {
                JSON.parse(savedSel).forEach(function(id) { selected.add(id); });
                if (selected.size > 0) {
                    // document.referrer is the page that linked here.
                    // Empty / cross-origin / pageless => treat as
                    // "from elsewhere"; same-origin /asset/... => was
                    // a detail-page round trip (still in the browsing
                    // flow, no warning needed); anything else
                    // same-origin (Tags, Stats, Maintain, …) => the
                    // user genuinely left and came back, surface the
                    // restored selection.
                    var ref = document.referrer || '';
                    var sameOrigin = ref.indexOf(window.location.origin) === 0;
                    var fromDetail = sameOrigin
                        && ref.substring(window.location.origin.length).indexOf('/asset/') === 0;
                    var fromBrowseFilterChange = sameOrigin
                        && (ref.substring(window.location.origin.length) === '/'
                            || ref.substring(window.location.origin.length).indexOf('/?') === 0);
                    restoredFromAway = !fromDetail && !fromBrowseFilterChange;
                }
            }
        }
    } catch(e) {}

    var toolbar = document.getElementById('batch-toolbar');
    var countEl = document.getElementById('batch-count');

    // Count selected IDs that aren't currently rendered as cards on this
    // page. This is the real "you might accidentally batch-edit something
    // you can't see" signal — the user's bug report named this exact
    // pattern (come back to browse, no visible selection on screen, count
    // says N selected, start a new selection round, batch-tag applies
    // to assets they'd forgotten about).
    function countSelectedOffPage() {
        if (selected.size === 0) return 0;
        var visible = new Set();
        document.querySelectorAll('.asset-card[data-asset-id]').forEach(function(card) {
            visible.add(card.getAttribute('data-asset-id'));
        });
        var off = 0;
        selected.forEach(function(id) { if (!visible.has(id)) off++; });
        return off;
    }

    function updateToolbar() {
        var n = selected.size;
        // If the recorded span hint no longer matches the actual selection
        // size (user toggled individual checkboxes after Shift-Cmd-A), drop
        // the hint — it'd be misleading. We keep it when the size matches
        // the spanned set; on partial deselect we'd need per-page metadata
        // we don't have, so the simpler invariant is "exact match or drop".
        if (selectionPageSpan && (selectionPageSpan.expectedSize !== n || n === 0)) {
            selectionPageSpan = null;
        }
        var label = n + ' selected';
        if (selectionPageSpan && selectionPageSpan.totalPages > 1) {
            label += ' (across ' + selectionPageSpan.totalPages + ' pages)';
        }
        countEl.textContent = label;
        if (n > 0) {
            toolbar.classList.add('visible');
            document.body.classList.add('has-batch-toolbar');
            document.body.classList.add('batch-active');
        } else {
            toolbar.classList.remove('visible');
            document.body.classList.remove('has-batch-toolbar');
            document.body.classList.remove('batch-active');
        }

        // Persistent off-page indicator. Stays visible AS LONG AS any
        // selected asset isn't rendered on the current page — clears
        // automatically when the user pages back to where they all live,
        // clears the selection, or deselects everything that isn't here.
        var pill = document.getElementById('restored-selection-pill');
        if (pill) {
            var offPage = countSelectedOffPage();
            if (offPage > 0) {
                pill.style.display = '';
                pill.textContent = '⚠ ' + offPage + ' selected off this page';
                pill.title = offPage + ' asset(s) you have selected are not visible on this page. Batch actions still apply to them.';
                countEl.classList.add('has-off-page');
            } else {
                pill.style.display = 'none';
                countEl.classList.remove('has-off-page');
            }
        }

        syncCheckboxes();
        updateStackButtons();
    }

    function updateStackButtons() {
        var ids = Array.from(selected);
        var stacked = [];
        var unstacked = [];
        var stackIds = new Set();
        ids.forEach(function(id) {
            var card = document.querySelector('[data-asset-id="' + id + '"]');
            if (!card) return;
            var sid = card.getAttribute('data-stack');
            if (sid) { stacked.push(id); stackIds.add(sid); }
            else { unstacked.push(id); }
        });
        var addBtn = document.getElementById('batch-add-to-stack');
        var removeBtn = document.getElementById('batch-remove-from-stack');
        var pickBtn = document.getElementById('batch-set-pick');
        // Add to stack: at least one unstacked + exactly one stacked (identifies the target stack)
        addBtn.style.display = (unstacked.length >= 1 && stacked.length >= 1 && stackIds.size === 1) ? '' : 'none';
        // Remove from stack: one or more stacked selected
        removeBtn.style.display = (stacked.length >= 1) ? '' : 'none';
        // Set pick: exactly one stacked asset selected
        pickBtn.style.display = (stacked.length === 1 && unstacked.length === 0) ? '' : 'none';
    }

    function syncCheckboxes() {
        var boxes = document.querySelectorAll('.asset-select-cb');
        boxes.forEach(function(cb) {
            var id = cb.value;
            cb.checked = selected.has(id);
            var card = cb.closest('.asset-card');
            if (card) {
                if (selected.has(id)) {
                    card.classList.add('selected');
                } else {
                    card.classList.remove('selected');
                }
            }
        });
    }

    function clearSelection() {
        selected.clear();
        selectionPageSpan = null;
        updateToolbar();
    }

    // Wire Enter → confirm, Escape → cancel for modal overlays
    function setupModalKeyboard(overlay, confirmSelector) {
        var confirmBtn = overlay.querySelector(confirmSelector);
        if (confirmBtn) confirmBtn.focus();
        function handler(e) {
            if (e.key === 'Escape') {
                e.preventDefault();
                e.stopImmediatePropagation();
                overlay.remove();
            } else if (e.key === 'Enter' && confirmBtn && !confirmBtn.disabled
                       && document.activeElement.tagName !== 'INPUT') {
                e.preventDefault();
                e.stopImmediatePropagation();
                confirmBtn.click();
            }
        }
        document.addEventListener('keydown', handler, true);
        new MutationObserver(function(_, obs) {
            if (!document.body.contains(overlay)) {
                document.removeEventListener('keydown', handler, true);
                obs.disconnect();
            }
        }).observe(document.body, {childList: true});
    }

    function getVisibleIds() {
        var ids = [];
        document.querySelectorAll('.asset-select-cb').forEach(function(cb) {
            ids.push(cb.value);
        });
        return ids;
    }

    // Checkbox change via event delegation
    document.getElementById('results').addEventListener('change', function(e) {
        if (!e.target.classList.contains('asset-select-cb')) return;
        var cb = e.target;
        var id = cb.value;

        if (cb.checked) {
            selected.add(id);
        } else {
            selected.delete(id);
        }
        updateToolbar();
    });

    // Re-sync after htmx swaps results
    document.body.addEventListener('htmx:afterSwap', function(e) {
        if (e.detail.target && e.detail.target.id === 'results') {
            syncCheckboxes();
            window.bustChangedPreviews();
            // Pagination, filter changes, and sort all swap #results.
            // Recompute the off-page pill against the new set of
            // visible cards so the indicator stays accurate as the
            // user pages around with a persistent selection.
            updateToolbar();
        }
    });

    // Select page
    document.getElementById('batch-select-page').addEventListener('click', function() {
        getVisibleIds().forEach(function(id) { selected.add(id); });
        updateToolbar();
    });

    // Clear
    document.getElementById('batch-clear').addEventListener('click', clearSelection);

    // Keyboard shortcuts (Cmd/Ctrl+A handled here; Escape handled by keyboard nav IIFE)
    document.addEventListener('keydown', function(e) {
        if (window.damHelp && window.damHelp.isOpen()) return;
        // Ignore when typing in a text input/textarea/select (but not checkboxes)
        var el = e.target;
        if (el.tagName === 'TEXTAREA' || el.tagName === 'SELECT') return;
        if (el.tagName === 'INPUT' && el.type !== 'checkbox') return;

        // Cmd/Ctrl+A — select all on current page
        if (e.key === 'a' && (e.metaKey || e.ctrlKey) && !e.shiftKey) {
            e.preventDefault();
            getVisibleIds().forEach(function(id) { selected.add(id); });
            updateToolbar();
        }
        // Shift-Cmd/Ctrl+A — select all matching the current query (across pages).
        // Note: KeyboardEvent.key reports the SHIFTED form for letter
        // keys, so this is "A" not "a" when shift is held.
        if ((e.key === 'A' || e.key === 'a') && (e.metaKey || e.ctrlKey) && e.shiftKey) {
            e.preventDefault();
            selectAllMatching();
        }
    });

    /// Fetch every asset ID matching the current browse filter and seed
    /// the selection with them. If the result spans more than one page,
    /// confirm via modal first; otherwise just merge silently (Shift-Cmd-A
    /// on a single-page result is effectively the same as Cmd-A).
    function selectAllMatching() {
        if (batchBusy) return;
        // Use the live URL so every active filter param flows through —
        // avoids re-implementing the search-param whitelist of getFilterParams().
        MAKI.api.get('/api/all-ids' + window.location.search)
            .then(function(data) {
                var ids = data.ids || [];
                var totalPages = data.total_pages || 1;
                var total = data.total || ids.length;
                if (ids.length === 0) {
                    countEl.textContent = 'No matching assets';
                    setTimeout(function() { updateToolbar(); }, 1200);
                    return;
                }
                if (totalPages <= 1) {
                    // Spans only the current page → no scary modal.
                    ids.forEach(function(id) { selected.add(id); });
                    selectionPageSpan = null;
                    updateToolbar();
                    return;
                }
                showSelectAllModal(ids, total, totalPages);
            })
            .catch(function(e) {
                countEl.textContent = 'Could not load all matching IDs';
                setTimeout(function() { updateToolbar(); }, 1500);
                console.error('select-all-matching:', e);
            });
    }

    /// Modal: "Select N matching assets across M pages?" with
    /// Replace / Add-to-selection radios. Reuses the group-confirm-overlay
    /// styling to stay visually consistent with delete / group / export modals.
    function showSelectAllModal(ids, total, totalPages) {
        var hasExisting = selected.size > 0;
        var overlay = document.createElement('div');
        overlay.className = 'group-confirm-overlay';
        var radios = hasExisting
            ? '<div class="export-options">' +
                '<div class="export-row"><label><input type="radio" name="select-mode" value="replace" checked> ' +
                    'Replace current selection (' + selected.size + ')</label></div>' +
                '<div class="export-row"><label><input type="radio" name="select-mode" value="add"> ' +
                    'Add to current selection</label></div>' +
              '</div>'
            : '';
        overlay.innerHTML = '<div class="group-confirm-modal">' +
            '<h3>Select ' + total + ' matching asset' + (total === 1 ? '' : 's') + '?</h3>' +
            '<p class="group-confirm-hint">Spans ' + totalPages + ' page' + (totalPages === 1 ? '' : 's') +
                '. Selection persists across page navigation; batch toolbar acts on the full set.</p>' +
            radios +
            '<div class="group-confirm-buttons">' +
                '<button class="group-confirm-cancel">Cancel</button>' +
                '<button class="group-confirm-ok select-all-confirm-btn">Select</button>' +
            '</div></div>';
        document.body.appendChild(overlay);
        setupModalKeyboard(overlay, '.select-all-confirm-btn');

        overlay.querySelector('.group-confirm-cancel').addEventListener('click', function() { overlay.remove(); });
        overlay.addEventListener('click', function(e) { if (e.target === overlay) overlay.remove(); });

        overlay.querySelector('.select-all-confirm-btn').addEventListener('click', function() {
            var modeInput = overlay.querySelector('input[name="select-mode"]:checked');
            var mode = modeInput ? modeInput.value : 'replace';
            overlay.remove();
            if (mode === 'replace') {
                selected.clear();
            }
            ids.forEach(function(id) { selected.add(id); });
            selectionPageSpan = { totalPages: totalPages, expectedSize: selected.size };
            updateToolbar();
        });
    }

    function setButtonsBusy(busy) {
        batchBusy = busy;
        var btns = toolbar.querySelectorAll('.batch-btn, .batch-star, .batch-rating-clear, .batch-label-dot, .batch-label-clear');
        btns.forEach(function(b) { b.disabled = busy; });
        if (busy) {
            countEl.textContent = 'Processing ' + selected.size + ' assets\u2026';
            countEl.classList.add('batch-processing');
        } else {
            // Restore the regular label via updateToolbar so the
            // "(across N pages)" hint is preserved.
            countEl.classList.remove('batch-processing');
            updateToolbar();
        }
    }

    function refreshResults() {
        htmx.ajax('GET', window.location.href, {target: '#results', swap: 'innerHTML'});
    }

    // Batch tag add
    document.getElementById('batch-add-tag').addEventListener('click', function() {
        var input = document.getElementById('batch-tag-input');
        var tag = input.value.trim();
        if (!tag || selected.size === 0 || batchBusy) return;
        setButtonsBusy(true);
        MAKI.api.post('/api/batch/tags', {asset_ids: Array.from(selected), tags: [tag], remove: false})
        .then(function() {
            input.value = '';
            if (window.damTags) window.damTags.fetchTags();
            refreshResults();
        })
        .catch(function(err) { console.error('Batch tag error:', err); })
        .finally(function() { setButtonsBusy(false); });
    });

    // Batch tag remove
    document.getElementById('batch-remove-tag').addEventListener('click', function() {
        var input = document.getElementById('batch-tag-input');
        var tag = input.value.trim();
        if (!tag || selected.size === 0 || batchBusy) return;
        setButtonsBusy(true);
        MAKI.api.post('/api/batch/tags', {asset_ids: Array.from(selected), tags: [tag], remove: true})
        .then(function() {
            input.value = '';
            if (window.damTags) window.damTags.fetchTags();
            refreshResults();
        })
        .catch(function(err) { console.error('Batch tag error:', err); })
        .finally(function() { setButtonsBusy(false); });
    });

    // --- Batch tag autocomplete ---
    (function() {
        var bInput = document.getElementById('batch-tag-input');
        var bAc = document.getElementById('batch-tag-autocomplete');
        var bAcIndex = -1;

        function bShowSuggestions(query) {
            if (!window.damTags) return;
            var tags = window.damTags.allTags();
            query = query.toLowerCase();
            var queryStorage = query.replace(/>/g, '|');
            var rawMatches = tags.filter(function(t) {
                var tl = t.toLowerCase();
                return tl.indexOf(query) !== -1 || tl.indexOf(queryStorage) !== -1;
            });
            // Keep leaves AND middle-of-hierarchy tags whose own name (any
            // component) matches the query — without the second clause,
            // searching "subject" with both `subject|nature` and
            // `subject|nature|landscape` in the catalog would hide the
            // middle node `subject|nature`.
            var matches = rawMatches.filter(function(t) {
                var prefix = t + '|';
                var hasChild = rawMatches.some(function(other) { return other.indexOf(prefix) === 0; });
                if (!hasChild) return true;
                var components = t.toLowerCase().split('|');
                return components.some(function(c) {
                    return c.indexOf(query) !== -1 || c.indexOf(queryStorage) !== -1;
                });
            }).slice(0, 12);

            if (matches.length === 0) {
                bAc.style.display = 'none';
                bAcIndex = -1;
                return;
            }

            bAc.innerHTML = '';
            bAcIndex = -1;
            matches.forEach(function(t) {
                var div = document.createElement('div');
                div.className = 'tag-suggestion';
                div.setAttribute('data-tag', t);
                var parts = t.split('|');
                if (parts.length > 1) {
                    var prefix = parts.slice(0, -1).join('|') + '|';
                    div.innerHTML = '<span class="tag-path-prefix">' +
                        prefix.replace(/&/g,'&amp;').replace(/</g,'&lt;') +
                        '</span>' + parts[parts.length - 1].replace(/&/g,'&amp;').replace(/</g,'&lt;');
                } else {
                    div.textContent = window.damTags.tagToDisplay(t);
                }
                div.addEventListener('mousedown', function(e) {
                    e.preventDefault();
                    bInput.value = window.damTags.tagToDisplay(t);
                    bAc.style.display = 'none';
                    bAcIndex = -1;
                });
                bAc.appendChild(div);
            });
            bAc.style.display = 'block';
        }

        function bHighlight(idx) {
            var items = bAc.querySelectorAll('.tag-suggestion');
            items.forEach(function(el, i) {
                el.classList.toggle('active', i === idx);
            });
            if (idx >= 0 && items[idx]) items[idx].scrollIntoView({block: 'nearest'});
        }

        bInput.addEventListener('input', function() {
            var val = this.value.trim();
            if (val.length > 0) {
                bShowSuggestions(val);
            } else {
                bAc.style.display = 'none';
                bAcIndex = -1;
            }
        });

        bInput.addEventListener('keydown', function(e) {
            var items = bAc.querySelectorAll('.tag-suggestion');
            if (e.key === 'ArrowDown' && bAc.style.display === 'block') {
                e.preventDefault();
                bAcIndex = Math.min(bAcIndex + 1, items.length - 1);
                bHighlight(bAcIndex);
            } else if (e.key === 'ArrowUp' && bAc.style.display === 'block') {
                e.preventDefault();
                bAcIndex = Math.max(bAcIndex - 1, 0);
                bHighlight(bAcIndex);
            } else if (e.key === 'Enter') {
                e.preventDefault();
                e.stopPropagation();
                if (bAcIndex >= 0 && items[bAcIndex]) {
                    bInput.value = window.damTags.tagToDisplay(items[bAcIndex].getAttribute('data-tag'));
                    bAc.style.display = 'none';
                    bAcIndex = -1;
                } else {
                    bAc.style.display = 'none';
                    bAcIndex = -1;
                    document.getElementById('batch-add-tag').click();
                }
            } else if (e.key === 'Escape') {
                bAc.style.display = 'none';
                bAcIndex = -1;
            }
        });

        bInput.addEventListener('blur', function() {
            setTimeout(function() {
                bAc.style.display = 'none';
                bAcIndex = -1;
            }, 150);
        });

        bInput.addEventListener('focus', function() {
            var val = this.value.trim();
            if (val.length > 0) bShowSuggestions(val);
        });
    })();

    // Batch rating stars
    var batchStars = document.querySelectorAll('#batch-rating .batch-star');
    batchStars.forEach(function(star) {
        star.addEventListener('mouseenter', function() {
            var val = parseInt(this.getAttribute('data-value'));
            batchStars.forEach(function(s) {
                var sv = parseInt(s.getAttribute('data-value'));
                if (sv <= val) {
                    s.classList.add('preview');
                    s.innerHTML = '&#9733;';
                } else {
                    s.classList.remove('preview');
                    s.innerHTML = '&#9734;';
                }
            });
        });
        star.addEventListener('mouseleave', function() {
            batchStars.forEach(function(s) {
                s.classList.remove('preview');
                if (!s.classList.contains('active')) {
                    s.innerHTML = '&#9734;';
                }
            });
        });
        star.addEventListener('click', function() {
            if (selected.size === 0 || batchBusy) return;
            var val = parseInt(this.getAttribute('data-value'));
            setButtonsBusy(true);
            MAKI.api.put('/api/batch/rating', {asset_ids: Array.from(selected), rating: val})
            .then(function() {
                refreshResults();
            })
            .catch(function(err) { console.error('Batch rating error:', err); })
            .finally(function() { setButtonsBusy(false); });
        });
    });

    // Batch rating clear
    document.getElementById('batch-rating-clear').addEventListener('click', function() {
        if (selected.size === 0 || batchBusy) return;
        setButtonsBusy(true);
        MAKI.api.put('/api/batch/rating', {asset_ids: Array.from(selected), rating: null})
        .then(function() {
            refreshResults();
        })
        .catch(function(err) { console.error('Batch rating error:', err); })
        .finally(function() { setButtonsBusy(false); });
    });

    // Batch label dots
    var batchLabelDots = document.querySelectorAll('#batch-label .batch-label-dot');
    batchLabelDots.forEach(function(dot) {
        dot.addEventListener('click', function() {
            if (selected.size === 0 || batchBusy) return;
            var lbl = this.getAttribute('data-label');
            setButtonsBusy(true);
            MAKI.api.put('/api/batch/label', {asset_ids: Array.from(selected), label: lbl})
            .then(function() {
                refreshResults();
            })
            .catch(function(err) { console.error('Batch label error:', err); })
            .finally(function() { setButtonsBusy(false); });
        });
    });

    // Batch label clear
    document.getElementById('batch-label-clear').addEventListener('click', function() {
        if (selected.size === 0 || batchBusy) return;
        setButtonsBusy(true);
        MAKI.api.put('/api/batch/label', {asset_ids: Array.from(selected), label: null})
        .then(function() {
            refreshResults();
        })
        .catch(function(err) { console.error('Batch label error:', err); })
        .finally(function() { setButtonsBusy(false); });
    });

    // Batch add to collection
    var colSelect = document.getElementById('batch-collection-select');

    function populateCollectionDropdown() {
        // Keep the first placeholder option, remove the rest
        while (colSelect.options.length > 1) colSelect.remove(1);
        return MAKI.api.get('/api/collections')
            .then(function(data) {
                data.forEach(function(c) {
                    var opt = document.createElement('option');
                    opt.value = c.name;
                    opt.textContent = c.name + ' (' + c.asset_count + ')';
                    colSelect.appendChild(opt);
                });
                // Add "New..." option at the end
                var newOpt = document.createElement('option');
                newOpt.value = '__new__';
                newOpt.textContent = 'New\u2026';
                colSelect.appendChild(newOpt);
            });
    }
    populateCollectionDropdown();

    colSelect.addEventListener('change', function() {
        if (this.value !== '__new__') return;
        this.value = ''; // Reset immediately
        var name = prompt('New collection name:');
        if (!name || !name.trim()) return;
        name = name.trim();
        MAKI.api.post('/api/collections', {name: name, description: null})
        .then(function() {
            return populateCollectionDropdown();
        })
        .then(function() {
            colSelect.value = name;
        })
        .catch(function(err) { alert('Failed to create collection: ' + err.message); });
    });

    document.getElementById('batch-add-collection').addEventListener('click', function() {
        var colName = colSelect.value;
        if (!colName || colName === '__new__' || selected.size === 0 || batchBusy) return;
        setButtonsBusy(true);
        MAKI.api.post('/api/batch/collection', {asset_ids: Array.from(selected), collection: colName})
        .then(function() {
            clearSelection();
            refreshResults();
            populateCollectionDropdown();
        })
        .catch(function(err) { console.error('Batch collection error:', err); })
        .finally(function() { setButtonsBusy(false); });
    });

    // Batch remove from collection
    document.getElementById('batch-remove-collection').addEventListener('click', function() {
        var colName = colSelect.value;
        if (!colName || colName === '__new__' || selected.size === 0 || batchBusy) return;
        setButtonsBusy(true);
        MAKI.api.del('/api/batch/collection', {asset_ids: Array.from(selected), collection: colName})
        .then(function() {
            clearSelection();
            refreshResults();
            populateCollectionDropdown().then(function() {
                // Re-select the collection if we're browsing it
                var filterSel = document.querySelector('[name=collection]');
                var browsing = filterSel ? filterSel.value : '';
                if (browsing) colSelect.value = browsing;
            });
        })
        .catch(function(err) { console.error('Batch collection remove error:', err); })
        .finally(function() { setButtonsBusy(false); });
    });

    // Show +Collection or −Collection depending on whether we're browsing a collection
    var addColBtn = document.getElementById('batch-add-collection');
    var removeColBtn = document.getElementById('batch-remove-collection');

    function updateCollectionButtons() {
        var filterSel = document.querySelector('[name=collection]');
        var browsing = filterSel ? filterSel.value : '';
        if (browsing) {
            addColBtn.style.display = 'none';
            removeColBtn.style.display = '';
            // Auto-select in batch dropdown once populated
            var checkInterval = setInterval(function() {
                for (var i = 0; i < colSelect.options.length; i++) {
                    if (colSelect.options[i].value === browsing) {
                        colSelect.value = browsing;
                        clearInterval(checkInterval);
                        return;
                    }
                }
            }, 100);
            setTimeout(function() { clearInterval(checkInterval); }, 3000);
        } else {
            addColBtn.style.display = '';
            removeColBtn.style.display = 'none';
        }
    }
    updateCollectionButtons();
    // Re-check when filter-row collection dropdown changes
    var filterColSel = document.querySelector('[name=collection]');
    if (filterColSel) {
        filterColSel.addEventListener('change', updateCollectionButtons);
    }
    window.addEventListener('popstate', updateCollectionButtons);

    // Batch stack (create stack from selected)
    document.getElementById('batch-stack').addEventListener('click', function() {
        if (selected.size < 2 || batchBusy) return;
        setButtonsBusy(true);
        MAKI.api.post('/api/batch/stack', {asset_ids: Array.from(selected)})
        .then(function() {
            clearSelection();
            refreshResults();
        })
        .catch(function(err) { console.error('Batch stack error:', err); })
        .finally(function() { setButtonsBusy(false); });
    });

    // Batch unstack (remove selected from stacks)
    document.getElementById('batch-unstack').addEventListener('click', function() {
        if (selected.size === 0 || batchBusy) return;
        setButtonsBusy(true);
        MAKI.api.del('/api/batch/stack', {asset_ids: Array.from(selected)})
        .then(function() {
            clearSelection();
            refreshResults();
        })
        .catch(function(err) { console.error('Batch unstack error:', err); })
        .finally(function() { setButtonsBusy(false); });
    });

    // Add to stack: add unstacked assets to the stack of the stacked asset
    document.getElementById('batch-add-to-stack').addEventListener('click', function() {
        if (batchBusy) return;
        var ids = Array.from(selected);
        var referenceId = null;
        var addIds = [];
        ids.forEach(function(id) {
            var card = document.querySelector('[data-asset-id="' + id + '"]');
            if (!card) return;
            if (card.getAttribute('data-stack')) { referenceId = id; }
            else { addIds.push(id); }
        });
        if (!referenceId || addIds.length === 0) return;
        setButtonsBusy(true);
        MAKI.api.post('/api/asset/' + encodeURIComponent(referenceId) + '/stack-add', {asset_ids: addIds})
        .then(function() { clearSelection(); refreshResults(); })
        .catch(function(err) { alert('Failed to add to stack: ' + err.message); })
        .finally(function() { setButtonsBusy(false); });
    });

    // Remove from stack: remove selected stacked assets from their stacks
    document.getElementById('batch-remove-from-stack').addEventListener('click', function() {
        if (batchBusy) return;
        var ids = Array.from(selected).filter(function(id) {
            var card = document.querySelector('[data-asset-id="' + id + '"]');
            return card && card.getAttribute('data-stack');
        });
        if (ids.length === 0) return;
        setButtonsBusy(true);
        MAKI.api.del('/api/batch/stack', {asset_ids: ids})
        .then(function() { clearSelection(); refreshResults(); })
        .catch(function(err) { console.error('Remove from stack error:', err); })
        .finally(function() { setButtonsBusy(false); });
    });

    // Set pick: set the single selected stacked asset as its stack's pick
    document.getElementById('batch-set-pick').addEventListener('click', function() {
        if (batchBusy) return;
        var ids = Array.from(selected);
        if (ids.length !== 1) return;
        setButtonsBusy(true);
        MAKI.api.put('/api/asset/' + encodeURIComponent(ids[0]) + '/stack-pick', {})
        .then(function() { clearSelection(); refreshResults(); })
        .catch(function(err) { alert('Failed to set pick: ' + err.message); })
        .finally(function() { setButtonsBusy(false); });
    });

    // Batch group — merge selected assets into one
    document.getElementById('batch-group').addEventListener('click', function() {
        if (selected.size < 2 || batchBusy) return;
        var focusedId = (window.damKeyNav && window.damKeyNav.getFocusedId) ? window.damKeyNav.getFocusedId() : null;
        var targetId = focusedId && selected.has(focusedId) ? focusedId : null;

        // Build confirm modal with thumbnails
        var overlay = document.createElement('div');
        overlay.className = 'group-confirm-overlay';
        overlay.innerHTML = '<div class="group-confirm-modal">' +
            '<h3>Merge ' + selected.size + ' assets into one?</h3>' +
            '<p class="group-confirm-hint">Donor variants become alternates. This cannot be undone.</p>' +
            '<div class="group-confirm-grid"></div>' +
            '<div class="group-confirm-buttons">' +
            '<button class="group-confirm-cancel">Cancel</button>' +
            '<button class="group-confirm-ok">Merge</button>' +
            '</div></div>';

        var grid = overlay.querySelector('.group-confirm-grid');
        selected.forEach(function(id) {
            var card = document.querySelector('.asset-card[data-asset-id="' + id + '"]');
            var thumb = document.createElement('div');
            thumb.className = 'group-confirm-thumb';
            if (targetId && id === targetId) {
                thumb.classList.add('group-target');
            }
            var img = card ? card.querySelector('img') : null;
            var src = img ? img.src : '';
            var nameEl = card ? (card.querySelector('.card-name') || card.querySelector('.card-filename')) : null;
            var name = nameEl ? nameEl.textContent.trim() : id.substring(0, 8) + '…';
            var imgHtml = src ? '<img src="' + src + '" alt="">' : '<div class="group-confirm-placeholder">' + id.substring(0, 8) + '</div>';
            thumb.innerHTML = imgHtml +
                '<span class="group-confirm-name">' + name + '</span>' +
                (targetId && id === targetId ? '<span class="group-confirm-badge">Target</span>' : '');
            grid.appendChild(thumb);
        });

        if (!targetId) {
            var hint = overlay.querySelector('.group-confirm-hint');
            hint.textContent = 'Oldest asset becomes the target. Donor variants become alternates. This cannot be undone.';
        }

        document.body.appendChild(overlay);
        setupModalKeyboard(overlay, '.group-confirm-ok');

        overlay.querySelector('.group-confirm-cancel').addEventListener('click', function() {
            overlay.remove();
        });
        overlay.addEventListener('click', function(e) {
            if (e.target === overlay) overlay.remove();
        });

        overlay.querySelector('.group-confirm-ok').addEventListener('click', function() {
            overlay.remove();
            setButtonsBusy(true);
            var payload = {asset_ids: Array.from(selected)};
            if (targetId) payload.target_id = targetId;
            MAKI.api.post('/api/batch/group', payload)
            .then(function(data) {
                clearSelection();
                refreshResults();
            })
            .catch(function(err) { alert('Group failed: ' + err.message); })
            .finally(function() { setButtonsBusy(false); });
        });
    });

    // Batch auto-group by filename stem
    document.getElementById('batch-auto-group').addEventListener('click', function() {
        if (selected.size === 0 || batchBusy) return;
        if (!confirm('Group ' + selected.size + ' selected asset(s) by filename stem? Assets with the same name will be merged. This cannot be undone.')) return;
        setButtonsBusy(true);
        MAKI.api.post('/api/batch/auto-group', {asset_ids: Array.from(selected)})
        .then(function(data) {
            clearSelection();
            refreshResults();
        })
        .catch(function(err) { console.error('Batch auto-group error:', err); })
        .finally(function() { setButtonsBusy(false); });
    });

    // Compare (navigate to compare page with 2-4 selected)
    document.getElementById('batch-compare').addEventListener('click', function() {
        if (selected.size < 2 || selected.size > 4) {
            alert('Select 2\u20134 assets to compare.');
            return;
        }
        window.location.href = '/compare?ids=' + Array.from(selected).join(',');
    });

    var describeBtn = document.getElementById('batch-describe');
    if (describeBtn) {
        describeBtn.addEventListener('click', function() {
            if (selected.size === 0 || batchBusy) return;
            if (!confirm('Describe ' + selected.size + ' selected asset(s) using VLM? This may take a few seconds per asset.')) return;
            var ids = Array.from(selected);
            var batchPayload = {asset_ids: ids};
            var batchModelSelect = document.getElementById('batch-vlm-model-select');
            if (batchModelSelect) batchPayload.model = batchModelSelect.value;
            MAKI.api.post('/api/batch/describe', batchPayload)
            .then(function(data) {
                clearSelection();
                window.makiJob.watch(data.job_id, 'Describe (' + ids.length + ')', {
                    onDone: function() {
                        htmx.ajax('GET', window.location.href, {target: '#results', swap: 'innerHTML'});
                    }
                });
            })
            .catch(function(err) {
                alert('VLM describe failed: ' + err.message);
            });
        });
    }

    var autotagBtn = document.getElementById('batch-autotag');
    if (autotagBtn) {
        autotagBtn.addEventListener('click', function() {
            if (selected.size === 0 || batchBusy) return;
            if (!confirm('Auto-tag ' + selected.size + ' selected asset(s)? Tags above the configured confidence threshold will be applied.')) return;
            var ids = Array.from(selected);
            // Async via job registry: POST returns a job_id, the toast shows
            // live progress and the final summary, then we refresh the grid
            // on done so the new tags surface in the UI.
            MAKI.api.post('/api/batch/auto-tag', {asset_ids: ids})
            .then(function(data) {
                clearSelection();
                window.makiJob.watch(data.job_id, 'Auto-tag (' + ids.length + ')', {
                    onDone: function() {
                        htmx.ajax('GET', window.location.href, {target: '#results', swap: 'innerHTML'});
                    }
                });
            })
            .catch(function(err) {
                alert('Auto-tag failed: ' + err.message);
            });
        });
    }

    var reviewTagsBtn = document.getElementById('batch-review-tags');
    if (reviewTagsBtn) {
        reviewTagsBtn.addEventListener('click', function() {
            if (batchBusy) return;
            // No-selection path: review every asset matching the current
            // filter (same /api/all-ids URL the Shift-Cmd-A selection uses).
            // Selected path: review just the chosen subset.
            var startReview = function(ids) {
                if (!ids || ids.length === 0) return;
                if (!confirm('Compute tag suggestions for ' + ids.length + ' asset(s)?\n\nThis runs the AI classifier on each image (roughly 1–2 s per image on CPU; faster on GPU). No tags are applied — you review each suggested tag\'s candidate set in the next step.')) return;
                MAKI.api.post('/api/maintain/suggest-tags-review', {asset_ids: ids})
                .then(function(data) {
                    // Open the review modal immediately in its "computing"
                    // phase — it subscribes to the job's progress stream
                    // itself and transitions to the review UI on `done`.
                    // No corner toast for this job kind: the modal IS
                    // the feedback surface, which matters because the AI
                    // model can take 30–60 s to load on first request
                    // and a small toast in the corner is easy to miss.
                    if (window.makiTagReview && window.makiTagReview.open) {
                        window.makiTagReview.open(data.job_id);
                    }
                })
                .catch(function(err) {
                    alert('Suggest-tags failed: ' + err.message);
                });
            };

            if (selected.size > 0) {
                startReview(Array.from(selected));
                return;
            }
            // No manual selection — pull every ID matching the live filter.
            MAKI.api.get('/api/all-ids' + window.location.search)
                .then(function(data) {
                    startReview(data.ids || []);
                })
                .catch(function(err) {
                    alert('Could not resolve the current filter: ' + err.message);
                });
        });
    }

    var embedBtn = document.getElementById('batch-embed');
    if (embedBtn) {
        embedBtn.addEventListener('click', function() {
            if (selected.size === 0 || batchBusy) return;
            if (!confirm('Generate embeddings for ' + selected.size + ' selected asset(s)?\n\nNo tags will be applied — this only builds the visual-similarity index.')) return;
            var ids = Array.from(selected);
            MAKI.api.post('/api/batch/embed', {asset_ids: ids})
            .then(function(data) {
                clearSelection();
                window.makiJob.watch(data.job_id, 'Embed (' + ids.length + ')');
            })
            .catch(function(err) {
                alert('Embed failed: ' + err.message);
            });
        });
    }

    var detectFacesBtn = document.getElementById('batch-detect-faces');
    if (detectFacesBtn) {
        detectFacesBtn.addEventListener('click', function() {
            if (selected.size === 0 || batchBusy) return;
            if (!confirm('Detect faces in ' + selected.size + ' selected asset(s)?')) return;
            var ids = Array.from(selected);
            MAKI.api.post('/api/batch/detect-faces', {asset_ids: ids})
            .then(function(data) {
                clearSelection();
                window.makiJob.watch(data.job_id, 'Detect faces (' + ids.length + ')', {
                    onDone: function() {
                        htmx.ajax('GET', window.location.href, {target: '#results', swap: 'innerHTML'});
                    }
                });
            })
            .catch(function(err) {
                alert('Detect faces failed: ' + err.message);
            });
        });
    }

    // --- Export ZIP ---
    function showExportModal(mode, ids, filters) {
        var overlay = document.createElement('div');
        overlay.className = 'group-confirm-overlay';
        var count = mode === 'selection' ? ids.length : 0;
        var title = mode === 'selection'
            ? 'Export ' + count + ' selected asset' + (count === 1 ? '' : 's')
            : 'Export all results';
        var defaultName = mode === 'selection'
            ? 'maki-export-' + count + '-assets'
            : 'maki-export';
        overlay.innerHTML = '<div class="group-confirm-modal export-modal">' +
            '<h3>' + title + '</h3>' +
            '<div class="export-options">' +
                '<div class="export-row"><label>Filename</label>' +
                    '<span class="export-filename-wrap"><input type="text" id="export-filename" value="' + defaultName + '">.zip</span>' +
                '</div>' +
                '<div class="export-row"><label>Layout</label>' +
                    '<label class="export-radio"><input type="radio" name="export-layout" value="flat" checked> Flat</label>' +
                    '<label class="export-radio"><input type="radio" name="export-layout" value="mirror"> Mirror</label>' +
                '</div>' +
                '<div class="export-row"><label>Content</label>' +
                    '<label class="export-radio" title="Original media files (volumes must be online)"><input type="radio" name="export-source" value="originals" checked> Originals</label>' +
                    '<label class="export-radio" title="Preview images — works with offline volumes"><input type="radio" name="export-source" value="previews"> Previews</label>' +
                    '<label class="export-radio" title="High-resolution smart previews — works with offline volumes"><input type="radio" name="export-source" value="smart"> Smart previews</label>' +
                '</div>' +
                '<div class="export-row"><label><input type="checkbox" id="export-all-variants"> All variants</label></div>' +
                '<div class="export-row"><label><input type="checkbox" id="export-sidecars"> Include sidecars (.xmp, .cos, \u2026)</label></div>' +
            '</div>' +
            '<div class="group-confirm-buttons">' +
                '<button class="group-confirm-cancel">Cancel</button>' +
                '<button class="group-confirm-ok export-download-btn">Download .zip</button>' +
            '</div>' +
        '</div>';
        document.body.appendChild(overlay);
        setupModalKeyboard(overlay, '.export-download-btn');

        overlay.querySelector('.group-confirm-cancel').addEventListener('click', function() {
            overlay.remove();
        });
        overlay.addEventListener('click', function(e) {
            if (e.target === overlay) overlay.remove();
        });

        // Previews are generated for the best variant only — "All variants"
        // makes no sense there, so disable it when a preview source is picked.
        overlay.querySelectorAll('input[name="export-source"]').forEach(function(radio) {
            radio.addEventListener('change', function() {
                var isOriginals = overlay.querySelector('input[name="export-source"]:checked').value === 'originals';
                var av = document.getElementById('export-all-variants');
                av.disabled = !isOriginals;
                if (!isOriginals) av.checked = false;
            });
        });

        overlay.querySelector('.export-download-btn').addEventListener('click', function() {
            var btn = this;
            var layout = overlay.querySelector('input[name="export-layout"]:checked').value;
            var source = overlay.querySelector('input[name="export-source"]:checked').value;
            var allVariants = document.getElementById('export-all-variants').checked;
            var sidecars = document.getElementById('export-sidecars').checked;

            var body = {layout: layout, source: source, all_variants: allVariants, include_sidecars: sidecars};
            if (mode === 'selection') {
                body.asset_ids = ids;
            } else {
                body.filters = filters || {};
            }

            btn.disabled = true;
            btn.textContent = 'Preparing\u2026';

            fetch('/api/batch/export', {
                method: 'POST',
                headers: {'Content-Type': 'application/json'},
                body: JSON.stringify(body)
            })
            .then(function(r) {
                if (!r.ok) {
                    return r.text().then(function(t) { throw new Error(t); });
                }
                var exported = parseInt(r.headers.get('X-Maki-Exported') || '0', 10);
                var skipped = parseInt(r.headers.get('X-Maki-Skipped') || '0', 10);
                var skippedOffline = parseInt(r.headers.get('X-Maki-Skipped-Offline') || '0', 10);
                return r.blob().then(function(blob) {
                    var userFilename = (document.getElementById('export-filename').value || 'maki-export').trim();
                    if (!userFilename.endsWith('.zip')) userFilename += '.zip';
                    var url = URL.createObjectURL(blob);
                    var a = document.createElement('a');
                    a.href = url;
                    a.download = userFilename;
                    a.click();
                    URL.revokeObjectURL(url);
                    overlay.remove();
                    if (skipped > 0) {
                        var msg = 'Export incomplete: ' + skipped + ' file' + (skipped === 1 ? '' : 's') + ' could not be included';
                        if (skippedOffline > 0) {
                            msg += ' (' + skippedOffline + ' on offline volume' + (skippedOffline === 1 ? '' : 's') + ')';
                        }
                        msg += '. The ZIP contains ' + exported + ' file' + (exported === 1 ? '' : 's') + '.';
                        alert(msg);
                    }
                });
            })
            .catch(function(err) {
                btn.disabled = false;
                btn.textContent = 'Download .zip';
                alert('Export failed: ' + err.message);
            });
        });
    }

    document.getElementById('batch-export').addEventListener('click', function() {
        if (selected.size === 0 || batchBusy) return;
        showExportModal('selection', Array.from(selected), null);
    });

    // Collect current browse filter params from the URL
    function getFilterParams() {
        var params = new URLSearchParams(window.location.search);
        var filters = {};
        ['q', 'type', 'tag', 'format', 'volume', 'rating', 'label', 'collection', 'path', 'person', 'stacks', 'nodefault'].forEach(function(k) {
            var v = params.get(k);
            if (v) filters[k] = v;
        });
        return filters;
    }

    // "Export all" link — delegated since results are loaded via htmx
    document.addEventListener('click', function(e) {
        var link = e.target.closest('#export-all-link');
        if (!link) return;
        e.preventDefault();
        showExportModal('all', null, getFilterParams());
    });

    // Batch delete — confirmation modal with thumbnails
    document.getElementById('batch-delete').addEventListener('click', function() {
        if (selected.size === 0 || batchBusy) return;
        var ids = Array.from(selected);
        var overlay = document.createElement('div');
        overlay.className = 'group-confirm-overlay';
        overlay.innerHTML = '<div class="group-confirm-modal">' +
            '<h3>Delete ' + ids.length + ' asset' + (ids.length === 1 ? '' : 's') + '?</h3>' +
            '<p class="group-confirm-hint">This removes assets from the catalog and their sidecar files. This cannot be undone.</p>' +
            '<div class="export-options">' +
                '<div class="export-row"><label><input type="checkbox" id="delete-remove-files"> Also delete media files from disk</label></div>' +
            '</div>' +
            '<div class="group-confirm-grid"></div>' +
            '<div class="group-confirm-buttons">' +
                '<button class="group-confirm-cancel">Cancel</button>' +
                '<button class="group-confirm-ok delete-confirm-btn">Delete</button>' +
            '</div></div>';

        var grid = overlay.querySelector('.group-confirm-grid');
        ids.forEach(function(id) {
            var card = document.querySelector('.asset-card[data-asset-id="' + id + '"]');
            var thumb = document.createElement('div');
            thumb.className = 'group-confirm-thumb';
            var img = card ? card.querySelector('img') : null;
            var src = img ? img.src : '';
            var nameEl = card ? card.querySelector('.card-title') : null;
            var name = nameEl ? nameEl.textContent.trim() : id.substring(0, 8) + '\u2026';
            thumb.innerHTML = (src ? '<img src="' + src + '" alt="">' : '<div class="group-confirm-placeholder">' + id.substring(0, 8) + '</div>') +
                '<span class="group-confirm-name">' + name + '</span>';
            grid.appendChild(thumb);
        });

        document.body.appendChild(overlay);
        setupModalKeyboard(overlay, '.delete-confirm-btn');

        overlay.querySelector('.group-confirm-cancel').addEventListener('click', function() { overlay.remove(); });
        overlay.addEventListener('click', function(e) { if (e.target === overlay) overlay.remove(); });

        overlay.querySelector('.delete-confirm-btn').addEventListener('click', function() {
            var removeFiles = document.getElementById('delete-remove-files').checked;
            overlay.remove();
            setButtonsBusy(true);
            MAKI.api.post('/api/batch/delete', {asset_ids: ids, remove_files: removeFiles})
            .then(function(data) {
                clearSelection();
                refreshResults();
                var msg = data.deleted + ' asset' + (data.deleted === 1 ? '' : 's') + ' deleted';
                if (data.files_removed > 0) msg += ', ' + data.files_removed + ' file' + (data.files_removed === 1 ? '' : 's') + ' removed';
                if (data.errors && data.errors.length > 0) msg += ' (' + data.errors.length + ' error' + (data.errors.length === 1 ? '' : 's') + ')';
                alert(msg);
            })
            .catch(function(err) { alert('Delete failed: ' + err.message); })
            .finally(function() { setButtonsBusy(false); });
        });
    });

    // Persist selection to sessionStorage so it survives fresh page loads (back-nav)
    window.addEventListener('pagehide', function() {
        if (selected.size > 0) {
            sessionStorage.setItem('maki-browse-selection', JSON.stringify(Array.from(selected)));
        } else {
            sessionStorage.removeItem('maki-browse-selection');
        }
    });

    // Apply restored selection to DOM (must run after initial render).
    // updateToolbar() now manages the persistent "off-page" pill itself
    // based on which selected IDs are actually rendered as cards.
    if (selected.size > 0) updateToolbar();

    // One-shot pulse on the count badge when the page loads with a
    // selection that came from a non-detail navigation (Tags / Stats /
    // Maintain / …). This is the attention-grabber on initial return;
    // the persistent visibility cue is the off-page pill driven by
    // updateToolbar(). Detail-page round trips don't pulse — that's
    // the normal browsing flow.
    if (restoredFromAway && selected.size > 0 && countEl) {
        countEl.classList.add('pulse-restored');
        setTimeout(function() {
            countEl.classList.remove('pulse-restored');
        }, 2200);
    }

    // Expose for keyboard navigation integration
    window.damBatch = {
        selected: selected,
        updateToolbar: updateToolbar,
        clearSelection: clearSelection,
        getVisibleIds: getVisibleIds,
        refreshResults: refreshResults,
        setButtonsBusy: setButtonsBusy,
        isBusy: function() { return batchBusy; }
    };
})();
