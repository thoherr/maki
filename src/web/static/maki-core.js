// MAKI web UI core helpers — one copy of the utilities every page script
// used to re-implement inline (F22.1 of the 2026-10-29 architecture audit).
//
// Exposed as a single global, `window.MAKI`:
//
//   MAKI.escHtml(s) / MAKI.escAttr(s)   HTML-escape for text and attributes
//   MAKI.toast(msg, kind, ms, label)    one-shot floating notification
//   MAKI.api.get/post/put/del(url, body) JSON fetch, throws on non-2xx
//   MAKI.dialog.open(el)/close(el)      flex overlay show/hide
//   MAKI.pathAutocomplete(input, opts)  hierarchical completion dropdown
//   MAKI.attachJobStream(jobId, h)      SSE subscription to a job's progress
//   MAKI.reattachIfRunning(match, fn)   find a running job via /api/jobs
//   MAKI.quoteFilterValue(v)            filter-syntax quoting rule
//   MAKI.labels / labelHex / labelNumMap / labelLetterMap
//                                       colour-label vocabulary (set from
//                                       the data island in base.html)
//
// The file is embedded into base.html at compile time (templates.rs
// `maki_core_js` filter) so it runs before any inline page script.
// Plain ES5 on purpose: the inline scripts it serves are ES5 too.
(function (global) {
    'use strict';

    var MAKI = global.MAKI || {};

    // ── Escaping ───────────────────────────────────────────────────────
    // Strictest of the former inline copies: all five HTML-significant
    // characters, so one function is safe for both text nodes and
    // double- or single-quoted attribute values. null/undefined → ''.
    var ESC_MAP = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
    function escHtml(s) {
        return (s == null ? '' : String(s)).replace(/[&<>"']/g, function (c) { return ESC_MAP[c]; });
    }
    function escAttr(s) { return escHtml(s); }

    // ── Toast ──────────────────────────────────────────────────────────
    // kind: 'success' | 'warn' | 'error' | 'info' → the .job-toast chrome
    //       with a .config-save-toast-<kind> accent (bottom-right, above
    //       dialogs); optional `label` is the bold first line.
    //       'drop' → the lightweight .drop-toast used for drag-and-drop
    //       feedback (fades in/out via the .show class).
    // ms:   auto-dismiss delay; 0 keeps the toast until replaced.
    // Single-instance per kind family: a new toast replaces the previous.
    function toast(msg, kind, ms, label) {
        kind = kind || 'success';
        if (kind === 'drop') {
            var dt = document.createElement('div');
            dt.className = 'drop-toast';
            dt.textContent = msg;
            document.body.appendChild(dt);
            requestAnimationFrame(function () { dt.classList.add('show'); });
            setTimeout(function () {
                dt.classList.remove('show');
                setTimeout(function () { dt.remove(); }, 300);
            }, ms == null ? 2000 : ms);
            return dt;
        }
        var existing = document.getElementById('maki-toast');
        if (existing) existing.remove();
        var el = document.createElement('div');
        el.id = 'maki-toast';
        el.className = 'job-toast config-save-toast config-save-toast-' + kind;
        el.style.display = 'block';
        el.innerHTML =
            (label ? '<div class="job-toast-label">' + escHtml(label) + '</div>' : '') +
            '<div class="job-toast-status">' + escHtml(msg) + '</div>';
        document.body.appendChild(el);
        if (ms == null) ms = kind === 'error' ? 8000 : 4000;
        if (ms > 0) setTimeout(function () { if (el.parentNode) el.remove(); }, ms);
        return el;
    }

    // ── JSON API ───────────────────────────────────────────────────────
    // Resolves with the parsed JSON body (null for 204 / empty). Rejects
    // with an Error whose message is the response text (or "HTTP <code>")
    // and whose .status carries the status code. Form posts, SSE and blob
    // downloads keep using fetch() directly.
    function request(method, url, body, opts) {
        var init = { method: method, headers: {} };
        if (opts && opts.headers) {
            for (var h in opts.headers) if (Object.prototype.hasOwnProperty.call(opts.headers, h)) init.headers[h] = opts.headers[h];
        }
        if (opts && opts.signal) init.signal = opts.signal;
        if (body !== undefined && body !== null) {
            init.headers['Content-Type'] = 'application/json';
            init.body = JSON.stringify(body);
        }
        return fetch(url, init).then(function (r) {
            return r.text().then(function (text) {
                if (!r.ok) {
                    var err = new Error(text || ('HTTP ' + r.status));
                    err.status = r.status;
                    throw err;
                }
                if (r.status === 204 || text === '') return null;
                try { return JSON.parse(text); } catch (_) { return text; }
            });
        });
    }
    var api = {
        get: function (url, opts) { return request('GET', url, undefined, opts); },
        post: function (url, body, opts) { return request('POST', url, body, opts); },
        put: function (url, body, opts) { return request('PUT', url, body, opts); },
        del: function (url, body, opts) { return request('DELETE', url, body, opts); }
    };

    // ── Dialog overlays ────────────────────────────────────────────────
    var dialog = {
        open: function (el) { if (el) el.style.display = 'flex'; },
        close: function (el) { if (el) el.style.display = 'none'; },
        isOpen: function (el) { return !!el && el.style.display !== 'none'; }
    };

    // ── Path autocomplete ──────────────────────────────────────────────
    // Hierarchical completion dropdown shared by the filter bar (path
    // filter), the import dialog (subfolder) and the maintain dialog
    // (sync / cleanup paths). Debounced fetch, ↑/↓ to move, Tab/Enter to
    // accept (drill into a directory, commit a file), Escape to close.
    //
    // opts:
    //   container   the dropdown element (.tag-suggestion rows are built
    //               inside it; CSS from style.css applies)
    //   key(value)  dedupe key for a query; null/undefined → just hide
    //   fetch(value) → Promise<items> | null (null → hide). Each item is
    //               { value, isDir } plus whatever render() needs.
    //   render(item, inputValue) → HTML string for a row (optional;
    //               default: escaped item.value)
    //   onCommit(item)  accepted item that is not a directory
    //   onEnter()   Enter pressed while no suggestion list is open
    //   resetOn     array of elements whose 'change' clears the dedupe key
    //   stopPropagation  swallow keydown/keyup so page shortcuts stay quiet
    //   debounceMs  default 120
    function pathAutocomplete(input, opts) {
        // opts.source may carry key/fetch/render (see volumeBrowseSource).
        var src = opts.source || {};
        opts = {
            container: opts.container, key: opts.key || src.key, fetch: opts.fetch || src.fetch,
            render: opts.render || src.render, onCommit: opts.onCommit, onEnter: opts.onEnter,
            resetOn: opts.resetOn, stopPropagation: opts.stopPropagation, debounceMs: opts.debounceMs
        };
        var ac = opts.container;
        if (!input || !ac) return null;
        var index = -1;
        var lastKey = null;
        var timer = null;
        var debounceMs = opts.debounceMs == null ? 120 : opts.debounceMs;

        function hide() { ac.style.display = 'none'; index = -1; }

        function items() { return ac.querySelectorAll('.tag-suggestion'); }

        function highlight(idx) {
            var rows = items();
            rows.forEach(function (el, i) { el.classList.toggle('active', i === idx); });
            if (idx >= 0 && rows[idx]) rows[idx].scrollIntoView({ block: 'nearest' });
        }

        function render(list, inputValue) {
            ac.innerHTML = '';
            index = -1;
            if (!list || list.length === 0) { hide(); return; }
            list.forEach(function (item) {
                var div = document.createElement('div');
                div.className = 'tag-suggestion';
                div.setAttribute('data-path', item.value);
                div.setAttribute('data-is-dir', item.isDir ? '1' : '0');
                if (opts.render) div.innerHTML = opts.render(item, inputValue);
                else div.textContent = item.value;
                div.onmousedown = function (e) {
                    e.preventDefault();
                    accept(item.value, !!item.isDir);
                };
                ac.appendChild(div);
            });
            ac.style.display = 'block';
        }

        function accept(value, isDir) {
            input.value = value;
            hide();
            if (isDir) {
                // Leave the cursor at the end so the next keystroke
                // continues at the next level; query that level now.
                input.focus();
                load(value);
            } else if (opts.onCommit) {
                opts.onCommit({ value: value, isDir: false });
            }
        }

        function load(value) {
            var key = opts.key ? opts.key(value) : value;
            if (key == null) { hide(); return; }
            // A rapid sequence of the same query (e.g. a spurious input
            // event) must not re-fetch.
            if (key === lastKey) return;
            lastKey = key;
            var p = opts.fetch(value);
            if (!p) { hide(); return; }
            p.then(function (list) {
                // Only render if the input still matches what we asked
                // for — avoids late responses overwriting fresher ones.
                if (input.value !== value) return;
                render(list, value);
            }).catch(function () { hide(); });
        }

        function schedule(value) {
            if (timer) clearTimeout(timer);
            timer = setTimeout(function () { load(value); }, debounceMs);
        }

        input.addEventListener('input', function () { schedule(this.value); });
        input.addEventListener('focus', function () {
            // Show suggestions immediately on focus (even with an empty
            // value) and drop the cache so a refocus after a context
            // change (volume switch) refetches.
            lastKey = null;
            load(this.value);
        });
        input.addEventListener('blur', function () {
            // Delay hide so an onmousedown on a suggestion can still fire.
            setTimeout(hide, 150);
        });
        (opts.resetOn || []).forEach(function (el) {
            if (el) el.addEventListener('change', function () { lastKey = null; });
        });

        input.addEventListener('keydown', function (e) {
            var rows = items();
            var open = ac.style.display === 'block' && rows.length > 0;
            if (e.key === 'ArrowDown' && open) {
                e.preventDefault();
                index = Math.min(index + 1, rows.length - 1);
                highlight(index);
            } else if (e.key === 'ArrowUp' && open) {
                e.preventDefault();
                index = Math.max(index - 1, 0);
                highlight(index);
            } else if ((e.key === 'Tab' || e.key === 'Enter') && open && index >= 0) {
                // Tab = accept current highlight, stay in field (shell-style).
                // Enter = accept too; a directory completion already carries
                // the trailing `/` for the next level.
                e.preventDefault();
                var el = rows[index];
                accept(el.getAttribute('data-path'), el.getAttribute('data-is-dir') === '1');
            } else if (e.key === 'Enter' && !open) {
                if (opts.onEnter) opts.onEnter();
            } else if (e.key === 'Escape' && open) {
                e.preventDefault();
                hide();
            }
            if (opts.stopPropagation) e.stopPropagation();
        });
        if (opts.stopPropagation) {
            input.addEventListener('keyup', function (e) { e.stopPropagation(); });
        }

        return { hide: hide, reset: function () { lastKey = null; } };
    }

    // Suggestion source for pathAutocomplete backed by
    // /api/volumes/{id}/browse (import subfolder, maintain sync/cleanup
    // paths). `volumeId()` returns the current volume UUID, or ''/null
    // when none is selected (then nothing is suggested). Directories are
    // offered with a trailing `/` so accepting one drills into it.
    function volumeBrowseSource(volumeId) {
        function splitPath(p) {
            var i = p.lastIndexOf('/');
            return i < 0 ? { dir: '', leaf: p } : { dir: p.substring(0, i + 1), leaf: p.substring(i + 1) };
        }
        return {
            key: function (value) {
                var volId = volumeId();
                if (!volId) return null;
                var s = splitPath(value);
                return volId + '|' + s.dir.replace(/\/+$/, '') + '|' + s.leaf;
            },
            fetch: function (value) {
                var volId = volumeId();
                if (!volId) return null;
                var s = splitPath(value);
                var prefix = s.dir.replace(/\/+$/, '');
                var leafLc = s.leaf.toLowerCase();
                var url = '/api/volumes/' + encodeURIComponent(volId) +
                    '/browse?limit=50&prefix=' + encodeURIComponent(prefix);
                return api.get(url).then(function (data) {
                    return ((data && data.entries) || []).filter(function (e) {
                        return e.name.toLowerCase().indexOf(leafLc) === 0;
                    }).map(function (e) {
                        return { value: e.is_dir ? e.relative_path + '/' : e.relative_path, isDir: !!e.is_dir, name: e.name };
                    });
                });
            },
            render: function (item) {
                return '<span class="path-entry-icon">' + (item.isDir ? '\uD83D\uDCC1' : '\uD83D\uDCC4') + '</span>' +
                    escHtml(item.name) + (item.isDir ? '/' : '');
            }
        };
    }

    // Suggestion source for pathAutocomplete backed by
    // /api/volumes/{id}/browse (import subfolder, maintain sync/cleanup
    // paths). `volumeId()` returns the current volume UUID, or ''/null
    // when none is selected (then nothing is suggested). Directories are
    // offered with a trailing `/` so accepting one drills into it.
    function volumeBrowseSource(volumeId) {
        function splitPath(p) {
            var i = p.lastIndexOf('/');
            return i < 0 ? { dir: '', leaf: p } : { dir: p.substring(0, i + 1), leaf: p.substring(i + 1) };
        }
        return {
            key: function (value) {
                var volId = volumeId();
                if (!volId) return null;
                var s = splitPath(value);
                return volId + '|' + s.dir.replace(/\/+$/, '') + '|' + s.leaf;
            },
            fetch: function (value) {
                var volId = volumeId();
                if (!volId) return null;
                var s = splitPath(value);
                var prefix = s.dir.replace(/\/+$/, '');
                var leafLc = s.leaf.toLowerCase();
                var url = '/api/volumes/' + encodeURIComponent(volId) +
                    '/browse?limit=50&prefix=' + encodeURIComponent(prefix);
                return api.get(url).then(function (data) {
                    return ((data && data.entries) || []).filter(function (e) {
                        return e.name.toLowerCase().indexOf(leafLc) === 0;
                    }).map(function (e) {
                        return { value: e.is_dir ? e.relative_path + '/' : e.relative_path, isDir: !!e.is_dir, name: e.name };
                    });
                });
            },
            render: function (item) {
                return '<span class="path-entry-icon">' + (item.isDir ? '\uD83D\uDCC1' : '\uD83D\uDCC4') + '</span>' +
                    escHtml(item.name) + (item.isDir ? '/' : '');
            }
        };
    }

    // Suggestion source for pathAutocomplete backed by
    // /api/volumes/{id}/browse (import subfolder, maintain sync/cleanup
    // paths). `volumeId()` returns the current volume UUID, or ''/null
    // when none is selected (then nothing is suggested). Directories are
    // offered with a trailing `/` so accepting one drills into it.
    function volumeBrowseSource(volumeId) {
        function splitPath(p) {
            var i = p.lastIndexOf('/');
            return i < 0 ? { dir: '', leaf: p } : { dir: p.substring(0, i + 1), leaf: p.substring(i + 1) };
        }
        return {
            key: function (value) {
                var volId = volumeId();
                if (!volId) return null;
                var s = splitPath(value);
                return volId + '|' + s.dir.replace(/\/+$/, '') + '|' + s.leaf;
            },
            fetch: function (value) {
                var volId = volumeId();
                if (!volId) return null;
                var s = splitPath(value);
                var prefix = s.dir.replace(/\/+$/, '');
                var leafLc = s.leaf.toLowerCase();
                var url = '/api/volumes/' + encodeURIComponent(volId) +
                    '/browse?limit=50&prefix=' + encodeURIComponent(prefix);
                return api.get(url).then(function (data) {
                    return ((data && data.entries) || []).filter(function (e) {
                        return e.name.toLowerCase().indexOf(leafLc) === 0;
                    }).map(function (e) {
                        return { value: e.is_dir ? e.relative_path + '/' : e.relative_path, isDir: !!e.is_dir, name: e.name };
                    });
                });
            },
            render: function (item) {
                return '<span class="path-entry-icon">' + (item.isDir ? '\uD83D\uDCC1' : '\uD83D\uDCC4') + '</span>' +
                    escHtml(item.name) + (item.isDir ? '/' : '');
            }
        };
    }

    // ── Job progress stream (SSE) ──────────────────────────────────────
    // handlers: onEvent(evt) per progress event, onDone(evt) for the
    // terminal `done: true` event (stream closed first), onError() on a
    // connection error. closeOnError (default true) tears the stream down
    // on error; pass false to let the browser auto-reconnect.
    // Returns { close() }.
    function attachJobStream(jobId, handlers) {
        handlers = handlers || {};
        var es = new EventSource('/api/jobs/' + encodeURIComponent(jobId) + '/progress');
        function close() { if (es) { es.close(); es = null; } }
        es.onmessage = function (e) {
            var evt;
            try { evt = JSON.parse(e.data); } catch (_) { return; }
            if (evt.done) {
                close();
                if (handlers.onDone) handlers.onDone(evt);
                return;
            }
            if (handlers.onEvent) handlers.onEvent(evt);
        };
        es.onerror = function () {
            if (handlers.closeOnError !== false) close();
            if (handlers.onError) handlers.onError();
        };
        return { close: close };
    }

    // Poll /api/jobs for a still-running job. `match` is a job kind
    // string, a { kind: 1 } map, or a predicate(job). When one is found
    // `onFound(job)` runs and the promise resolves true; otherwise false
    // (also on any fetch failure).
    function reattachIfRunning(match, onFound) {
        var pred = typeof match === 'function' ? match
            : typeof match === 'string' ? function (j) { return j.kind === match; }
            : function (j) { return !!match[j.kind]; };
        return fetch('/api/jobs')
            .then(function (r) { return r.ok ? r.json() : { jobs: [] }; })
            .then(function (s) {
                var jobs = s.jobs || [];
                for (var i = 0; i < jobs.length; i++) {
                    if (jobs[i].completed) continue;
                    if (pred(jobs[i])) {
                        if (onFound) onFound(jobs[i]);
                        return true;
                    }
                }
                return false;
            })
            .catch(function () { return false; });
    }

    // ── Filter-syntax quoting ──────────────────────────────────────────
    // Wrap a filter value in double quotes when it contains a space so the
    // query tokenizer keeps `tag:Fools Theater` as one token. Canonical
    // rule: Rust `SavedSearch::to_url_params` / `requote_token` in
    // src/saved_search.rs (a parity test is a later step).
    function quoteFilterValue(v) {
        v = String(v);
        return v.indexOf(' ') !== -1 ? '"' + v + '"' : v;
    }

    // ── Colour labels ──────────────────────────────────────────────────
    // Vocabulary comes from Rust `models::asset::COLOR_LABELS` via the
    // data island in base.html; derived lookup maps are rebuilt here.
    function setLabels(list) {
        MAKI.labels = list || [];
        MAKI.labelColors = {};
        MAKI.labelNumMap = {};
        MAKI.labelLetterMap = {};
        MAKI.labels.forEach(function (l) {
            MAKI.labelColors[l.name] = l.hex;
            MAKI.labelNumMap[String(l.number)] = l.name;
            MAKI.labelLetterMap[l.key] = l.name;
        });
    }
    function labelHex(name) { return MAKI.labelColors[name]; }

    MAKI.escHtml = escHtml;
    MAKI.escAttr = escAttr;
    MAKI.toast = toast;
    MAKI.api = api;
    MAKI.dialog = dialog;
    MAKI.pathAutocomplete = pathAutocomplete;
    MAKI.volumeBrowseSource = volumeBrowseSource;
    MAKI.volumeBrowseSource = volumeBrowseSource;
    MAKI.volumeBrowseSource = volumeBrowseSource;
    MAKI.attachJobStream = attachJobStream;
    MAKI.reattachIfRunning = reattachIfRunning;
    MAKI.quoteFilterValue = quoteFilterValue;
    MAKI.setLabels = setLabels;
    MAKI.labelHex = labelHex;
    setLabels([]);

    global.MAKI = MAKI;
})(window);
