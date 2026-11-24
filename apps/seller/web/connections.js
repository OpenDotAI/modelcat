(() => {
    const byId = id => document.getElementById(id);
    const add = byId('connection-add');
    const wizard = byId('connection-wizard');
    const wizardTitle = byId('connection-wizard-title');
    const types = byId('connection-types');
    const form = byId('connection-form');
    const list = byId('connection-list');
    const sources = document.querySelector('.connection-sources');
    const trial = byId('inference-form');
    const feedback = byId('connection-feedback');
    const formError = byId('connection-error');
    const modelSelect = byId('local-model');
    const send = byId('inference-submit');
    const workbench = trial.closest('.trial-workbench');
    const promptField = trial.elements.prompt;
    const editor = byId('prompt-editor');
    const exchange = trial.parentElement;
    for (const mascot of workbench.querySelectorAll('[data-trial-cat]')) {
        mascot.append(byId('trial-cat-art').content.cloneNode(true));
    }
    const reader = workbench.querySelector('.trial-reader');
    let followReply = true;
    let draggingReader = false;
    let lastReaderTop = 0;
    function followLatestReply() {
        if (!followReply || !reader.clientHeight) return;
        reader.scrollTop = reader.scrollHeight;
        lastReaderTop = reader.scrollTop;
    }
    function bottomGap() {
        return reader.scrollHeight - reader.clientHeight - reader.scrollTop;
    }
    reader.addEventListener('wheel', event => {
        if (event.ctrlKey) return;
        if (event.deltaY < 0 && reader.scrollTop > 0) followReply = false;
        if (event.deltaMode === 0 && event.deltaY > 0 && event.deltaY >= bottomGap()) followReply = true;
    }, {passive: true});
    reader.addEventListener('keydown', event => {
        if (event.defaultPrevented) return;
        if (['ArrowUp', 'PageUp', 'Home'].includes(event.key) || (event.key === ' ' && event.shiftKey)) {
            if (reader.scrollTop > 0) followReply = false;
        }
        if (event.key === 'End') followReply = true;
    });
    reader.addEventListener('pointerdown', () => {
        draggingReader = true;
        lastReaderTop = reader.scrollTop;
    });
    window.addEventListener('pointerup', () => { draggingReader = false; });
    reader.addEventListener('pointercancel', () => { draggingReader = false; });
    reader.addEventListener('touchmove', () => { followReply = false; }, {passive: true});
    reader.addEventListener('scroll', () => {
        const top = reader.scrollTop;
        if (draggingReader && top < lastReaderTop) followReply = false;
        if (top > lastReaderTop && bottomGap() <= 4) followReply = true;
        lastReaderTop = top;
    }, {passive: true});
    let followFrame = 0;
    function scheduleFollow() {
        if (!followReply || followFrame) return;
        followFrame = requestAnimationFrame(() => {
            followFrame = 0;
            followLatestReply();
        });
    }
    const replyLayout = new ResizeObserver(() => {
        if (workbench.dataset.phase !== 'idle') scheduleFollow();
    });
    for (const element of [reader, ...reader.children]) replyLayout.observe(element);
    const metrics = byId('trial-metrics');
    const statusBar = workbench.querySelector('.trial-status');
    const route = workbench.querySelector('.trial-route');
    new IntersectionObserver(([entry]) => {
        const pinned = entry.rootBounds && entry.boundingClientRect.bottom <= entry.rootBounds.top;
        statusBar.toggleAttribute('data-pinned', Boolean(pinned));
    }, {root: reader, threshold: [0, 0.01]}).observe(route);
    const detailsToggle = byId('trial-details-toggle');
    const currentFacts = byId('trial-current-facts');
    const detailsPopover = byId('trial-details-popover');
    function positionDetails() {
        if (!detailsPopover.matches(':popover-open')) return;
        const button = detailsToggle.getBoundingClientRect();
        const gap = 6;
        const below = window.innerHeight - button.bottom - gap * 2;
        const above = button.top - gap * 2;
        const openAbove = below < detailsPopover.scrollHeight + 2 && above > below;
        detailsPopover.style.maxHeight = Math.max(0, openAbove ? above : below) + 'px';
        const top = openAbove ? button.top - gap - detailsPopover.offsetHeight : button.bottom + gap;
        detailsPopover.style.top = top + 'px';
        detailsPopover.style.left = Math.max(gap, button.right - detailsPopover.offsetWidth) + 'px';
    }
    detailsPopover.addEventListener('toggle', positionDetails);
    reader.addEventListener('scroll', positionDetails, {passive: true});
    window.addEventListener('resize', positionDetails);
    new ResizeObserver(positionDetails).observe(currentFacts);
    function resetTrialDetails() {
        detailsPopover.hidePopover();
        byId('trial-current-details').hidden = true;
    }
    function moveComposer(parent) {
        const {selectionStart, selectionEnd, selectionDirection} = promptField;
        parent.append(trial);
        promptField.focus({preventScroll: true});
        promptField.setSelectionRange(selectionStart, selectionEnd, selectionDirection);
    }
    byId('prompt-expand').onclick = () => {
        editor.showModal();
        moveComposer(editor);
    };
    function collapseComposer() {
        if (!editor.open) return;
        editor.close();
        moveComposer(exchange);
    }
    byId('prompt-collapse').onclick = collapseComposer;
    editor.oncancel = event => { event.preventDefault(); collapseComposer(); };
    promptField.addEventListener('keydown', event => {
        if (event.key !== 'Enter' || event.shiftKey || event.altKey || event.metaKey || event.ctrlKey ||
            event.defaultPrevented || event.isComposing || event.keyCode === 229) return;
        event.preventDefault();
        if (!event.repeat && !send.disabled) trial.requestSubmit(send);
    });
    let connections = [];
    let source = 'all';
    let editing = '';
    let latest = [];
    let active = [];
    let api;
    let refresh;
    let signature = '';
    let wizardTrigger;
    const drafts = new Map();
    let activeKind;
    let testing = false;
    let trialTrace = '';
    let trialRequestId = '';
    let trialModel = '';
    const trialDrawer = byId('model-trial-drawer');
    let trialTrigger;
    let debugTarget;
    const checking = new Set();

    function duration(ms) {
        return ms < 1000 ? Math.round(ms) + ' ms' : (ms / 1000).toFixed(1) + ' 秒';
    }

    function noteCheck(connection, ok, started) {
        localStorage.setItem('check.' + connection.id, JSON.stringify({
            base: connection.base, ok, ms: performance.now() - started, at: Date.now()
        }));
    }

    function readCheck(connection) {
        try {
            const result = JSON.parse(localStorage.getItem('check.' + connection.id));
            return result?.base === connection.base ? result : null;
        } catch { return null; }
    }

    function updateModelResults() {
        const byConnection = new Map();
        for (const record of latest) {
            if (!record.source_id || !record.status) continue;
            if (!byConnection.has(record.source_id)) byConnection.set(record.source_id, new Map());
            byConnection.get(record.source_id).set(record.model, record);
        }
        for (const cell of list.querySelectorAll('[data-model-result]')) {
            const record = byConnection.get(cell.closest('.connection-row').dataset.id)?.get(cell.dataset.modelResult);
            const phase = record?.status;
            const detail = phase === 'ok' ? '成功' : phase === 'cancelled' ? '已取消' : '失败';
            const value = phase ? '最近调用' + detail + (record.duration_ms == null ? '' : ' · ' + duration(record.duration_ms)) : '暂无调用记录';
            if (cell.textContent !== value) cell.textContent = value;
        }
    }
    const connectionTypes = [
        {name:'云端 API', description:'服务商或自建网关', icon:'cloud', protocol:'chat_completions'},
        {name:'本地服务', description:'连接运行中的 Ollama', icon:'chip', protocol:'ollama'},
    ];

    let feedbackTimer;
    function confirmChange(message) {
        clearTimeout(feedbackTimer);
        feedback.textContent = message;
        feedbackTimer = setTimeout(() => {
            if (feedback.textContent === message) feedback.textContent = '';
        }, 3500);
    }

    function node(tag, text) {
        return Object.assign(document.createElement(tag), {textContent: text});
    }

    function button(text, handler, icon) {
        const control = node('button', text);
        control.type = 'button';
        if (icon) window.modelcatVisuals.setIcon(control, icon);
        control.addEventListener('click', handler);
        return control;
    }

    async function run(control, output, work) {
        control.disabled = true;
        output.textContent = '';
        try {
            await work();
        } catch (error) {
            output.textContent = error.message;
        } finally {
            control.disabled = false;
        }
    }

    function showSource(next, focus = true) {
        const option = sources.querySelector('input[value="' + next + '"]');
        option.checked = true;
        if (source !== next) {
            source = next;
            renderConnections();
        }
        if (focus) option.focus();
    }
    sources.addEventListener('change', event => showSource(event.target.value, false));

    let openModelList = '';
    function setModelList(button, open) {
        const panel = document.getElementById(button.getAttribute('aria-controls'));
        const start = panel.getBoundingClientRect().height;
        for (const motion of panel.getAnimations()) motion.cancel();
        button.setAttribute('aria-expanded', String(open));
        panel.inert = !open;
        if (matchMedia('(prefers-reduced-motion: reduce)').matches) {
            panel.hidden = !open;
            return;
        }
        panel.hidden = false;
        const height = panel.firstElementChild.getBoundingClientRect().height;
        const frames = [{height: start + 'px', easing: 'cubic-bezier(.24,.68,.32,1)'}];
        if (open) frames.push({height: (height + 3) + 'px', offset: .72, easing: 'ease-out'});
        frames.push({height: (open ? height : 0) + 'px'});
        const motion = panel.animate(frames, {duration: open ? 310 : 150, fill: 'both'});
        motion.onfinish = () => {
            if (button.getAttribute('aria-expanded') === String(open)) panel.hidden = !open;
            motion.cancel();
        };
    }

    function renderConnections() {
        const visible = source === 'all' ? connections : connections.filter(connection =>
            (connection.protocol === 'ollama' ? 'local' : 'cloud') === source
        );
        if (!visible.length) {
            const message = {
                all: '添加云端 API 或本地服务，即可开始调用模型。',
                cloud: '暂无云端模型连接。',
                local: '暂无本地模型连接。'
            };
            const empty = node('p', message[source]);
            empty.className = 'connection-empty';
            list.replaceChildren(empty);
            return;
        }

        const rows = document.createDocumentFragment();
        for (const connection of visible) {
            const row = document.createElement('article');
            row.className = 'connection-row';
            row.dataset.id = connection.id;
            const header = document.createElement('div');
            header.className = 'connection-head';
            header.dataset.panelRow = '';
            const summary = node('button', '');
            summary.type = 'button';
            summary.className = 'connection-heading';
            const mark = node('span', '');
            mark.className = 'connection-mark';
            window.modelcatVisuals.setIcon(mark, connectionTypes[Number(connection.protocol === 'ollama')].icon);
            const identity = node('span', '');
            identity.className = 'connection-identity';
            const caption = node('span', '');
            caption.className = 'connection-caption';
            const previous = readCheck(connection);
            const health = node('small', checking.has(connection.id) ? '检测中…' : previous
                ? (previous.ok ? '检测成功' : '检测失败') + ' · ' + duration(previous.ms)
                : '已保存');
            health.className = 'connection-health';
            if (previous) {
                health.dataset.state = previous.ok ? 'ok' : 'failed';
                health.title = '上次检测：' + new Date(previous.at).toLocaleString();
            }
            caption.append(node('strong', connection.label), health);
            const address = new URL(connection.base).host;
            identity.append(caption, node('small', (connection.oauth ? 'ChatGPT OAuth' : address) + ' · ' + connection.models.length + ' 个模型'));
            summary.append(mark, identity);
            const content = document.createElement('div');
            content.className = 'connection-expanded';
            const panel = document.createElement('div');
            panel.className = 'connection-reveal';
            panel.id = 'connection-models-' + connection.id;
            panel.hidden = openModelList !== connection.id;
            summary.setAttribute('aria-controls', panel.id);
            summary.setAttribute('aria-expanded', String(!panel.hidden));
            summary.onclick = () => {
                const open = summary.getAttribute('aria-expanded') !== 'true';
                const previous = rowSummary(openModelList);
                if (previous && previous !== summary) setModelList(previous, false);
                openModelList = open ? connection.id : '';
                setModelList(summary, open);
            };
            const toolbar = document.createElement('div');
            toolbar.className = 'connection-toolbar';
            const check = button('重新检测', () => run(check, feedback, async () => {
                checking.add(connection.id);
                health.textContent = '检测中…';
                const started = performance.now();
                try {
                    await api('/api/connections/' + connection.id + '/check', {});
                    noteCheck(connection, true, started);
                } catch (error) {
                    checking.delete(connection.id);
                    noteCheck(connection, false, started);
                    renderConnections();
                    rowSummary(connection.id)?.focus({preventScroll: true});
                    throw error;
                }
                checking.delete(connection.id);
                await refresh();
                renderConnections();
                rowSummary(connection.id)?.focus({preventScroll: true});
            }), 'refresh');
            const remove = button('移除连接', () => {
                const dialog = byId('remove-connection');
                byId('remove-description').textContent = '将移除「' + connection.label + '」及其保存在本机的凭据。';
                dialog.returnValue = '';
                dialog.onclose = () => {
                    if (dialog.returnValue !== 'remove') return;
                    void run(remove, feedback, async () => {
                        await api('/api/connections/' + connection.id + '/remove', {});
                        localStorage.removeItem('check.' + connection.id);
                        await refresh();
                        add.focus({preventScroll: true});
                        confirmChange('已移除 ' + connection.label);
                    });
                };
                dialog.showModal();
            }, 'trash');
            const edit = button('编辑', () => openForm(connection, edit), 'settings');
            edit.setAttribute('aria-label', '编辑连接');
            check.disabled = checking.has(connection.id);
            remove.lastChild.remove();
            remove.title = '移除连接';
            remove.setAttribute('aria-label', '移除连接');
            remove.className = 'remove-action';
            if (connection.oauth) {
                const renew = button('刷新令牌', () => run(renew, feedback, async () => {
                    await api('/api/connections/' + connection.id + '/refresh', {});
                    confirmChange('令牌已刷新');
                }), 'refresh');
                toolbar.append(edit, check, renew, remove);
            } else toolbar.append(edit, check, remove);
            header.append(summary, toolbar);
            const label = node('h3', '可用模型');
            label.className = 'model-list-title';
            content.append(label);
            const models = document.createElement('ul');
            models.setAttribute('aria-label', '可用模型');
            for (const model of connection.models) {
                const entry = document.createElement('li');
                const name = node('span', model.name);
                const info = node('span', '');
                info.className = 'model-entry';
                const result = node('small', '');
                result.dataset.modelResult = model.name;
                info.append(name, result);
                const useModel = button('调试', () => openModelTest(model.id, useModel), 'play');
                useModel.className = 'model-try';
                useModel.dataset.model = model.id;
                useModel.setAttribute('aria-haspopup', 'dialog');
                useModel.setAttribute('aria-label', '调试 ' + model.name);
                entry.append(info, useModel);
                models.append(entry);
            }
            content.append(models);
            panel.append(content);
            row.append(header, panel);
            rows.append(row);
        }
        list.replaceChildren(rows);
        updateModelResults();
    }

    function rowSummary(id) {
        return list.querySelector('[data-id="' + CSS.escape(id) + '"] .connection-heading');
    }

    const pickerTrigger = byId('model-picker-trigger');
    const pickerPanel = byId('model-picker-panel');
    const pickerSearch = byId('model-picker-search');
    const pickerResults = byId('model-picker-results');

    function refreshPicker() {
        if (pickerPanel.hidden) return;
        const query = pickerSearch.value.trim().toLowerCase();
        const groups = new Map();
        for (const option of modelSelect.options) {
            if (!option.value || !option.label.toLowerCase().includes(query)) continue;
            const choice = window.modelcatConnections.choice(option.value);
            const source = choice?.source || '';
            if (!groups.has(source)) groups.set(source, []);
            groups.get(source).push({id: option.value, name: choice?.name || option.label});
        }
        const content = document.createDocumentFragment();
        for (const [source, models] of groups) {
            const group = node('div', '');
            group.setAttribute('role', 'group');
            if (source) {
                group.setAttribute('aria-label', source);
                group.append(node('strong', source));
            }
            for (const model of models) {
                const item = node('button', model.name);
                item.type = 'button';
                item.dataset.value = model.id;
                item.setAttribute('aria-pressed', String(model.id === modelSelect.value));
                group.append(item);
            }
            content.append(group);
        }
        if (!groups.size) content.append(node('p', '没有匹配的模型'));
        pickerResults.replaceChildren(content);
        pickerResults.scrollTop = 0;
    }

    function closePicker() {
        pickerPanel.hidden = true;
        pickerTrigger.setAttribute('aria-expanded', 'false');
    }
    pickerTrigger.onclick = () => {
        if (!pickerPanel.hidden) { closePicker(); return; }
        pickerSearch.value = '';
        pickerPanel.hidden = false;
        pickerTrigger.setAttribute('aria-expanded', 'true');
        refreshPicker();
        pickerSearch.focus({preventScroll: true});
    };
    pickerSearch.oninput = refreshPicker;
    pickerResults.onclick = event => {
        const item = event.target.closest('button');
        if (!item || modelSelect.disabled) return;
        modelSelect.value = item.dataset.value;
        modelSelect.dispatchEvent(new Event('change'));
        closePicker();
        pickerTrigger.focus({preventScroll: true});
    };
    pickerPanel.onkeydown = event => {
        if (event.isComposing) return;
        if (event.key === 'Escape') {
            event.preventDefault();
            closePicker();
            pickerTrigger.focus();
        } else if (event.key === 'Enter' && event.target === pickerSearch) {
            event.preventDefault();
            pickerResults.querySelector('button')?.click();
        } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
            event.preventDefault();
            const items = [pickerSearch, ...pickerResults.querySelectorAll('button')];
            const step = event.key === 'ArrowDown' ? 1 : -1;
            items[(items.indexOf(event.target) + step + items.length) % items.length].focus();
        }
    };
    const pickerRoot = pickerTrigger.parentElement;
    pickerRoot.addEventListener('focusout', event => {
        if (event.relatedTarget && !pickerRoot.contains(event.relatedTarget)) closePicker();
    });
    document.addEventListener('pointerdown', event => {
        if (!pickerPanel.hidden && !pickerRoot.contains(event.target)) closePicker();
    });

    const localTest = byId('local-test');
    localTest.onclick = () => openModelTest(modelSelect.value, localTest);
    function openModelTest(id, trigger) {
        if (!id) return;
        if (testing && trialModel !== id) {
            confirmChange('请等待当前模型返回后再切换模型');
            return;
        }
        debugTarget = {id, ...window.modelcatConnections.choice(id)};
        trialTrigger = trigger;
        updateTrialModel();
        trialPanel.open();
        if (testing) followLatestReply();
        else reader.scrollTop = 0;
        promptField.focus({preventScroll: true});
    }

    function updateTrialModel() {
        const id = debugTarget?.id || modelSelect.value;
        const available = !!id && [...modelSelect.options].some(option => option.value === id);
        if (trialModel && trialModel !== id && !testing) {
            byId('inference-output').textContent = '';
            byId('inference-error').textContent = '';
            trialRequestId = '';
            resetTrialDetails();
            trialModel = '';
            workbench.dataset.phase = 'idle';
            metrics.hidden = true;
        }
        if (workbench.dataset.phase === 'idle') {
            byId('inference-result-status').textContent = available ? '待发送' : debugTarget ? '此模型已不可用' : '暂无可用模型';
        }
        workbench.toggleAttribute('data-ready', available);
        const choice = debugTarget || window.modelcatConnections.choice(modelSelect.value);
        const protocol = choice?.protocol;
        byId('destination-model').textContent = choice?.name || modelSelect.selectedOptions[0]?.text || '暂无可用模型';
        byId('destination-source').textContent = choice?.source || '';
        window.modelcatVisuals.setIcon(byId('trial-target-icon'), protocol === 'ollama' || modelSelect.value.startsWith('ollama/') ? 'chip' : 'models');
        const clientId = modelSelect.selectedOptions[0]?.dataset.clientId || modelSelect.value;
        byId('local-model-id').textContent = clientId || '暂无可用模型';
        byId('local-model-id').title = clientId;
        modelSelect.disabled = !modelSelect.value || testing;
        pickerTrigger.textContent = modelSelect.selectedOptions[0]?.label || '暂无可用模型';
        pickerTrigger.disabled = modelSelect.disabled;
        if (pickerTrigger.disabled) closePicker();
        localTest.disabled = !modelSelect.value;
        byId('local-setup').hidden = !!modelSelect.value;
        send.disabled = !available || !trial.elements.prompt.value.trim() || testing;
        byId('copy-model').disabled = !modelSelect.value;
    }

    function updateTrialDetails() {
        if (trialTrace) {
            const current = active.find(r => r.trace === trialTrace) || latest.find(r => r.trace === trialTrace);
            if (current) {
                trialRequestId = current.id;
                trialTrace = '';
            }
        }
        if (!trialRequestId) return;
        const record = latest.find(r => r.id === trialRequestId) || active.find(r => r.id === trialRequestId);
        renderFacts(currentFacts, record || {id: trialRequestId});
        byId('trial-current-details').hidden = false;
    }

    function renderFacts(facts, selectedRequest) {
        if (!selectedRequest) {
            if (facts.childElementCount) facts.replaceChildren();
            delete facts.dataset.record;
            return;
        }
        const fields = [
            ['请求 ID', selectedRequest.id],
            ['HTTP 状态码', selectedRequest.http_status],
            ['输入 Token', selectedRequest.input_tokens],
        ];
        const signature = JSON.stringify(fields);
        if (facts.dataset.record === signature) return;
        facts.dataset.record = signature;
        facts.replaceChildren();
        for (const [label, value] of fields) {
            if (value == null) continue;
            const line = document.createElement('div');
            const detail = node('dd', value);
            if (label === '请求 ID') {
                detail.className = 'request-id-value';
                detail.title = value;
            }
            line.append(node('dt', label), detail);
            facts.append(line);
        }
    }

    const auth = byId('connection-auth');
    const submitConnection = form.querySelector('button[type="submit"]');
    const reauthorize = byId('connection-reauthorize');
    reauthorize.onclick = () => {
        if (form.reportValidity() && !deviceAttempt) void authorizeDevice();
    };
    let deviceAttempt;
    function closeAuthorizationWindow(attempt) {
        if (embedded && attempt?.id) parent.postMessage({type: 'close-workspace-authorization', attempt: attempt.id}, '*');
    }
    function cancelDevice() {
        const attempt = deviceAttempt;
        closeAuthorizationWindow(attempt);
        deviceAttempt = null;
        byId('codex-callback').value = '';
        byId('codex-login-method').disabled = false;
        if (attempt?.id) void api('/api/connections/codex/' + attempt.id + '/cancel', {}).catch(() => {});
        byId('connection-device-pending').hidden = true;
        byId('connection-device-code').textContent = '';
        form.elements.label.readOnly = false;
    }
    const openBrowser = byId('codex-open-browser');
    async function launchBrowser(attempt) {
        if (!embedded) {
            window.open(attempt.url, '_blank', 'noopener');
            return;
        }
        const channel = new MessageChannel();
        await new Promise((resolve, reject) => {
            const timer = setTimeout(() => {
                channel.port1.close();
                reject(new Error('打开浏览器超时，请重试'));
            }, 30000);
            channel.port1.onmessage = ({data}) => {
                clearTimeout(timer);
                channel.port1.close();
                if (data.error) reject(new Error(data.error));
                else resolve();
            };
            parent.postMessage({type: 'workspace-authorization', attempt: attempt.id}, '*', [channel.port2]);
        });
    }
    openBrowser.onclick = () => {
        if (deviceAttempt) void run(openBrowser, formError, () => launchBrowser(deviceAttempt));
    };
    async function authorizeDevice() {
        const attempt = {};
        deviceAttempt = attempt;
        formError.textContent = '';
        submitConnection.disabled = true;
        reauthorize.disabled = true;
        const browser = form.elements.codexMethod.value === 'browser';
        byId('codex-login-method').disabled = true;
        auth.disabled = true;
        form.elements.label.readOnly = true;
        try {
            const flow = await api('/api/connections/codex/start', {id: editing || null, label: form.elements.label.value, browser});
            if (attempt !== deviceAttempt) {
                await api('/api/connections/codex/' + flow.id + '/cancel', {});
                return;
            }
            attempt.id = flow.id;
            byId('device-code-line').hidden = browser;
            byId('codex-manual').hidden = !browser;
            window.modelcatVisuals.setPanelOpen(byId('codex-manual').querySelector('button'), false);
            attempt.url = browser ? flow.url : 'https://auth.openai.com/codex/device';
            byId('connection-device-code').textContent = flow.code || '';
            byId('connection-device-pending').hidden = false;
            openBrowser.firstChild.textContent = browser ? '返回授权窗口 ' : '打开授权页 ';
            (browser ? byId('codex-wait-status') : openBrowser).focus({preventScroll: true});
            if (browser) {
                try { await launchBrowser(attempt); }
                catch (error) { if (attempt === deviceAttempt) formError.textContent = error.message; }
            }
            while (attempt === deviceAttempt) {
                await new Promise(resolve => setTimeout(resolve, flow.interval * 1000));
                if (attempt !== deviceAttempt) return;
                let connection;
                try {
                    connection = await api('/api/connections/codex/' + flow.id + '/poll', {});
                } catch (error) {
                    if (!(error instanceof TypeError)) throw error;
                    if (attempt === deviceAttempt) formError.textContent = '工作区连接中断，正在重试查询授权结果…';
                    continue;
                }
                if (attempt !== deviceAttempt || connection.pending) continue;
                closeAuthorizationWindow(attempt);
                deviceAttempt = null;
                formError.textContent = '';
                if (source === 'local') showSource('cloud', false);
                connections = [...connections.filter(item => item.id !== connection.id), connection];
                renderConnections();
                form.hidden = true;
                byId('connection-back').hidden = true;
                wizardTitle.textContent = editing ? '已重新授权' : '连接已保存';
                byId('connection-saved-name').textContent = connection.label;
                byId('connection-saved-models').textContent = connection.model_check_failed
                    ? '模型清单获取失败，可在连接列表中重新检测。'
                    : '已检测到 ' + connection.models.length + ' 个模型';
                byId('connection-saved-plan').textContent = connection.plan ? 'ChatGPT · ' + connection.plan : 'ChatGPT OAuth';
                byId('connection-saved-list').replaceChildren(...connection.models.map(m => node('li', m.name)));
                byId('connection-saved').hidden = false;
                byId('connection-cancel').textContent = '完成';
                wizardTrigger = rowSummary(connection.id) || add;
                byId('connection-cancel').focus();
                void refresh().catch(() => {});
                return;
            }
        } catch (error) {
            if (attempt === deviceAttempt) formError.textContent = error.message;
        } finally {
            if (attempt === deviceAttempt) {
                cancelDevice();
                updateProtocol();
            }
        }
    }
    byId('codex-submit-callback').onclick = event => run(event.currentTarget, formError, async () => {
        if (!deviceAttempt?.id) return;
        await api('/api/connections/codex/' + deviceAttempt.id + '/callback', {url:byId('codex-callback').value});
        byId('codex-callback').value = '';
    });
    byId('codex-login-method').onchange = updateProtocol;
    auth.onchange = () => {
        if (auth.value === 'oauth' && !form.elements.label.value) form.elements.label.value = 'ChatGPT / Codex';
        updateProtocol();
    };
    function updateProtocol() {
        const local = form.elements.protocol.value === 'ollama';
        const oauth = !local && auth.value === 'oauth';
        auth.disabled = Boolean(editing || deviceAttempt);
        byId('connection-auth-field').hidden = local;
        byId('connection-base-field').hidden = oauth;
        form.elements.base.disabled = oauth;
        byId('connection-device').hidden = !oauth;
        byId('connection-key-label').hidden = local || oauth;
        form.elements.key.disabled = oauth;
        byId('connection-protocol-label').hidden = local || oauth;
        reauthorize.hidden = !oauth || !editing;
        reauthorize.disabled = Boolean(deviceAttempt);
        submitConnection.textContent = oauth ? (editing ? '保存' : form.elements.codexMethod.value === 'browser' ? '使用 ChatGPT 登录' : '获取设备码') : '测试并保存';
        submitConnection.disabled = Boolean(deviceAttempt);
        const routes = {
            chat_completions: ['OpenAI Chat Completions', 'chat/completions'],
            responses: ['OpenAI Responses', 'responses'],
            messages: ['Anthropic Messages', 'messages'],
        };
        const activeRoutes = local ? [routes.chat_completions, routes.messages] : [routes[oauth ? 'responses' : form.elements.protocol.value]];
        byId('connection-endpoints').replaceChildren(...activeRoutes.map(([name, resource]) => {
            const line = node('div', '');
            line.append(node('dt', name), node('dd', '/v1/' + resource));
            return line;
        }));
        const original = connections.find(connection => connection.id === editing);
        const canReuse = original?.base === form.elements.base.value && original.protocol === form.elements.protocol.value;
        form.elements.key.required = !local && !oauth && !canReuse;
        form.elements.key.placeholder = canReuse ? '留空以保留现有密钥' : '';
        byId('connection-base-help').hidden = local;
        form.elements.base.placeholder = local ? 'http://192.168.1.20:11434' : 'https://api.example.com';
    }

    const baseHelp = byId('connection-base-help');
    baseHelp.onfocusin = baseHelp.onpointerenter = () => delete baseHelp.dataset.dismissed;
    baseHelp.querySelector('button').onclick = event => {
        delete baseHelp.dataset.dismissed;
        event.currentTarget.focus();
    };
    wizard.addEventListener('keydown', event => {
        if (event.key === 'Escape' && !baseHelp.hidden && !baseHelp.hasAttribute('data-dismissed') &&
            baseHelp.matches(':hover, :focus-within')) {
            baseHelp.dataset.dismissed = '';
            event.preventDefault();
        }
    });
    const keyInput = form.elements.key;
    const keyToggle = byId('connection-key-toggle');
    function showKey(visible) {
        keyInput.type = visible ? 'text' : 'password';
        keyToggle.title = visible ? '隐藏 API Key' : '显示 API Key';
        keyToggle.setAttribute('aria-label', keyToggle.title);
        window.modelcatVisuals.setIcon(keyToggle, visible ? 'eyeOff' : 'eye');
    }
    keyToggle.onclick = () => showKey(keyInput.type === 'password');
    form.addEventListener('reset', () => showKey(false));

    function openWizard(trigger) {
        wizardTrigger = trigger;
        byId('connection-saved').hidden = true;
        byId('connection-cancel').textContent = '取消';
        connectionPanel.open();
        add.setAttribute('aria-expanded', 'true');
        feedback.textContent = '';
        formError.textContent = '';
    }

    function chooseType() {
        form.hidden = true;
        types.hidden = false;
        byId('connection-back').hidden = true;
        formError.textContent = '';
        wizardTitle.textContent = '添加连接';
        window.modelcatVisuals.setIcon(wizardTitle, 'link');
        wizardTitle.focus();
    }

    function showForm(kind) {
        activeKind = kind;
        const saved = connections.find(connection => connection.id === editing);
        byId('detected-models').hidden = !saved;
        if (saved) {
            byId('detected-count').textContent = saved.models.length + ' 个';
            byId('detected-list').replaceChildren(...saved.models.map(model => node('li', model.name)));
        }
        wizardTitle.textContent = editing ? '编辑' + form.elements.label.value : kind.name;
        window.modelcatVisuals.setIcon(wizardTitle, kind.icon);
        types.hidden = true;
        form.hidden = false;
        byId('connection-back').hidden = Boolean(editing);
        updateProtocol();
        form.elements.label.focus();
    }

    function openForm(connection, trigger) {
        openWizard(trigger);
        form.reset();
        editing = connection.id;
        auth.value = connection.oauth ? 'oauth' : 'key';
        form.elements.label.value = connection.label;
        form.elements.base.value = connection.base;
        form.elements.protocol.value = connection.protocol;
        showForm(connectionTypes[Number(connection.protocol === 'ollama')]);
    }

    const trialPanel = window.modelcatDrawer(trialDrawer, byId('model-trial-close'), {beforeClose: () => detailsPopover.hidePopover(), afterClose: finishTrial});
    const connectionPanel = window.modelcatDrawer(wizard, byId('connection-cancel'), {
        beforeClose: () => { cancelDevice(); showKey(false); },
        afterClose: finishForm
    });

    function finishTrial(restoreFocus) {
        debugTarget = undefined;
        if (!modelSelect.value && modelSelect.options.length) modelSelect.selectedIndex = 0;
        updateTrialModel();
        if (restoreFocus) {
            const trigger = trialTrigger.isConnected ? trialTrigger :
                [...list.querySelectorAll('.model-try')].find(item => item.dataset.model === trialTrigger.dataset.model);
            (trigger || add).focus({preventScroll: true});
        }
    }
    function finishForm(restoreFocus) {
        const origin = wizardTrigger?.isConnected ? wizardTrigger : (rowSummary(editing) || add);
        form.reset();
        drafts.clear();
        editing = '';
        add.setAttribute('aria-expanded', 'false');
        if (restoreFocus) origin.focus({preventScroll: true});
    }

    function openDraft(kind) {
        form.reset();
        form.elements.protocol.value = kind.protocol;
        for (const [name, value] of drafts.get(kind) || []) form.elements[name].value = value;
        showForm(kind);
    }

    for (const kind of connectionTypes) {
        const card = button('', () => openDraft(kind), kind.icon);
        const label = node('span', '');
        label.append(node('strong', kind.name), node('small', kind.description));
        card.append(label);
        types.append(card);
    }
    form.elements.protocol.onchange = updateProtocol;
    form.elements.base.oninput = updateProtocol;
    add.onclick = () => {
        if (wizard.open && !editing) { connectionPanel.close(); return; }
        editing = '';
        form.reset();
        openWizard(add);
        if (source === 'all') {
            chooseType();
            return;
        }
        const kind = connectionTypes[Number(source === 'local')];
        openDraft(kind);
    };
    byId('connection-back').onclick = () => {
        cancelDevice();
        drafts.set(activeKind, new FormData(form));
        chooseType();
        types.children[connectionTypes.indexOf(activeKind)].focus({preventScroll: true});
    };
    const waitNotice = byId('connection-wait');
    const stopWaiting = byId('connection-stop');
    async function saveWhenReachable(endpoint, payload) {
        const controller = new AbortController();
        const {signal} = controller;
        stopWaiting.onclick = () => controller.abort();
        let probe = false;
        try {
            for (;;) {
                try {
                    if (!probe) return await api(endpoint, payload);
                    await api(endpoint + '?probe_only=true', payload, signal);
                    signal.throwIfAborted();
                    waitNotice.hidden = true;
                    probe = false;
                } catch (error) {
                    if (!inDesktop || error.code !== 'local_service_unreachable') throw error;
                    probe = true;
                    waitNotice.hidden = false;
                    stopWaiting.disabled = false;
                    await new Promise((resolve, reject) => {
                        const cancel = () => {
                            clearTimeout(timer);
                            reject(signal.reason);
                        };
                        const timer = setTimeout(() => {
                            signal.removeEventListener('abort', cancel);
                            resolve();
                        }, 2000);
                        signal.addEventListener('abort', cancel, {once: true});
                    });
                }
            }
        } catch (error) {
            if (!signal.aborted) throw error;
            return null;
        } finally {
            stopWaiting.onclick = null;
            waitNotice.hidden = true;
        }
    }

    form.onsubmit = event => {
        event.preventDefault();
        if (deviceAttempt) return;
        const oauth = auth.value === 'oauth' && form.elements.protocol.value !== 'ollama';
        if (oauth && !editing) {
            void authorizeDevice();
            return;
        }
        const submit = event.submitter || submitConnection;
        void run(submit, formError, async () => {
            const endpoint = editing ? '/api/connections/' + editing : '/api/connections';
            const controls = [...wizard.querySelectorAll('input, select, button:not([type="submit"]), summary'), add];
            const payload = Object.fromEntries(new FormData(form));
            const initialLabel = submit.textContent;
            controls.forEach(control => control.disabled = true);
            submit.textContent = oauth ? '正在保存…' : '正在测试…';
            try {
                const started = performance.now();
                const value = oauth
                    ? await api(endpoint + '/rename', {label: payload.label})
                    : await saveWhenReachable(endpoint, payload);
                if (!value) return;
                if (!oauth) noteCheck(value, true, started);
                const wasEditing = Boolean(editing);
                const savedSource = payload.protocol === 'ollama' ? 'local' : 'cloud';
                if (source !== 'all' && source !== savedSource) showSource(savedSource, false);
                await connectionPanel.close(false);
                await refresh();
                confirmChange((wasEditing ? '已保存 ' : '已添加 ') + value.label + ' · ' + value.models.length + ' 个模型');
                (rowSummary(value.id) || add).focus();
            } finally {
                controls.forEach(control => control.disabled = false);
                submit.textContent = initialLabel;
                updateProtocol();
            }
        });
    };
    async function readTrial(response, output, started) {
        let first, last, tokens, limited = false;
        let terminal = false;
        const limit = response.headers.get('x-modelcat-output-limit');
        function metadata(value) {
            tokens = value.usage?.output_tokens ?? value.usage?.completion_tokens ?? tokens;
            limited ||= value.stop_reason === 'max_tokens' || value.delta?.stop_reason === 'max_tokens'
                || value.choices?.[0]?.finish_reason === 'length' || value.incomplete_details?.reason === 'max_output_tokens';
        }
        function append(text) {
            if (!text) return;
            last = performance.now();
            if (first == null) {
                first = last;
                workbench.setAttribute('data-receiving', '');
                byId('inference-result-status').textContent = '正在接收回复…';
                output.replaceChildren(document.createTextNode(''));
            }
            output.firstChild.appendData(text);
            scheduleFollow();
        }
        function eventData(raw) {
            if (raw === '[DONE]') { terminal = true; return; }
            const event = JSON.parse(raw);
            if (event.error || event.type === 'response.failed') throw Error(event.error?.message || event.response?.error?.message || '模型返回错误');
            metadata(event.response || event);
            const choice = event.choices?.[0];
            append(choice?.delta?.content || event.delta?.text || (event.type === 'response.output_text.delta' ? event.delta : ''));
            terminal ||= Boolean(choice?.finish_reason) || ['message_stop', 'response.completed', 'response.incomplete'].includes(event.type);
        }
        if (response.headers.get('content-type')?.includes('text/event-stream')) {
            const stream = response.body.getReader();
            const decoder = new TextDecoder();
            let pending = '';
            let data = [];
            function line(value) {
                if (value.startsWith('data:')) data.push(value.slice(5).trimStart());
                if (!value && data.length) { eventData(data.join('\n')); data = []; }
            }
            try {
                for (;;) {
                    const {value, done} = await stream.read();
                    pending += decoder.decode(value, {stream: !done});
                    const lines = pending.split(/\r?\n/);
                    pending = lines.pop();
                    for (const entry of lines) line(entry);
                    if (done) break;
                }
                line(pending);
                line('');
                if (!terminal) throw Error('回复流提前结束');
            } finally {
                await stream.cancel().catch(() => {});
                stream.releaseLock();
            }
        } else {
            const value = await response.json();
            metadata(value);
            output.textContent = value.choices?.[0]?.message?.content
                || value.content?.filter(item => item.type === 'text').map(item => item.text).join('\n')
                || value.output_text || value.output?.flatMap(item => item.content || []).filter(item => item.type === 'output_text').map(item => item.text).join('\n')
                || JSON.stringify(value, null, 2);
        }
        return {total: performance.now() - started, first: first == null ? null : first - started,
            perToken: tokens > 1 && last > first ? (last - first) / (tokens - 1) : null, tokens, limit, limited};
    }

    modelSelect.addEventListener('change', updateTrialModel);
    trial.elements.prompt.addEventListener('input', () => {
        send.disabled = !workbench.hasAttribute('data-ready') || !promptField.value.trim() || testing;
    });
    trial.onsubmit = event => {
        event.preventDefault();
        if (testing || !workbench.hasAttribute('data-ready')) return;
        if (!promptField.value.trim()) return;
        collapseComposer();
        const output = byId('inference-output');
        testing = true;
        workbench.dataset.phase = 'sending';
        trialModel = debugTarget?.id || modelSelect.value;
        const prompt = promptField.value;
        promptField.value = '';
        const shortcuts = trial.querySelector('.prompt-shortcuts');
        const readyHint = shortcuts.textContent;
        shortcuts.textContent = '可起草下一条，回复后可发送';
        promptField.focus({preventScroll: true});
        byId('trial-sent').textContent = prompt;
        followReply = true;
        for (const value of metrics.querySelectorAll('[data-metric]')) value.textContent = '';
        metrics.hidden = false;
        byId('trial-limit-note').textContent = '';
        byId('trial-reply-name').lastChild.textContent = byId('destination-model').textContent;
        trialTrace = crypto.randomUUID();
        modelSelect.disabled = true;
        trialRequestId = '';
        resetTrialDetails();
        const submit = event.submitter;
        const oldText = submit.lastChild.textContent;
        submit.lastChild.textContent = '等待回复';
        const resultStatus = byId('inference-result-status');
        const started = performance.now();
        resultStatus.textContent = '等待模型响应…';
        output.textContent = '';
        followLatestReply();
        void run(submit, byId('inference-error'), async () => {
            try {
                const timing = await api('/api/inference/test', {model: trialModel, prompt, trace: trialTrace, stream: true},
                    undefined, response => readTrial(response, output, started));
                workbench.dataset.phase = 'done';
                resultStatus.textContent = '';
                byId('trial-limit-note').textContent = timing.limited ? '已达输出上限' : '';
                metrics.querySelector('[data-metric="total"]').textContent = duration(timing.total);
                metrics.querySelector('[data-metric="first"]').textContent = timing.first == null ? '—' : duration(timing.first);
                metrics.querySelector('[data-metric="perToken"]').textContent = timing.perToken == null ? '—' : timing.perToken.toFixed(1) + ' ms';
                metrics.querySelector('[data-metric="output"]').textContent = (timing.tokens ?? '—') + ' / ' + (timing.limit || '服务默认');
            } catch (error) {
                workbench.dataset.phase = 'failed';
                if (!promptField.value) promptField.value = prompt;
                resultStatus.textContent = '请求失败';
                output.textContent = '';
                throw error;
            } finally {
                workbench.removeAttribute('data-receiving');
                try {
                    await refresh();
                } finally {
                    followLatestReply();
                    testing = false;
                    trialTrace = '';
                    shortcuts.textContent = readyHint;
                    submit.lastChild.textContent = oldText;
                }
            }
        }).then(() => {
            updateTrialModel();
            scheduleFollow();
        });
    };
    byId('trace-model-settings').onclick = () => {
        const sourceId = byId('trace-model-settings').dataset.sourceId;
        byId('trace-open-models').click();
        const connection = rowSummary(sourceId);
        if (connection && connection.getAttribute('aria-expanded') !== 'true') connection.click();
        connection?.focus();
    };

    window.modelcatConnections = {
        configure(call, update) { api = call; refresh = update; },
        showSource,
        closePicker,
        refreshPicker,
        dismissEditor() {
            closePicker();
            collapseComposer();
            if (wizard.open && !add.disabled) connectionPanel.close(false);
            if (trialDrawer.open) trialPanel.close(false);
        },
        sync(items, records) {
            const next = JSON.stringify(items);
            if (next !== signature) {
                connections = items;
                renderConnections();
                signature = next;
            }
            latest = records;
            updateModelResults();
            modelcatHistory.update(connections, latest, active);
            updateTrialDetails();
        },
        activity(items, recent) {
            active = items;
            const known = new Set(latest.map(record => record.id));
            const updates = new Map(recent.map(record => [record.id, record]));
            latest = latest.map(record => updates.get(record.id) || record);
            latest = [...latest, ...recent.slice().reverse().filter(record => !known.has(record.id))].slice(-1000);
            updateModelResults();
            modelcatHistory.update(connections, latest, active);
            updateTrialDetails();
        },
        selectRequest(id) { trialRequestId = id; updateTrialDetails(); },
        updateTrialModel,
        choice(id) {
            for (const connection of connections) {
                const model = connection.models.find(item => item.id === id);
                if (model) return {name: model.name, source: connection.label, label: model.name + ' · ' + connection.label, protocol: connection.protocol};
            }
        },
    };
})();
