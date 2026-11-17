// Browse page: keyboard navigation.
//
// The focused card (arrow keys, restored when coming back from the detail
// page or the lightbox) and the page's keyboard shortcuts. Loaded by
// browse.html from /static/browse/keyboard.js, after batch.js and the
// lightbox bridge; exposes window.damKeyNav. Reaches the other sections
// only through window.* (damBatch, damLightbox, damCalendar, damFacets,
// damFilterToggle, damHelp), all looked up when a key is pressed.
//
// Plain ES5, no template expressions.
(function() {
    var focusedIndex = -1;

    function getCards() {
        return Array.from(document.querySelectorAll('.asset-card'));
    }

    function getColCount() {
        var grid = document.querySelector('.results-grid');
        if (!grid) return 1;
        return getComputedStyle(grid).gridTemplateColumns.split(' ').length;
    }

    function setFocus(index) {
        var cards = getCards();
        var old = document.querySelector('.asset-card.focused');
        if (old) old.classList.remove('focused');
        if (cards.length === 0) { focusedIndex = -1; return; }
        focusedIndex = Math.max(0, Math.min(index, cards.length - 1));
        cards[focusedIndex].classList.add('focused');
        cards[focusedIndex].scrollIntoView({ block: 'nearest' });
    }

    function clearFocus() {
        var old = document.querySelector('.asset-card.focused');
        if (old) old.classList.remove('focused');
        focusedIndex = -1;
    }

    function getFocusedId() {
        var cards = getCards();
        if (focusedIndex < 0 || focusedIndex >= cards.length) return null;
        return cards[focusedIndex].getAttribute('data-asset-id');
    }

    function getTargetIds() {
        if (window.damBatch && window.damBatch.selected.size > 0) {
            return Array.from(window.damBatch.selected);
        }
        var id = getFocusedId();
        return id ? [id] : [];
    }

    function refreshResults() {
        htmx.ajax('GET', window.location.href, {target: '#results', swap: 'innerHTML'});
    }

    // Rating: 0 clears, 1-5 sets
    function applyRating(rating) {
        var ids = getTargetIds();
        if (ids.length === 0) return;
        if (window.damBatch && window.damBatch.isBusy()) return;
        if (ids.length === 1 && window.damBatch.selected.size === 0) {
            // Single-asset via focused card
            fetch('/api/asset/' + ids[0] + '/rating', {
                method: 'PUT',
                headers: {'Content-Type': 'application/x-www-form-urlencoded'},
                body: 'rating=' + rating
            }).then(function() { refreshResults(); });
        } else {
            // Batch
            window.damBatch.setButtonsBusy(true);
            MAKI.api.put('/api/batch/rating', {asset_ids: ids, rating: rating || null})
            .then(function() {
                refreshResults();
            })
            .catch(function(err) { console.error('Batch rating error:', err); })
            .finally(function() { window.damBatch.setButtonsBusy(false); });
        }
    }

    // Label: empty string clears, color name sets
    var labelNumMap = MAKI.labelNumMap;
    var labelLetterMap = MAKI.labelLetterMap;

    function applyLabel(label) {
        var ids = getTargetIds();
        if (ids.length === 0) return;
        if (window.damBatch && window.damBatch.isBusy()) return;
        if (ids.length === 1 && window.damBatch.selected.size === 0) {
            // Single-asset via focused card
            fetch('/api/asset/' + ids[0] + '/label', {
                method: 'PUT',
                headers: {'Content-Type': 'application/x-www-form-urlencoded'},
                body: 'label=' + encodeURIComponent(label)
            }).then(function() { refreshResults(); });
        } else {
            // Batch
            window.damBatch.setButtonsBusy(true);
            MAKI.api.put('/api/batch/label', {asset_ids: ids, label: label || null})
            .then(function() {
                refreshResults();
            })
            .catch(function(err) { console.error('Batch label error:', err); })
            .finally(function() { window.damBatch.setButtonsBusy(false); });
        }
    }

    document.addEventListener('keydown', function(e) {
        // Don't handle keys while lightbox is open (it captures its own)
        if (window.damLightbox && window.damLightbox.isOpen()) return;
        // Don't handle keys while help panel is open
        if (window.damHelp && window.damHelp.isOpen()) return;

        // Ignore when typing in a text input/textarea/select
        var el = e.target;
        if (el.tagName === 'TEXTAREA' || el.tagName === 'SELECT') return;
        if (el.tagName === 'INPUT' && el.type !== 'checkbox') return;

        var currentView = document.documentElement.getAttribute('data-view') || 'grid';

        var cards = getCards();
        if (cards.length === 0 && currentView === 'grid') return;

        // Escape: clear selection first, then focus
        if (e.key === 'Escape') {
            if (window.damBatch && window.damBatch.selected.size > 0) {
                window.damBatch.clearSelection();
            } else if (focusedIndex >= 0) {
                clearFocus();
            }
            return;
        }

        // Shift+Arrow: page turn
        if (e.shiftKey && (e.key === 'ArrowLeft' || e.key === 'ArrowRight')) {
            e.preventDefault();
            var link = e.key === 'ArrowLeft'
                ? document.querySelector('.top-pagination a[href]:nth-child(2)')
                : document.querySelector('.top-pagination a[href]:nth-last-child(2)');
            if (link) link.click();
            return;
        }

        // Arrow keys — grid view only
        if (currentView === 'grid' && (e.key === 'ArrowLeft' || e.key === 'ArrowRight' || e.key === 'ArrowUp' || e.key === 'ArrowDown')) {
            e.preventDefault();
            if (focusedIndex < 0) {
                setFocus(0);
                return;
            }
            var cols = getColCount();
            if (e.key === 'ArrowLeft') setFocus(focusedIndex - 1);
            else if (e.key === 'ArrowRight') setFocus(focusedIndex + 1);
            else if (e.key === 'ArrowUp') setFocus(focusedIndex - cols);
            else if (e.key === 'ArrowDown') setFocus(focusedIndex + cols);
            return;
        }

        // Enter or l: open lightbox for focused card
        if ((e.key === 'Enter' || e.key === 'l') && !e.metaKey && !e.ctrlKey && !e.altKey) {
            if (focusedIndex >= 0 && window.damLightbox) {
                e.preventDefault();
                window.damLightbox.open(focusedIndex);
            }
            return;
        }

        // d: open detail page for focused card
        if (e.key === 'd' && !e.metaKey && !e.ctrlKey && !e.altKey) {
            var focId = getFocusedId();
            if (focId) {
                e.preventDefault();
                sessionStorage.setItem('maki-browse-focus', focId);
                window.location.href = '/asset/' + focId;
            }
            return;
        }

        // s: stroll from focused card
        if (e.key === 's' && !e.metaKey && !e.ctrlKey && !e.altKey) {
            var focId = getFocusedId();
            if (focId) {
                e.preventDefault();
                window.location.href = '/stroll?id=' + focId;
            }
            return;
        }

        // Space: toggle selection of focused card
        if (e.key === ' ') {
            var focId = getFocusedId();
            if (focId && window.damBatch) {
                e.preventDefault();
                if (window.damBatch.selected.has(focId)) {
                    window.damBatch.selected.delete(focId);
                } else {
                    window.damBatch.selected.add(focId);
                }
                window.damBatch.updateToolbar();
            }
            return;
        }

        // Alt/Option+0-7: color label (use e.code for macOS compatibility)
        if (e.altKey && e.code >= 'Digit0' && e.code <= 'Digit7') {
            e.preventDefault();
            var num = e.code.charAt(5);
            if (num === '0') {
                applyLabel('');
            } else {
                applyLabel(labelNumMap[num] || '');
            }
            return;
        }

        // 0-5: rating (no modifier)
        if (!e.metaKey && !e.ctrlKey && !e.altKey && e.key >= '0' && e.key <= '5') {
            applyRating(parseInt(e.key));
            return;
        }

        // Letter keys for color label (no modifier)
        if (!e.metaKey && !e.ctrlKey && !e.altKey) {
            var lower = e.key.toLowerCase();
            if (labelLetterMap[lower]) {
                applyLabel(labelLetterMap[lower]);
                return;
            }
            if (lower === 'x') {
                applyLabel('');
                return;
            }
            // c: compare selected (2-4)
            if (lower === 'c' && window.damBatch && window.damBatch.selected.size >= 2 && window.damBatch.selected.size <= 4) {
                window.location.href = '/compare?ids=' + Array.from(window.damBatch.selected).join(',');
                return;
            }
            // f: facet sidebar toggle
            if (lower === 'f' && !e.shiftKey && window.damFacets) {
                window.damFacets.toggle();
                return;
            }
            // Shift+F: filter bar toggle
            if (lower === 'f' && e.shiftKey && window.damFilterToggle) {
                window.damFilterToggle();
                return;
            }
            // m: map view toggle
            if (lower === 'm' && window.damCalendar) {
                var cur = document.documentElement.getAttribute('data-view') || 'grid';
                window.damCalendar.setView(cur === 'map' ? 'grid' : 'map');
                return;
            }
        }
    });

    // Re-apply focus after htmx swaps (pagination/sort/search)
    var focusedAssetId = null;
    document.body.addEventListener('htmx:beforeSwap', function(e) {
        if (e.detail.target && e.detail.target.id === 'results') {
            // Save focused asset ID before the swap replaces DOM
            var cards = getCards();
            if (focusedIndex >= 0 && focusedIndex < cards.length) {
                focusedAssetId = cards[focusedIndex].getAttribute('data-asset-id');
            }
        }
    });
    document.body.addEventListener('htmx:afterSwap', function(e) {
        if (e.detail.target && e.detail.target.id === 'results') {
            // Restore focus by asset ID, fall back to clamped index
            if (focusedAssetId) {
                var cards = getCards();
                var found = false;
                for (var i = 0; i < cards.length; i++) {
                    if (cards[i].getAttribute('data-asset-id') === focusedAssetId) {
                        focusedIndex = i;
                        cards[i].classList.add('focused');
                        found = true;
                        break;
                    }
                }
                if (!found) {
                    if (focusedIndex >= cards.length) focusedIndex = cards.length - 1;
                    if (focusedIndex >= 0 && cards.length > 0) cards[focusedIndex].classList.add('focused');
                }
                focusedAssetId = null;
            } else {
                var cards = getCards();
                if (focusedIndex >= cards.length) focusedIndex = cards.length - 1;
                if (focusedIndex >= 0 && cards.length > 0) cards[focusedIndex].classList.add('focused');
            }
        }
    });

    window.damKeyNav = {
        setFocusById: function(assetId) {
            if (!assetId) return;
            var cards = getCards();
            for (var i = 0; i < cards.length; i++) {
                if (cards[i].getAttribute('data-asset-id') === assetId) {
                    setFocus(i);
                    return;
                }
            }
        },
        getFocusedId: getFocusedId
    };

    // Restore focus on fresh page load (non-bfcache back-nav)
    var initFocusId = sessionStorage.getItem('maki-browse-focus');
    if (initFocusId) {
        var initCards = getCards();
        for (var fi = 0; fi < initCards.length; fi++) {
            if (initCards[fi].getAttribute('data-asset-id') === initFocusId) {
                setFocus(fi);
                // Use 'center' to approximate scroll restore
                initCards[fi].scrollIntoView({ block: 'center', behavior: 'instant' });
                break;
            }
        }
    }
})();
