(() => {
    const el = id => document.getElementById(id);
    const dialog = el('vm-monitor');
    const opener = el('monitor-open');
    let visible = false, period = 300, revision = 0, timer, data;
    const clock = time => new Date(time * 1000).toLocaleTimeString('zh-CN', {hour:'2-digit',minute:'2-digit',hour12:false});
    function size(value) {
        const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
        let i = 0;
        while (value >= 1024 && i < units.length - 1) { value /= 1024; i++; }
        return Number(value.toFixed(i >= 3 ? 2 : i === 1 ? 1 : 0)) + ' ' + units[i];
    }
    const rate = n => size(n) + '/s';
    function chart(id, fields, value, detail, fixedMax) {
        const card = el('chart-' + id);
        card.querySelector('strong').textContent = value;
        card.querySelector('.chart-detail').textContent = detail;
        const canvas = card.querySelector('canvas');
        const width = canvas.clientWidth, height = canvas.clientHeight;
        if (!width || !height) return;
        const scale = devicePixelRatio || 1;
        canvas.width = Math.round(width * scale);
        canvas.height = Math.round(height * scale);
        const ctx = canvas.getContext('2d');
        ctx.scale(scale, scale);
        const style = getComputedStyle(card);
        const color = token => style.getPropertyValue(token).trim();
        const left = data.now - period;
        const points = data.points;
        const peak = Math.max(1, ...points.flatMap(p => fields.map(f => p[f])));
        const max = id === 'cpu' ? [5,10,25,50,100].find(v => v >= Math.min(100,peak))
            : fixedMax || 2 ** Math.ceil(Math.log2(peak));
        const tickLabel = n => id === 'cpu' ? n+'%' : id === 'mem' ? size(n) : rate(n);
        ctx.font = '11px system-ui';
        const ticksY = [max, max / 2, 0];
        const gutter = Math.ceil(Math.max(...ticksY.map(n => ctx.measureText(tickLabel(n)).width))) + 9;
        const top = 9, bottom = height - 8;
        ctx.textBaseline = 'middle';
        for (const n of ticksY) {
            const y = bottom - n/max * (bottom-top);
            ctx.fillStyle = color('--muted');
            ctx.fillText(tickLabel(n), 0, y);
            ctx.strokeStyle = color('--border');
            ctx.lineWidth = .7;
            ctx.beginPath();
            ctx.moveTo(gutter,y);
            ctx.lineTo(width,y);
            ctx.stroke();
        }
        card.querySelector('.chart-axis').style.paddingLeft = gutter+'px';
        fields.forEach((field, i) => {
            ctx.strokeStyle = ctx.fillStyle = color(i ? '--chart-warm' : '--chart-cool');
            ctx.setLineDash(i ? [4,2] : []);
            ctx.lineWidth = 1.7;
            ctx.beginPath();
            let previous;
            for (const point of points) {
                const x = gutter + (point.stamp-left)/period * (width-gutter-2);
                const y = bottom - (bottom-top)*Math.min(1,point[field]/max);
                if (!previous || point.segment !== previous.segment) {
                    ctx.moveTo(x,y);
                    ctx.fillRect(x-1,y-1,2,2);
                } else ctx.lineTo(x,y);
                previous = point;
            }
            ctx.stroke();
        });
        const ticks = card.querySelectorAll('.chart-axis small');
        ticks[0].textContent = (period === 86400 ? '昨天 ' : '') + clock(left);
        ticks[1].textContent = clock(data.now);
    }
    function render() {
        if (!dialog.open || !data) return;
        const s = data.latest;
        const live = s && data.now - s.stamp < 20;
        el('monitor-time').textContent = s ? '最近采样 ' + clock(s.stamp) : '';
        el('monitor-message').textContent = !s ? '等待虚拟机首次采样…' : !live ? '采样已中断，图中显示最近记录。' : '';
        chart('cpu',['cpu'],live ? s.cpu.toFixed(1)+'%' : '—','虚拟机全部核心',100);
        chart('mem',['mem'],live ? size(s.mem) : '—',s ? '共 '+size(s.ram) : '',s?.ram || 1);
        chart('net',['rx','tx'],live ? '↓ '+rate(s.rx)+'　↑ '+rate(s.tx) : '—','接收（实线） · 发送（虚线）');
        chart('disk',['rd','wr'],live ? '读 '+rate(s.rd)+'　写 '+rate(s.wr) : '—',
            s ? '可用 '+size(s.free)+' / '+size(s.total)+' · 写入为虚线' : '');
    }
    function refresh() {
        clearTimeout(timer);
        const request = ++revision;
        if (!visible) return;
        const seconds = dialog.open ? period : 0;
        window.__TAURI__.core.invoke('vm_usage', {seconds}).then(result => {
            if (request !== revision) return;
            data = result;
            const live = result.latest && result.now-result.latest.stamp < 20;
            el('vm-free').textContent = live ? '磁盘可用 '+size(result.latest.free) : '';
            render();
        }).catch(() => {
            if (request !== revision) return;
            el('vm-free').textContent = '';
            el('monitor-message').textContent = '暂时无法读取虚拟机用量，正在重试。';
        }).finally(() => {
            if (request === revision) timer = setTimeout(refresh, 5000);
        });
    }
    opener.onclick = () => {
        dialog.showModal();
        refresh();
    };
    el('monitor-close').onclick = () => dialog.close();
    dialog.addEventListener('close', refresh);
    el('monitor-ranges').onchange = e => {
        period = Number(e.target.value);
        if (data) { data = {...data,points:[]}; render(); }
        refresh();
    };
    new ResizeObserver(render).observe(dialog);
    window.vmMonitor = {setVisible(next) {
        if (visible === next) return;
        visible = next;
        opener.hidden = !next;
        if (!next) {
            if (dialog.open) dialog.close();
            data = undefined;
            el('vm-free').textContent = '';
        }
        refresh();
    }};
})();
