(() => {
    const glyphs = {
        overview: '<rect x="3" y="3" width="7" height="10" rx="2"/><rect x="14" y="3" width="7" height="5" rx="2"/><rect x="3" y="17" width="7" height="4" rx="2"/><rect x="14" y="12" width="7" height="9" rx="2"/>',
        models: '<path d="m12 3 9 5-9 5-9-5 9-5Zm-9 9 9 5 9-5m-18 4 9 5 9-5"/>',
        coins: '<ellipse cx="10" cy="7" rx="7" ry="3"/><path d="M3 7v4c0 1.7 3.1 3 7 3 1 0 2-.1 3-.3M3 11v4c0 1.7 3.1 3 7 3m7-11v3"/><ellipse cx="16" cy="13" rx="5" ry="2.5"/><path d="M11 13v4c0 1.4 2.2 2.5 5 2.5s5-1.1 5-2.5v-4"/>',
        terminal: '<rect x="3" y="4" width="18" height="16" rx="3"/><path d="m7 9 3 3-3 3m6 0h4"/>',
        tools: '<path d="M14 7a4.5 4.5 0 0 0-5.8 5.8L3.6 17.4a2 2 0 1 0 3 3l4.6-4.6A4.5 4.5 0 0 0 17 10l-2 2-3-3 2-2Z"/><path d="M17 3v4m-2-2h4m2 10v4m-2-2h4"/>',
        seller: '<path d="M4 11v9h16v-9M3 8l2-4h14l2 4v2a3 3 0 0 1-6 0 3 3 0 0 1-6 0 3 3 0 0 1-6 0V8Z"/><path d="M10 20v-5h4v5"/>',
        billing: '<path d="M4 20h16M7 16v-5m5 5V6m5 10V9"/>',
        settings: '<path d="M4 6h7m4 0h5M4 12h3m4 0h9M4 18h10m4 0h2"/><circle cx="13" cy="6" r="2"/><circle cx="9" cy="12" r="2"/><circle cx="16" cy="18" r="2"/>',
        network: '<circle cx="12" cy="6" r="2.5"/><circle cx="5" cy="18" r="2.5"/><circle cx="19" cy="18" r="2.5"/><path d="m10.7 8.2-4.4 7.6m7-7.6 4.4 7.6M7.5 18h9"/>',
        gateway: '<circle cx="12" cy="12" r="9"/><path d="M3 12h18m-9-9c5 4 5 14 0 18-5-4-5-14 0-18Z"/>',
        mac: '<rect x="4" y="4" width="16" height="12" rx="2"/><path d="M2 17v1a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-1H2Z"/>',
        chip: '<rect x="6" y="6" width="12" height="12" rx="2"/><rect x="9" y="9" width="6" height="6" rx="1"/><path d="M9 3v3m6-3v3m-6 12v3m6-3v3M3 9h3m-3 6h3m12-6h3m-3 6h3"/>',
        docker: '<rect x="3" y="4" width="18" height="6" rx="2"/><rect x="3" y="14" width="18" height="6" rx="2"/><path d="M7 7h.01M7 17h.01m4-10h6m-6 10h6"/>',
        play: '<path d="M8 5v14l11-7L8 5Z"/>',
        power: '<path d="M12 3v8m4.9-5.3a8 8 0 1 1-9.8 0"/>',
        plus: '<path d="M12 5v14M5 12h14"/>',
        back: '<path d="m13 6-6 6 6 6m-6-6h13"/>',
        cloud: '<path d="M7 18a4 4 0 1 1 .5-8 5 5 0 0 1 9.7 1.5A3.3 3.3 0 1 1 18 18Z"/>',
        expand: '<path d="M14 4h6v6m0-6-7 7M4 14v6h6m-6 0 7-7"/>',
        send: '<path d="m3 10 17-6-6 17-3-8-8-3Zm8 3 9-9"/>',
        stop: '<rect x="6" y="6" width="12" height="12" rx="2"/>',
        pause: '<path d="M9 5v14m6-14v14"/>',
        trash: '<path d="M4 6h16M9 6V3h6v3M6 6l1 15h10l1-15M10 10v7m4-7v7"/>',
        refresh: '<path d="M20 11a8 8 0 0 0-14-5L3 9m0-5v5h5m-4 4a8 8 0 0 0 14 5l3-3m0 5v-5h-5"/>',
        restart: '<path d="M5 9a8 8 0 1 1 1 8M5 4v5h5"/><path d="m11 9 5 3-5 3V9Z"/>',
        search: '<circle cx="10.5" cy="10.5" r="6.5"/><path d="m16 16 5 5"/>',
        batch: '<rect x="3" y="4" width="13" height="5" rx="1.5"/><path d="M3 13h8m-8 5h5m5 2 3-.7 5-5-2.3-2.3-5 5L13 20Z"/>',
        draft: '<path d="M6 3h9l4 4v14H5V4a1 1 0 0 1 1-1Z"/><path d="M14 3v5h5M8 12h8m-8 4h5"/>',
        tag: '<path d="M3 4h8l10 10-7 7L3 10V4Z"/><circle cx="7.5" cy="7.5" r="1"/>',
        info: '<circle cx="12" cy="12" r="9"/><path d="M12 11v6m0-10v.1"/>',
        eye: '<path d="M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12Z"/><circle cx="12" cy="12" r="3"/>',
        eyeOff: '<path d="m3 3 18 18M9 5.5a10 10 0 0 1 3-.5c6.4 0 10 7 10 7a21 21 0 0 1-3.2 4M6.1 6.5A22 22 0 0 0 2 12s3.6 7 10 7c1.6 0 3.2-.5 4.6-1.2M10 10a3 3 0 0 0 4 4"/>',
        key: '<circle cx="8" cy="15" r="4"/><path d="m11 12 8-8m-3 3 3 3m-5-1 2 2"/>',
        account: '<circle cx="12" cy="8" r="3.5"/><path d="M5 20v-2a4 4 0 0 1 4-4h6a4 4 0 0 1 4 4v2"/>',
        folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7Z"/>',
        link: '<path d="m10 14 4-4m-6 6-1 1a3 3 0 0 1-4-4l4-4a3 3 0 0 1 4 0m5-1 1-1a3 3 0 0 1 4 4l-4 4a3 3 0 0 1-4 0"/>',
        check: '<path d="m5 12 4 4L19 6"/>',
        activity: '<path d="M2 12h5l3-7 4 14 3-7h5"/>',
        sliders: '<path d="M5 4v16m7-16v16m7-16v16"/><path d="M2 9h6m1 6h6m1-7h6"/>',
        chevron: '<path d="m9 6 6 6-6 6"/>',
        bolt: '<path d="M14 2 4 14h7l-1 8 10-13h-7l1-7Z"/>',
        clock: '<circle cx="12" cy="12" r="8"/><path d="M12 7v5l3 2"/>',
        idle: '<circle cx="12" cy="12" r="3"/>',
        arrow: '<path d="M4 12h15m-6-6 6 6-6 6"/>',
        down: '<path d="M12 4v15m-6-6 6 6 6-6"/>',
        up: '<path d="M12 20V5m6 6-6-6-6 6"/>',
        shield: '<path d="M12 3 20 6v5c0 5-4 8-8 10-4-2-8-5-8-10V6l8-3Z"/><path d="m8.5 12 2.5 2.5 4.5-5"/>',
        log: '<rect x="5" y="3" width="14" height="18" rx="2"/><path d="M9 8h6m-6 4h6m-6 4h4"/>',
        copy: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/>'
    };

    function icon(name) {
        return '<svg class="glyph" viewBox="0 0 24 24" aria-hidden="true" focusable="false">' + glyphs[name] + '</svg>';
    }

    function setIcon(element, name) {
        if (element.dataset.glyph === name && element.firstElementChild?.classList.contains('glyph')) return;
        element.querySelector(':scope > .glyph')?.remove();
        element.insertAdjacentHTML('afterbegin', icon(name));
        element.dataset.glyph = name;
    }

    for (const element of document.querySelectorAll('[data-icon]')) setIcon(element, element.dataset.icon);

    const topology = document.querySelector('[data-topology]');
    const workspaceDiagram = topology?.dataset.topology === 'workspace';
    const diagramWidth = 450;
    const diagramHeight = workspaceDiagram ? 226 : 248;
    const workspaceNodes = {
        agent: [54, 33], cloud: [54, 105], localModel: [54, 177],
        gateway: [302, 105], subscriber: [412, 105]
    };
    function topologyNode(name, symbol, label, action) {
        const tag = action ? 'button' : 'div';
        const attributes = action ? ` type="button" title="${action}" aria-label="${label}，${action}"` : '';
        const position = workspaceDiagram
            ? ` style="--node-x:${workspaceNodes[name][0] / diagramWidth * 100}%;--node-y:${workspaceNodes[name][1] / diagramHeight * 100}%"`
            : '';
        return `<${tag} class="topology-node" data-node="${name}"${position}${attributes}>
            <span class="topology-tile">${icon(symbol)}</span><span>${label}</span></${tag}>`;
    }

    const routes = workspaceDiagram ? {
        agent: 'M54 33H94Q108 33 108 47V80Q108 94 122 94H177',
        cloud: 'M54 105H177',
        localModel: 'M54 177H94Q108 177 108 163V130Q108 116 122 116H177',
        gateway: 'M177 105H302',
        subscriber: 'M302 105H412'
    } : {
        cloud: 'M70 66H104C140 66 117 104 155 104H225',
        localModel: 'M70 182H104C140 182 117 144 155 144H225',
        agent: 'M225 104H295C333 104 310 66 346 66H380',
        earnings: 'M225 144H295C333 144 310 182 346 182H380'
    };

    if (topology) {
        topology.innerHTML = `
            <svg class="topology-wires" viewBox="0 0 ${diagramWidth} ${diagramHeight}" preserveAspectRatio="none" aria-hidden="true">
                ${workspaceDiagram
                    ? ''
                    : `<ellipse class="orbit" cx="225" cy="124" rx="81" ry="74"/>
                       <ellipse class="orbit orbit-outer" cx="225" cy="124" rx="112" ry="101"/>
                       <path class="topology-guides" d="M103 124H347M225 14V234"/>`}
                ${Object.entries(routes).map(([name, coordinates]) => `
                    <g data-wire="${name}">
                        <path class="route-track" d="${coordinates}" pathLength="100"/>
                        <path class="route-trail" d="${coordinates}" pathLength="100"/>
                        <path class="route-light" d="${coordinates}" pathLength="100"/>
                    </g>`).join('')}
                ${workspaceDiagram ? '<path class="supply-arrows" d="m247 102 3 3-3 3m109-6 3 3-3 3"/>' : ''}
            </svg>
            <div class="topology-core"${workspaceDiagram ? ' style="--core-y:46.46%"' : ''}>
                ${workspaceDiagram ? `<a class="stop-workspace" href="tauri://localhost/vm.html?action=stop" aria-label="停止工作区" title="停止工作区">
                    <img src="${topology.dataset.logo}" alt="">${icon('stop')}
                    <span class="topology-core-caption" aria-hidden="true"><span>ModelCat</span><span>停止工作区</span></span>
                </a>` : `<img src="${topology.dataset.logo}" alt="">`}
            </div>
            ${workspaceDiagram
                ? topologyNode('agent', 'terminal', '工作区 Agent', '打开工具管理') +
                  topologyNode('cloud', 'cloud', '云端 LLM', '管理云端模型连接') +
                  topologyNode('localModel', 'chip', '本地 LLM', '管理本地模型连接') + topologyNode('gateway', 'gateway', '平台网关') +
                  topologyNode('subscriber', 'account', '订阅者')
                : ['cloud','chip','terminal','coins'].map(symbol =>
                    `<span class="intro-node topology-tile" data-symbol="${symbol}" aria-hidden="true">${icon(symbol)}</span>`).join('')}`;
        const observer = new IntersectionObserver(([entry]) => {
            topology.dataset.visible = String(entry.isIntersecting);
        });
        observer.observe(topology);
    }

    function topologyState({
        workspace = 'idle', models = 'idle', gateway = 'idle', active = 0,
        gatewayUrl, agentRunning = false, subscriberActive = false, hasCloud = false, hasLocal = false
    }) {
        if (!workspaceDiagram) return;
        topology.dataset.workspace = workspace;
        topology.dataset.active = String(workspace === 'online' && active > 0);
        const states = workspaceDiagram ? {
            agent: agentRunning ? 'online' : 'idle', gateway, subscriber: subscriberActive ? 'online' : 'idle',
            cloud: hasCloud ? 'online' : 'idle', localModel: hasLocal ? 'online' : 'idle'
        } : {client: workspace, vm: workspace, models, gateway};
        if (workspaceDiagram) {
            const gatewayTile = topology.querySelector('[data-node="gateway"]');
            let host = '';
            try { if (gatewayUrl) host = new URL(gatewayUrl).host; } catch {}
            const label = host || '平台网关';
            if (gatewayTile.lastElementChild.textContent !== label) gatewayTile.lastElementChild.textContent = label;
            gatewayTile.title = label;
            topology.setAttribute('aria-label', `工作区 Agent 调用 ModelCat API。ModelCat 接入云端 LLM（${hasCloud ? '已配置' : '未配置'}）和本地 LLM（${hasLocal ? '已配置' : '未配置'}），可通过${label}向订阅者提供已发布的模型。订阅者从网关发起请求。`);
        }
        for (const [node, state] of Object.entries(states)) {
            const tile = topology.querySelector('[data-node="' + node + '"]');
            const wire = topology.querySelector('[data-wire="' + node + '"]');
            if (tile.dataset.state !== state) tile.dataset.state = state;
            if (wire.dataset.state !== state) wire.dataset.state = state;
        }
    }

    function flowState(id, state, label) {
        const target = document.getElementById(id);
        target.textContent = label;
        target.closest('li').dataset.state = state;
    }

    const number = new Intl.NumberFormat('zh-CN');
    const size = new Intl.NumberFormat('zh-CN', { maximumFractionDigits: 1 });
    function bytes(value) {
        if (value >= 1e18) return size.format(value / 1e18) + ' EB';
        if (value >= 1e15) return size.format(value / 1e15) + ' PB';
        if (value >= 1e12) return size.format(value / 1e12) + ' TB';
        if (value >= 1e9) return size.format(value / 1e9) + ' GB';
        if (value >= 1e6) return size.format(value / 1e6) + ' MB';
        if (value >= 1e3) return size.format(value / 1e3) + ' KB';
        return number.format(value) + ' B';
    }

    function bars(container, entries, formatter, max = Math.max(1, ...entries.map(entry => entry.value))) {
        if (!container.hasChildNodes()) {
            for (const { label, symbol, tone } of entries) {
                const row = document.createElement('div');
                row.className = 'chart-row';
                row.dataset.tone = tone;
                row.innerHTML = '<dt>' + icon(symbol) + label + '</dt><dd><span class="chart-track" aria-hidden="true"><span></span></span><span class="chart-value"></span></dd>';
                container.append(row);
            }
        }
        entries.forEach(({ value }, index) => {
            const row = container.children[index];
            row.querySelector('.chart-track > span').style.width = (value / max * 100) + '%';
            row.querySelector('.chart-value').textContent = formatter(value);
        });
    }

    const dateLabel = new Intl.DateTimeFormat('zh-CN', { month: 'numeric', day: 'numeric' });
    function renderHistory(container, records) {
        const today = new Date();
        today.setHours(0, 0, 0, 0);
        const buckets = Array.from({length: 15}, (_, index) => {
            const date = new Date(today);
            date.setDate(date.getDate() + index - 14);
            return { date, count: 0 };
        });
        for (const record of records) {
            const timestamp = record.at * 1000;
            const index = buckets.findIndex(({date}, i) => timestamp >= +date && timestamp < +(buckets[i + 1]?.date ?? Infinity));
            if (index >= 0 && timestamp <= Date.now()) buckets[index].count++;
        }
        const signature = buckets.map(({date, count}) => `${+date}:${count}`).join('|');
        if (container.dataset.rendered === signature) return;
        container.dataset.rendered = signature;
        const max = Math.max(1, ...buckets.map(bucket => bucket.count));
        const plot = container.querySelector('ol');
        buckets.forEach(({date, count}, index) => {
            const mark = plot.children[index] || plot.appendChild(document.createElement('li'));
            if (!mark.firstChild) {
                const hit = document.createElement('button');
                hit.type = 'button';
                hit.onkeydown = event => {
                    const offset = {ArrowLeft:-1, ArrowRight:1}[event.key];
                    if (!offset) return;
                    event.preventDefault();
                    plot.children[Math.max(0, Math.min(14,index+offset))].firstChild.focus();
                };
                mark.append(hit);
            }
            const label = dateLabel.format(date) + ' · ' + number.format(count) + ' 次请求';
            mark.firstChild.setAttribute('aria-label', label);
            mark.dataset.empty = String(count === 0);
            mark.style.setProperty('--value', (count / max * 100) + '%');
        });
        container.querySelector('[data-first-day]').textContent = dateLabel.format(buckets[0].date);
        container.querySelector('[data-peak]').textContent = number.format(max);
    }

    const trafficRanges = {'5m': [300, '5 分钟'], '1h': [3600, '1 小时'], '24h': [86400, '24 小时']};
    const trafficGraph = document.querySelector('.traffic-graph');
    const trafficInspection = trafficGraph?.querySelector('.traffic-inspection');
    const trafficTime = new Intl.DateTimeFormat('zh-CN', {
        month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit', second: '2-digit',
        hourCycle: 'h23'
    });
    let trafficView;
    let trafficPointer = null;
    let trafficKeyTime = null;

    function trafficY(rate, scale) {
        return 125 - 122 * rate / scale;
    }

    function inspectTraffic(announce = false) {
        if (!trafficView) return;
        const { samples, cutoff, seconds, scale } = trafficView;
        const time = trafficPointer === null ? trafficKeyTime : cutoff + trafficPointer * seconds;
        const sample = time === null ? undefined : samples.findLast(point =>
            point.start <= time && point.end >= time);
        const announcement = trafficGraph.querySelector('.traffic-announcement');
        trafficInspection.hidden = time === null || !trafficView.samples.length;
        if (!sample) {
            announcement.textContent = '';
            trafficInspection.querySelector('.traffic-tooltip-time').textContent = '此时段无采样';
            trafficInspection.dataset.missing = 'true';
            if (time !== null) {
                trafficInspection.style.setProperty('--cursor-x', ((time-cutoff) / seconds * 100) + '%');
                trafficInspection.classList.toggle('tooltip-left', time >= cutoff + seconds / 2);
            }
            return;
        }

        trafficInspection.dataset.missing = 'false';
        const pointTime = trafficView.tokens ? (sample.start + sample.end) / 2 : sample.end;
        const position = (pointTime - cutoff) / seconds;
        trafficInspection.style.setProperty('--cursor-x', (position * 100) + '%');
        trafficInspection.classList.toggle('tooltip-left', position >= 0.5);
        const interval = trafficTime.formatRange(sample.start * 1000, sample.end * 1000);
        trafficInspection.querySelector('.traffic-tooltip-time').textContent = interval;
        for (const direction of ['rx', 'tx']) {
            const marker = trafficInspection.querySelector('[data-marker="' + direction + '"]');
            marker.style.top = (trafficY(sample[direction], scale) / 128 * 100) + '%';
            trafficInspection.querySelector('[data-detail="' + direction + '"]').textContent =
                trafficView.format(sample[direction]);
        }
        if (announce) announcement.textContent = interval + '，' + (trafficView.tokens ? '输入 ' : '接收 ') +
            trafficView.format(sample.rx) + (trafficView.tokens ? '，输出 ' : '，发送 ') + trafficView.format(sample.tx);
    }

    if (trafficGraph) {
        trafficGraph.addEventListener('pointermove', event => {
            if (event.pointerType === 'touch') return;
            const { left, width } = trafficGraph.getBoundingClientRect();
            trafficPointer = Math.max(0, Math.min(1, (event.clientX - left) / width));
            trafficKeyTime = null;
            inspectTraffic();
        });
        trafficGraph.addEventListener('pointerleave', () => {
            trafficPointer = null;
            inspectTraffic();
        });
        trafficGraph.addEventListener('blur', () => {
            trafficKeyTime = null;
            inspectTraffic();
        });
        trafficGraph.addEventListener('keydown', event => {
            if (event.key === 'Escape') {
                trafficPointer = trafficKeyTime = null;
                inspectTraffic();
                return;
            }
            if (!['ArrowLeft', 'ArrowRight'].includes(event.key) || !trafficView?.samples.length) return;
            event.preventDefault();
            const { samples } = trafficView;
            const current = samples.findIndex(point => point.start <= trafficKeyTime && point.end >= trafficKeyTime);
            const step = event.key === 'ArrowLeft' ? -1 : 1;
            const index = current < 0 ? samples.length - 1 : Math.max(0, Math.min(samples.length - 1, current + step));
            const { start, end } = samples[index];
            trafficPointer = null;
            trafficKeyTime = (start + end) / 2;
            inspectTraffic(true);
        });
    }

    function smoothSeries(samples, direction, cutoff, seconds, scale, tokens) {
        if (!samples.length) return '';
        const points = [];
        const add = (time, value) => points.push([
            (time - cutoff) * 640 / seconds, trafficY(value, scale)
        ]);
        add(samples[0].start, samples[0][direction]);
        samples.forEach((sample, index) => {
            const previous = samples[index - 1];
            if (previous && previous.end < sample.start) {
                const ramp = Math.min((sample.start - previous.end) / 2, seconds / 160);
                add(previous.end, previous[direction]);
                add(previous.end + ramp, 0);
                add(sample.start - ramp, 0);
                add(sample.start, sample[direction]);
            }
            add(tokens ? (sample.start + sample.end) / 2 : sample.end, sample[direction]);
        });
        const last = samples[samples.length - 1];
        if (tokens) add(last.end, last[direction]);
        let path = 'M' + points[0].join(' ');
        for (let i = 1; i < points.length; i++) {
            const [x, y] = points[i];
            const [px, py] = points[i - 1];
            const mid = (x + px) / 2;
            path += ` C${mid} ${py} ${mid} ${y} ${x} ${y}`;
        }
        return path;
    }

    function tokenBuckets(data) {
        const rows = new Map(data.rows.map(row => [row[0], row]));
        const buckets = [];
        for (let start = data.start; start <= data.now; start += data.step) {
            const [,requests,rx,tx,cache] = rows.get(start) || [start,0,0,0,0];
            buckets.push({start, end: Math.min(start + data.step, data.now), requests, rx, tx, cache});
        }
        return buckets;
    }

    function gatewayTraffic(data, range, errorText, tokens = false) {
        const chart = document.getElementById('traffic');
        const [windowSeconds, label] = trafficRanges[range];
        const samples = tokens && data ? tokenBuckets(data) : data?.samples || [];
        const cutoff = tokens && data ? data.start : (data?.now || 0) - windowSeconds;
        const seconds = Math.max(1, (data?.now ?? cutoff + windowSeconds) - cutoff);
        const peak = samples.reduce((max, sample) => Math.max(max, sample.rx, sample.tx), 0);
        const scale = data?.axisLimit ?? Math.max(1, Math.ceil(peak));
        const format = tokens ? value => number.format(value) : value => bytes(Math.round(value)) + '/s';
        const totals = {};
        chart.querySelector('#token-totals').hidden = !tokens;
        chart.querySelector('table').hidden = tokens;
        for (const cell of chart.querySelectorAll('[data-total]')) {
            const key = cell.dataset.total;
            totals[key] = tokens ? samples.reduce((sum, sample) => sum + sample[key], 0) : 0;
            cell.textContent = data ? number.format(totals[key]) : '—';
        }
        for (const direction of ['rx', 'tx']) {
            const incoming = direction === 'rx';
            const legend = tokens ? (incoming ? '输入' : '输出') : (incoming ? '↓ 网关接收' : '↑ 网关发送');
            chart.querySelector('[data-legend="' + direction + '"]').textContent = legend;
            chart.querySelector('[data-detail-label="' + direction + '"]').textContent = legend;
            const value = tokens && data ? totals[direction] : data?.latest?.[direction];
            chart.querySelector('[data-rate="' + direction + '"]').textContent = value == null ? '—' : format(value);
            chart.querySelector('[data-series="' + direction + '"]').setAttribute('d',
                smoothSeries(samples, direction, cutoff, seconds, scale, tokens));
        }
        const gapLabel = chart.querySelector('.traffic-gap');
        let gapStart = 0, gapWidth = 0;
        if (!tokens) {
            for (let index = 1; index < samples.length; index++) {
                const start = samples[index - 1].end;
                const width = samples[index].start - start;
                if (width > gapWidth) { gapStart = start; gapWidth = width; }
            }
        }
        gapLabel.hidden = gapWidth / seconds < 0.16;
        if (!gapLabel.hidden) gapLabel.style.left = ((gapStart + gapWidth / 2 - cutoff) / seconds * 100) + '%';
        const empty = !data ? errorText : tokens && !totals.requests ? '此时段暂无请求' : samples.length ? '' : '等待网关采样';
        chart.querySelector('.traffic-empty').textContent = empty;
        chart.querySelector('.traffic-scale').textContent = (tokens ? peak : data) ? format(scale) + (tokens ? ' Token' : '') : '';
        document.getElementById('traffic-start').textContent = tokens && data
            ? new Date(cutoff * 1000).toLocaleTimeString('zh-CN', {hour:'2-digit', minute:'2-digit'}) : label + '前';
        document.getElementById('traffic-mid').textContent = data ? new Date((cutoff + seconds / 2) * 1000)
            .toLocaleTimeString('zh-CN', {hour:'2-digit', minute:'2-digit'}) : '';
        const title = tokens ? 'Token 用量' : '网关收发速率';
        trafficGraph.setAttribute('aria-label', title + '趋势，可用左右方向键查看详情');
        chart.querySelector('svg').setAttribute('aria-label', '近 ' + label + title + '，' + (empty || '最高 ' + format(peak)));
        if (range !== trafficView?.range || tokens !== trafficView?.tokens) trafficKeyTime = null;
        trafficView = { samples, cutoff, scale, range, seconds, tokens, format };
        inspectTraffic();
    }

    let usageRecords = [];
    const usageModel = document.getElementById('usage-model');
    const usagePeriod = document.getElementById('usage-period');
    let usageView = 'model';
    const usageViews = document.querySelectorAll('[data-usage-view]');
    function modelUsageChart(rows, peak) {
        const table = document.createElement('table');
        table.className = 'model-usage-chart';
        table.setAttribute('aria-label', '各模型 Token 用量对比');
        const header = table.createTHead().insertRow();
        const metrics = [['input', '输入'], ['output', '输出'], ['cache', '缓存输入']];
        for (const label of ['模型', '请求', ...metrics.map(([, label]) => label)]) {
            const th = document.createElement('th');
            th.scope = 'col';
            th.textContent = label;
            header.append(th);
        }
        const body = table.createTBody();
        for (const [name, usage] of rows) {
            const row = body.insertRow();
            const model = document.createElement('th');
            model.scope = 'row';
            model.textContent = name;
            row.append(model);
            row.insertCell().textContent = number.format(usage.requests);
            for (const [metric] of metrics) {
                const cell = row.insertCell();
                cell.textContent = number.format(usage[metric]);
                const track = document.createElement('span');
                track.className = 'usage-meter';
                track.dataset.metric = metric;
                track.setAttribute('aria-hidden', 'true');
                const fill = document.createElement('span');
                fill.style.width = (100 * usage[metric] / peak) + '%';
                track.append(fill);
                cell.append(track);
            }
        }
        return table;
    }
    function renderUsage() {
        const days = Number(usagePeriod.querySelector('input:checked').value);
        const cutoff = days ? Date.now()/1000 - days*86400 : 0;
        usageModel.hidden = usageView === 'model';
        const selectedModel = usageModel.hidden ? '' : usageModel.value;
        const groups = new Map();
        let requests = 0;
        for (const record of usageRecords) {
            const model = record.model || '未知模型';
            if (record.at < cutoff || (selectedModel && model !== selectedModel)) continue;
            const key = usageView === 'total' ? '' : usageView === 'model' ? model : new Date(record.at*1000).toLocaleDateString('sv-SE');
            const group = groups.get(key) || {requests:0,input:0,output:0,cache:0};
            requests++;
            group.requests++;
            group.input += record.input_tokens || 0;
            group.output += record.output_tokens || 0;
            group.cache += record.cache_tokens || 0;
            groups.set(key, group);
        }
        const chart = document.querySelector('[data-token-chart]');
        chart.querySelector('[data-requests]').textContent = number.format(requests);
        const host = document.getElementById('usage-breakdown');
        const rows = [...groups.entries()].sort(([a,x],[b,y]) => usageView === 'day' ? b.localeCompare(a) :
            (y.input+y.output)-(x.input+x.output) || a.localeCompare(b));
        const signature = JSON.stringify([usageView,rows]);
        if (host.dataset.chart === signature) return;
        host.dataset.chart = signature;
        host.replaceChildren();
        if (!rows.length) { host.textContent = '所选范围暂无请求'; return; }
        const max = Math.max(1, ...rows.map(([,g])=>Math.max(g.input,g.output,g.cache)));
        if (usageView === 'model') {
            host.append(modelUsageChart(rows, max));
            return;
        }
        for (const [name,g] of rows) {
            const group = document.createElement('div');
            group.className = 'usage-series';
            if (name) {
                const heading = document.createElement('h3');
                const count = document.createElement('small');
                heading.textContent = name;
                count.textContent = number.format(g.requests)+' 次请求';
                heading.append(count);
                group.append(heading);
            }
            const plot = document.createElement('dl');
            plot.className = 'chart-bars';
            bars(plot, [
                {label:'输入',value:g.input,symbol:'down',tone:'cool'},
                {label:'输出',value:g.output,symbol:'up',tone:'warm'},
                {label:'缓存输入',value:g.cache,symbol:'models',tone:'neutral'}
            ], value=>number.format(value), max);
            group.append(plot);
            host.append(group);
        }
    }
    for (const control of [usagePeriod,usageModel]) control?.addEventListener('change', renderUsage);
    for (const button of usageViews) button.onclick = () => {
        usageView = button.dataset.usageView;
        for (const item of usageViews) item.setAttribute('aria-pressed',String(item === button));
        renderUsage();
    };
    function renderCharts(snapshot) {
        usageRecords = snapshot.records;
        const names = [...new Set(usageRecords.map(r=>r.model || '未知模型'))].sort();
        const selected = usageModel.value;
        if (JSON.stringify(names)!==usageModel.dataset.names) {
            usageModel.dataset.names=JSON.stringify(names);
            usageModel.replaceChildren(new Option('全部模型',''), ...names.map(name=>new Option(name,name)));
            if (names.includes(selected)) usageModel.value=selected;
        }
        renderUsage();
        const totals = {upstreamIn:0, upstreamOut:0};
        for (const record of usageRecords) {
            totals.upstreamIn += record.upstream_bytes_in;
            totals.upstreamOut += record.upstream_bytes_out;
        }
        renderHistory(document.querySelector('[data-request-history]'), snapshot.records);
        document.getElementById('total-gw-in').textContent = bytes(snapshot.runtime.gw_bytes_in);
        document.getElementById('total-gw-out').textContent = bytes(snapshot.runtime.gw_bytes_out);
        document.getElementById('total-up-in').textContent = bytes(totals.upstreamIn);
        document.getElementById('total-up-out').textContent = bytes(totals.upstreamOut);
    }

    function stateBadge(element, state, label) {
        const symbol = { online: 'check', pending: 'clock', paused: 'pause', offline: 'stop', idle: 'idle' }[state];
        element.dataset.state = state;
        if (element.lastElementChild?.textContent === label && element.dataset.glyph === symbol) return element;
        element.replaceChildren(Object.assign(document.createElement('span'), { textContent: label }));
        setIcon(element, symbol);
        return element;
    }

    function setPanelOpen(button, open) {
        const panel = document.getElementById(button.getAttribute('aria-controls'));
        panel.hidden = !open;
        button.setAttribute('aria-expanded', String(open));
        button.textContent = open ? button.dataset.panelClose : button.dataset.panelLabel;
    }

    document.addEventListener('click', event => {
        const row = event.target.closest('[data-panel-row]');
        if (row && !event.target.closest('button, a, input, label, select, textarea')) {
            const toggle = row.querySelector('button[aria-expanded]:not([hidden]):enabled');
            if (toggle) {
                toggle.focus({preventScroll: true});
                toggle.click();
                return;
            }
        }
        const action = event.target.closest('[data-panel-label]');
        if (action) setPanelOpen(action, action.getAttribute('aria-expanded') !== 'true');
        const trigger = event.target.closest('[data-open-dialog]');
        if (trigger) document.getElementById(trigger.dataset.openDialog).showModal();
        const close = event.target.closest('[data-close-dialog]');
        if (close) close.closest('dialog').close();
    });

    window.modelcatVisuals = { setPanelOpen, setIcon, stateBadge, topologyState, renderCharts, flowState, gatewayTraffic };
})();
