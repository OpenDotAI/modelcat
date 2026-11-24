(() => {
    const el = id => document.getElementById(id);
    const dialog = el('request-detail');
    const source = el('history-source');
    const range = el('history-range');
    const status = el('history-status');
    const shortcuts = document.querySelectorAll('[data-history-status]');
    let connections = [], recent = [], running = [], archive = [], counts = [];
    let detail = null, trigger = null;
    let attempted = false, next = null, loaded = false, busy = false, lastFinished = '', dirty = false;
    const firstPages = new Map();
    let pendingRequest = null, displayedFilter = null;
    const date = new Intl.DateTimeFormat('zh-CN', {month:'2-digit', day:'2-digit', hour:'2-digit', minute:'2-digit', second:'2-digit', hour12:false});
    function text(tag, value) {
        const node = document.createElement(tag);
        node.textContent = value;
        return node;
    }
    function when(r) { return r.at ? date.format(r.at * 1000) : '—'; }
    function origin(r) { return r.gateway ? '平台网关' : '本地调用'; }
    function label(r) { return r.source_label || r.source || connections.find(c => c.id === r.source_id)?.label || '连接未记录'; }
    function state(r) {
        if (r.billingOnly) return '仅有账单记录';
        if (!r.status) return r.phase === 'receiving' ? '正在返回' : '进行中';
        return {ok:'已完成', cancelled:'已取消'}[r.status] || '失败';
    }
    function duration(r) { return r.duration_ms == null ? '—' : r.duration_ms < 1000 ? r.duration_ms + ' ms' : (r.duration_ms / 1000).toFixed(1) + ' 秒'; }
    function requestStatus(r) {
        const label = state(r);
        const result = text('span', label);
        result.className = 'request-status';
        if (label === '已完成') result.classList.add('request-ok');
        if (label === '失败') result.classList.add('request-error');
        return result;
    }
    function renderRows(id, rows) {
        const root = el(id);
        const signature = JSON.stringify(rows.map(r => [r.id, when(r), r.model, label(r), origin(r), state(r), duration(r)]));
        if (root.dataset.signature === signature) return;
        const focused = root.contains(document.activeElement) && document.activeElement.dataset.request;
        root.dataset.signature = signature;
        const buttons = rows.map(r => {
            const b = document.createElement('button');
            b.className = 'history-row';
            b.dataset.request = r.id;
            const name = text('strong', r.model || '未知模型');
            const connection = text('small', label(r));
            const identity = document.createElement('span');
            identity.append(name, connection);
            b.append(identity, text('span', origin(r)), text('time', when(r)), requestStatus(r), text('span', duration(r)), text('span', '›'));
            b.onclick = () => open(r, b);
            return b;
        });
        root.replaceChildren(...buttons);
        if (focused) buttons.find(b => b.dataset.request === focused)?.focus({preventScroll:true});
    }
    const path = el('trace-path');
    for (const edge of path.querySelectorAll('.trace-edge')) edge.onclick = () => el('detail-fields').focus();
    el('trace-billing').onclick = () => window.modelcatOpenBilling(detail);

    function showDetail(r) {
        const previous = detail?.id === r.id ? detail : null;
        detail = r;
        el('detail-model').textContent = r.model || '未知模型';
        const hasRoute = !r.billingOnly && Boolean(r.endpoint || r.failure_at || r.phase || r.source_id);
        el('detail-diagnosis').hidden = !hasRoute;
        el('detail-route').hidden = hasRoute;
        el('detail-route').textContent = origin(r);
        el('trace-billing').hidden = !r.gateway || !r.status || r.billingOnly || el('history-page').hidden;
        if (hasRoute) {
            el('trace-ingress-label').textContent = origin(r);
            el('trace-ingress').dataset.openTab = r.gateway ? 'seller' : 'local';
            el('trace-source').textContent = label(r);
            el('trace-model-settings').dataset.sourceId = r.source_id || '';
            path.dataset.failure = r.failure_at || '';
            const returning = previous?.phase === 'sending' && r.status === 'ok';
            if (!previous || r.phase !== previous.phase || returning) {
                path.classList.remove('is-moving');
                if (r.phase || returning) {
                    path.dataset.direction = returning || r.phase === 'receiving' ? 'receive' : 'send';
                    void path.offsetWidth;
                    path.classList.add('is-moving');
                }
            }
        }
        const fields = [
            ['状态', 'activity', requestStatus(r)],
            ['时间', 'clock', r.at ? when(r) : null],
            ['总耗时', 'bolt', r.duration_ms == null ? null : duration(r)],
            ['HTTP', 'gateway', r.http_status],
            ['失败环节', 'network', {model_route:'连接模型服务', model:'模型服务响应'}[r.failure_at]],
            ['错误类型', 'info', state(r) === '失败' ? r.status : null],
            ['输入 Token', 'down', r.input_tokens],
            ['输出 Token', 'up', r.output_tokens],
            ['缓存 Token', 'models', r.cache_tokens],
            ['服务地址', 'link', r.endpoint],
            ['请求 ID', 'tag', r.id, 'request-id']
        ];
        el('detail-fields').replaceChildren(...fields.filter(([, , value]) => value != null).map(([name, icon, value, className]) => {
            const row = document.createElement('div');
            const term = text('dt', name);
            window.modelcatVisuals.setIcon(term, icon);
            const description = document.createElement('dd');
            description.append(value instanceof Node ? value : String(value));
            if (className) {
                description.className = className;
                description.title = String(value);
            }
            row.append(term, description);
            return row;
        }));
    }
    function open(r, from) {
        trigger = from;
        el('detail-feedback').textContent = '';
        showDetail(r);
        detailPanel.open();
    }
    const detailPanel = window.modelcatDrawer(dialog, el('detail-close'), {afterClose(restoreFocus) {
        if (restoreFocus) {
            const replacement = trigger?.dataset.request && Array.from(document.querySelectorAll('.history-row')).find(b =>
                b.dataset.request === trigger.dataset.request && b.getClientRects().length);
            (trigger?.isConnected ? trigger : replacement || el('history-refresh')).focus({preventScroll:true});
        }
        trigger = null;
        detail = null;
    }});
    function renderHistory() {
        const known = new Set(archive.map(r => r.id));
        const live = running.filter(r => !known.has(r.id));
        const perSource = new Map(counts.map(c => [c.source,c.count]));
        for (const r of live) perSource.set(r.source_id || '', (perSource.get(r.source_id || '') || 0) + 1);
        const names = new Map(connections.map(c => [c.id,c.label]));
        for (const key of perSource.keys()) if (!names.has(key)) names.set(key, recent.find(r => r.source_id === key)?.source_label || key || '未记录连接');
        const options = [['*','全部连接'], ...Array.from(names, ([id,name]) => [id,name + '（' + (perSource.get(id) || 0) + '）'])];
        const signature = JSON.stringify(options);
        if (source.dataset.signature !== signature) {
            const previous = source.value;
            source.replaceChildren(...options.map(([id,name]) => new Option(name,id)));
            source.value = names.has(previous) ? previous : '*';
            source.dataset.signature = signature;
        }
        const models = connections.reduce((n,c) => n + c.models.length, 0);
        el('history-connections').textContent = connections.length;
        el('history-models').textContent = models;
        el('history-period').textContent = range.querySelector(':checked').parentElement.textContent + '请求';
        const selectedLive = live.filter(r => source.value === '*' || source.value === (r.source_id || ''));
        const selectedCounts = counts.filter(c => source.value === '*' || source.value === c.source);
        el('history-active').textContent = selectedLive.length;
        el('history-total').textContent = loaded ? selectedCounts.reduce((n,c) => n + c.count, selectedLive.length) : '—';
        el('history-failed').textContent = loaded ? selectedCounts.reduce((n,c) => n + c.failed, 0) : '—';
        const visible = (!status.value || status.value === 'running') ? selectedLive : [];
        const rows = status.value === 'running' ? visible : visible.concat(archive);
        renderRows('history-rows', rows);
        if (loaded) el('history-message').textContent = rows.length ? '' : '当前条件下暂无请求';
        el('history-more').hidden = !next || status.value === 'running';
    }
    function displayPage(data, filter, more) {
        archive = more ? archive.concat(data.items) : data.items;
        counts = data.counts;
        next = data.next;
        loaded = true;
        displayedFilter = filter;
        renderHistory();
    }
    function syncShortcuts() {
        for (const button of shortcuts) button.setAttribute('aria-pressed', String(status.value === button.dataset.historyStatus));
    }
    async function load(more = false, background = false) {
        syncShortcuts();
        pendingRequest?.abort();
        const request = new AbortController();
        pendingRequest = request;
        busy = true;
        attempted = true;
        const filter = {days:range.querySelector(':checked').value, source:source.value, status:status.value};
        const key = JSON.stringify(filter);
        if (!more && !background && firstPages.has(key)) {
            displayPage(firstPages.get(key), filter, false);
        }
        if (!loaded) el('history-message').textContent = '正在读取请求记录…';
        const list = el('history-rows');
        list.setAttribute('aria-busy', 'true');
        const refresh = el('history-refresh');
        refresh.classList.remove('history-loading');
        const indicator = setTimeout(() => refresh.classList.add('history-loading'), 180);
        el('history-more').disabled = true;
        const days = Number(filter.days);
        const params = new URLSearchParams({since:days ? Math.floor(Date.now()/1000) - days*86400 : 0});
        if (filter.source !== '*') params.set('source',filter.source);
        if (filter.status) params.set('status',filter.status);
        if (more) params.set('before',next);
        try {
            const data = await api('/api/request-history?' + params, null, request.signal);
            if (request.signal.aborted) return;
            if (!more) {
                firstPages.delete(key);
                firstPages.set(key, data);
                if (firstPages.size > 12) firstPages.delete(firstPages.keys().next().value);
            }
            dirty = false;
            displayPage(data, filter, more);
        } catch (error) {
            if (request.signal.aborted) return;
            if (displayedFilter) {
                range.querySelector('input[value="' + displayedFilter.days + '"]').checked = true;
                source.value = displayedFilter.source;
                status.value = displayedFilter.status;
                syncShortcuts();
            }
            el('history-message').textContent = '读取失败，请刷新重试。' + error.message;
        } finally {
            clearTimeout(indicator);
            if (pendingRequest === request) {
                pendingRequest = null;
                busy = false;
                refresh.classList.remove('history-loading');
                list.setAttribute('aria-busy', 'false');
                el('history-more').disabled = false;
            }
        }
    }
    for (const input of [source,range,status]) input.onchange = () => load();
    for (const button of shortcuts) {
        button.onclick = () => {
            status.value = status.value === button.dataset.historyStatus ? '' : button.dataset.historyStatus;
            status.dispatchEvent(new Event('change'));
        };
    }
    el('history-more').onclick = () => load(true);
    el('history-refresh').onclick = () => load();
    window.modelcatHistory = {
        load,
        dismissDetail: detailPanel.dismiss,
        openRecord(record) {
            const row = el('history-rows').querySelector('[data-request="' + CSS.escape(record.id) + '"]');
            open(record, row || el('history-refresh'));
        },
        update(items, records, active) {
            const oldRunning = JSON.stringify(running);
            connections = items;
            const ids = new Set(records.map(r => r.id));
            running = active.filter(r => !ids.has(r.id)).sort((a,b) => b.at-a.at || a.id.localeCompare(b.id));
            recent = running.concat(records.slice().reverse());
            if (dialog.open && detail) {
                const fresh = recent.find(r => r.id === detail.id);
                if (fresh && JSON.stringify(detail) !== JSON.stringify(fresh)) showDetail(fresh);
            }
            const latest = records.at(-1)?.id || '';
            if (!el('history-page').hidden) {
                if (lastFinished !== latest) dirty = true;
                if (!busy && (!attempted || (loaded && dirty && archive.length <= 50))) void load(false, loaded);
                else if (!busy && oldRunning !== JSON.stringify(running)) renderHistory();
            }
            lastFinished = latest;
        },
        billingLink(item) {
            const b = text('button',item.id);
            b.onclick = async () => {
                const cached = recent.concat(archive).find(r => r.id === item.id);
                open(cached || {...item, billingOnly:true, gateway:true}, b);
                if (!cached) {
                    try {
                        const record = await api('/api/request-history/' + encodeURIComponent(item.id));
                        if (dialog.open && detail?.id === item.id) showDetail(record);
                    } catch (e) {
                        if (dialog.open && detail?.id === item.id) el('detail-feedback').textContent = e.message;
                    }
                }
            };
            return b;
        }
    };
})();
