const byId = id => document.getElementById(id);
const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);
const navigation = window.modelcatNavigation;
const status = byId('status');
const toggle = byId('stop-on-exit');
const guest = byId('workspace-frame');
const params = new URLSearchParams(location.search);
let guestOrigin = '';
let guestReady = false;
let runtimeHealth;
let page = 'overview';
const returnPath = [];
const stage = document.querySelector('.desktop-stage');
function rememberOrigin(origin) {
    if (returnPath.length >= 32) returnPath.shift();
    returnPath.push(origin);
}
function hostOrigin(trigger) {
    return {page, scroll: stage.scrollTop, trigger};
}
function renderNavigation() {
    const origin = returnPath.at(-1);
    const label = origin ? '返回' + navigation.pageFor(origin.page).label : '';
    const span = byId('context-back').querySelector('span');
    if (span.textContent !== label) span.textContent = label;
    byId('return-bar').hidden = !origin;
    navigation.select(page, false, origin?.page);
}
byId('context-back').onclick = () => {
    const origin = returnPath.pop();
    if (origin) void openPage(origin.page, true, origin);
};
let directory = '';
let verified = false;
let ready = false;
let running = false;
let busy = true;
let opening = false;
let starting = false;
let stageTimer;
let stageShownAt;
let stopping = false;
let startFailed = false;
let handingOff = false;
let allowArrivalMotion = false;
let workspaceArrival;
let verifying = false;
let verificationFailed = false;
let verificationInterrupted = false;
let savedStopOnExit = true;

function updateConnectivity() {
    const offline = navigator.onLine === false;
    byId('internet-status').hidden = !verified || !offline;
    if (!verified && !verifying) {
        const feedback = byId('platform-status');
        if (offline) {
            feedback.textContent = '当前离线。首次验证需要联网，请连接网络后重试。';
            verificationInterrupted = true;
        } else if (verificationInterrupted) {
            feedback.textContent = '网络已恢复，可以继续验证。';
        } else if (!verificationFailed) {
            feedback.textContent = '';
        }
    }
}

function render() {
    const signIn = ready && !verified && page !== 'settings';
    const showWorkspace = ready && verified && page === 'overview';
    const showGuest = guestReady && usesGuest(page);
    document.body.toggleAttribute('data-guest-visible', showGuest);
    guest.inert = !showGuest;
    document.querySelector('.desktop-stage > main').hidden = showGuest;
    document.body.dataset.entry = !ready ? 'loading' : signIn ? 'login' : verified ? 'workspace' : 'settings';
    renderNavigation();
    byId('entry').hidden = !signIn;
    byId('platform').hidden = !signIn;
    byId('workspace').hidden = !showWorkspace;
    byId('back-to-sign-in').hidden = verified || page !== 'settings';
    byId('account').hidden = !verified || page !== 'account';
    byId('platform-logout').disabled = !verified || busy;
    byId('settings').hidden = page !== 'settings';
    byId('tools').hidden = !verified || page !== 'tools';
    window.vmMonitor.setVisible(verified && running && !stopping && page === 'tools');
    byId('offline').hidden = !ready || !verified || page === 'overview' || navigation.pageFor(page).host;
    const feedback = byId(showWorkspace ? 'workspace-feedback' : 'host-feedback');
    if (status.parentNode !== feedback) feedback.append(status);
    const toolsStart = byId('tools-start-workspace');
    byId('tool-vm-state').textContent = starting ? '启动中' : stopping ? '停止中' : running ? '运行中' : ready && !busy ? '已停止' : '正在检查…';
    byId('tools-network').disabled = busy || !verified;
    byId('tools-network').textContent = running ? '网络设置' : '启动并设置网络';
    toolsStart.hidden = !ready || running;
    toolsStart.disabled = busy;
    toolsStart.querySelector('span').textContent = starting ? '正在启动…' : '启动工作区';
    if (!running && page === 'tools') renderToolWorkspaceState();
    window.modelcatVisuals.stateBadge(byId('account-state'), verified ? 'online' : 'idle',
        !ready ? '正在检查' : verified ? '平台 API Key 已验证' : '尚未验证');
    byId('path-label').textContent = directory || '正在读取';
    byId('choose').disabled = busy || running || !verified;
    const storageHint = byId('storage-hint');
    storageHint.textContent = running ? '停止工作区后可以更换目录。' : !verified && ready ? '验证平台账号后可以选择目录。' : '';
    storageHint.hidden = !storageHint.textContent;
    const start = byId('start');
    start.setAttribute('aria-disabled', String(busy || handingOff));
    start.dataset.state = starting || handingOff ? 'starting' : stopping ? 'pending' : busy && !running ? 'checking' : running ? 'online' : 'idle';
    const action = byId('workspace-action');
    const label = handingOff ? '正在打开运行总览' : stopping ? '停止中' : starting ? '启动中'
        : running ? '打开运行总览' : busy ? '正在检查工作区' : startFailed ? '重试启动工作区' : '启动工作区';
    action.textContent = label;
    start.setAttribute('aria-label', label);
    start.title = label;
    action.hidden = handingOff || !(starting || stopping);
    const overlay = byId('workspace-power');
    overlay.hidden = start.dataset.state !== 'idle';
    window.modelcatVisuals.setIcon(overlay, 'power');
    byId('offline-title').textContent = starting ? '工作区正在启动' : running ? '正在打开工作区' : '工作区已停止';
    byId('offline-detail').textContent = starting || running ? '准备就绪后将自动打开此页。' : '启动本机工作区后，即可使用此功能。';
    byId('offline-start').disabled = busy || (running && page !== 'tools');
    byId('open-network').disabled = !verified || busy;
    byId('open-network').textContent = starting ? '正在启动…' : running ? '打开网络设置' : '启动并打开设置';
    byId('network-detail').textContent = running ? '管理代理规则、工作区出口和连接检测。' : verified ? '此设置使用工作区服务，打开时将启动工作区。' : '验证平台账号后可使用工作区网络设置。';
    updateConnectivity();
}

function renderToolWorkspaceState() {
    const pending = busy && !startFailed;
    window.modelcatVisuals.stateBadge(byId('tool-status'), pending ? 'pending' : 'offline',
        starting ? '工作区启动中' : pending ? '正在检查工作区…' : startFailed ? '工作区启动失败' : '工作区已停止');
}

function usesGuest(destination) {
    return running && !stopping && (destination === 'overview' || !navigation.pageFor(destination).host);
}

function loadGuest(url, destination, fromLaunch = false) {
    const target = new URL(url);
    guestOrigin = target.origin;
    target.searchParams.set('desktop', '1');
    target.searchParams.set('embedded', '1');
    target.searchParams.set('page', destination);
    if (fromLaunch) target.searchParams.set('launch', '1');
    guest.src = target.href;
}

function sendToGuest(message) {
    guest.contentWindow.postMessage(message, guestOrigin);
}

function skipWorkspaceArrival() {
    allowArrivalMotion = false;
    workspaceArrival?.finishEarly();
}

window.addEventListener('blur', skipWorkspaceArrival);
document.addEventListener('visibilitychange', () => {
    if (document.hidden) skipWorkspaceArrival();
});

function animateWorkspaceArrival(target) {
    const start = byId('start');
    const core = start.querySelector('.topology-core');
    const bounds = core.getBoundingClientRect();
    if (!bounds.width || !bounds.height || !target.width || !target.height) return false;

    const focus = document.activeElement === start;
    const frame = guest.getBoundingClientRect();
    const x = frame.left + target.left - bounds.left;
    const y = frame.top + target.top - bounds.top;
    const scale = target.width / bounds.width;
    const fromBorder = getComputedStyle(core).borderColor;
    const movingCore = core.cloneNode(false);
    movingCore.append(core.querySelector('img').cloneNode());
    Object.assign(movingCore.style, {left: bounds.left + 'px', top: bounds.top + 'px', width: bounds.width + 'px'});
    movingCore.classList.add('workspace-moving-core');
    document.body.append(movingCore);
    const move = movingCore.animate([
        {transform: 'translate(0, 0) scale(1)', borderColor: fromBorder},
        {transform: 'translate(' + x + 'px, ' + y + 'px) scale(' + scale + ')', borderColor: target.borderColor}
    ], {duration: 620, easing: 'cubic-bezier(.45, 0, .22, 1)', fill: 'both'});
    let revealTimeout;
    const complete = (moveFocus = false) => {
        if (revealTimeout !== undefined) return;
        sendToGuest({type: 'arrival-complete', focus: focus && moveFocus});
        revealTimeout = setTimeout(cleanup, 500);
    };
    const cleanup = () => {
        clearTimeout(revealTimeout);
        window.removeEventListener('resize', finishEarly);
        workspaceArrival = undefined;
        move.cancel();
        movingCore.remove();
    };
    const finishEarly = () => {
        complete();
        cleanup();
    };
    window.addEventListener('resize', finishEarly);
    workspaceArrival = {cleanup, finishEarly};
    move.finished.then(() => complete(page === 'overview'), () => {});
    return true;
}

window.addEventListener('modelcat-runtime', event => {
    runtimeHealth = event.detail;
    if (guestReady) sendToGuest({type: 'runtime', health: runtimeHealth});
});

window.addEventListener('message', event => {
    if (event.source !== guest.contentWindow || event.origin !== guestOrigin) return;
    const message = event.data;
    if (guestReady && message?.type === 'workspace-authorization' && event.ports[0]) {
        const port = event.ports[0];
        invoke('workspace_authorization', {attempt: message.attempt})
            .then(() => port.postMessage({}), error => port.postMessage({error: String(error)}))
            .finally(() => port.close());
    } else if (guestReady && message?.type === 'close-workspace-authorization') {
        void invoke('close_workspace_authorization', {attempt: message.attempt}).catch(() => {});
    } else if (message?.type === 'workspace-ready') {
        guestReady = true;
        if (runtimeHealth) sendToGuest({type: 'runtime', health: runtimeHealth});
        if (usesGuest(page) && message.page !== page) sendToGuest({type: 'page', page});
        const moving = handingOff && allowArrivalMotion && !document.hidden && document.hasFocus()
            && page === 'overview' && message.arrival && animateWorkspaceArrival(message.arrival);
        allowArrivalMotion = false;
        if (message.arrival && !moving) sendToGuest({type: 'arrival-complete'});
        if (handingOff) status.textContent = '';
        handingOff = false;
        render();
    } else if (guestReady && message?.type === 'arrival-visible') {
        workspaceArrival?.cleanup();
    } else if (guestReady && usesGuest(page) && message?.type === 'page-ready' && message.page === page) {
        render();
        if (message.focus) sendToGuest({type: 'focus', page, restore: message.restore});
    } else if (guestReady && usesGuest(page) && message?.type === 'page') {
        const destination = navigation.pageFor(message.page);
        if (destination?.id === 'overview' || destination && !destination.host) {
            if (destination.id !== 'overview') skipWorkspaceArrival();
            if (message.origin?.page === page && destination.id !== page) rememberOrigin(message.origin);
            if (message.traceReturn && returnPath.at(-1)?.page === destination.id) returnPath.pop();
            page = destination.id;
            savePage();
            renderNavigation();
        }
    } else if (guestReady && usesGuest(page) && message?.type === 'host') {
        if (message.action === 'stop') void stopWorkspace();
        if (message.action === 'settings') void openPage('settings', true);
        if (message.action === 'tools') {
            if (message.origin?.page === page) rememberOrigin(message.origin);
            void openPage('tools', true);
        }
    }
});

function savePage() {
    if (params.get('page') !== page && (page !== 'overview' || params.has('page'))) {
        params.set('page', page);
        history.replaceState(null, '', location.pathname + '?' + params);
    }
}

async function openPage(destination, moveFocus = false, restoreOrigin) {
    if (!verified && destination !== 'settings') destination = 'overview';
    const next = navigation.pageFor(destination);
    if (!next) return;
    if (stopping || (!running && destination === 'network')) return;
    const previous = page;
    if (destination !== 'overview') skipWorkspaceArrival();
    if (guestReady && usesGuest(previous) && !usesGuest(destination)) sendToGuest({type: 'workspace-hidden'});
    page = destination;
    savePage();
    if (usesGuest(page) && guestReady) {
        // Keep the current pane visible until the workbench has selected the requested page.
        sendToGuest({type: 'page', page, focus: moveFocus, restore: restoreOrigin});
        return;
    }
    render();
    if (usesGuest(page)) {
        if (opening || guestOrigin) return;
        opening = true;
        try {
            const url = await invoke('vm_open');
            if (url) {
                loadGuest(url, usesGuest(page) ? page : destination);
                return;
            }
            running = false;
            render();
        } catch (error) {
            status.textContent = String(error);
        } finally {
            opening = false;
        }
    }
    if (page === 'tools') {
        window.modelcatUpdateTools?.(running);
        void loadVmLimits();
    }
    if (page === 'account' || previous === 'account') window.modelcatUpdateAccount();
    if (moveFocus) {
        const target = restoreOrigin?.trigger || byId(verified || page === 'settings' ? 'page-title' : 'key');
        target.focus({preventScroll: true});
        stage.scrollTop = restoreOrigin?.scroll || 0;
    }
}
window.modelcatOpenPage = (destination, trigger) => {
    if (destination !== page) rememberOrigin(hostOrigin(trigger));
    return openPage(destination, true);
};

function showStartupStage(message) {
    clearTimeout(stageTimer);
    if (status.textContent === message) return;
    const show = () => {
        stageShownAt = performance.now();
        status.textContent = message;
        if (!matchMedia('(prefers-reduced-motion: reduce)').matches) {
            status.animate([{opacity: .45, transform: 'translateY(3px)'}, {opacity: 1, transform: 'none'}],
                {duration: 160, easing: 'ease-out'});
        }
    };
    const delay = stageShownAt + 900 - performance.now();
    if (delay > 0) stageTimer = setTimeout(show, delay);
    else show();
}

async function startWorkspace() {
    if (busy || !verified) return;
    busy = true;
    starting = true;
    allowArrivalMotion = page === 'overview' && !document.hidden && document.hasFocus();
    startFailed = false;
    stageShownAt = -Infinity;
    render();
    showStartupStage('正在检查工作区…');
    try {
        const url = await invoke('vm_start', {directory});
        if (stopping) return;
        running = true;
        if (page === 'overview' || !navigation.pageFor(page).host) {
            handingOff = page === 'overview';
            status.textContent = handingOff ? '正在打开首页…' : '';
            loadGuest(url, page, handingOff && allowArrivalMotion);
        }
        else {
            status.textContent = '';
            if (page === 'tools') window.modelcatUpdateTools?.(running);
        }
    } catch (error) {
        handingOff = false;
        startFailed = !running;
        status.textContent = String(error).includes('Lima 状态检查超时')
            ? '无法读取虚拟机状态。请在设置中重新选择工作区目录后重试。'
            : String(error);
    } finally {
        busy = stopping;
        starting = false;
        clearTimeout(stageTimer);
        render();
    }
}

function syncResourceControls() {
    const form = byId('vm-limits');
    for (const button of form.querySelectorAll('[data-step]')) {
        const input = button.parentElement.querySelector('input');
        button.disabled = button.dataset.step === '-1' ? input.valueAsNumber <= Number(input.min) :
            Boolean(input.max) && input.valueAsNumber >= Number(input.max);
    }
    form.querySelector('[type=submit]').disabled = ![...form.querySelectorAll('input')].some(input => input.value !== input.defaultValue) || !form.checkValidity();
}
byId('vm-limits').oninput = syncResourceControls;
byId('vm-limits').onclick = event => {
    const button = event.target.closest('[data-step]');
    if (!button) return;
    const input = button.parentElement.querySelector('input');
    if (button.dataset.step === '1') input.stepUp();
    else input.stepDown();
    syncResourceControls();
};
async function loadVmLimits(limits) {
    const form = byId('vm-limits');
    const feedback = byId('limits-feedback');
    const fields = form.querySelector('fieldset');
    fields.disabled = true;
    try {
        const data = await invoke('vm_resources', limits ? {limits} : {});
        const saved = data.limits;
        for (const key of Object.keys(saved)) form.elements[key].defaultValue = form.elements[key].value = saved[key];
        form.elements.cpus.max = data.max_cpu;
        form.elements.memory_gib.max = data.max_memory;
        form.elements.disk_gib.min = data.min_disk;
        byId('resource-summary').textContent = saved.cpus + ' 核 · ' + saved.memory_gib + ' GiB 内存 · ' + saved.disk_gib + ' GiB 磁盘';
        const pending = Object.keys(saved).some(key => saved[key] !== data.current[key]);
        feedback.textContent = pending ? '已保存，下次启动生效' : limits ? '资源设置已保存' : '';
        fields.disabled = false;
        syncResourceControls();
    } catch (error) {
        feedback.textContent = String(error);
        fields.disabled = !form.elements.cpus.value;
    }
    feedback.hidden = !feedback.textContent;
}
byId('vm-limits').onsubmit = event => {
    event.preventDefault();
    const form = event.currentTarget;
    const limits = Object.fromEntries(['cpus','memory_gib','disk_gib'].map(key => [key,form.elements[key].valueAsNumber]));
    void loadVmLimits(limits);
};
const appearanceControl = byId('appearance');
let savedAppearance = 'system';
async function updateAppearance(mode) {
    const error = byId('appearance-error');
    appearanceControl.disabled = true;
    error.hidden = true;
    try {
        savedAppearance = await invoke('desktop_appearance', mode ? {mode} : {});
    } catch (failure) {
        error.textContent = String(failure);
        error.hidden = false;
    } finally {
        appearanceControl.querySelector('input[value="' + savedAppearance + '"]').checked = true;
        appearanceControl.disabled = false;
    }
}
appearanceControl.onchange = event => void updateAppearance(event.target.value);

async function updateStopOnExit(enabled) {
    const error = byId('settings-error');
    const retry = byId('exit-retry');
    const feedback = byId('exit-save-status');
    toggle.disabled = true;
    retry.hidden = true;
    error.hidden = true;
    feedback.textContent = enabled === undefined ? '' : '正在保存运行偏好…';
    try {
        savedStopOnExit = await invoke('vm_stop_on_exit', enabled === undefined ? {} : {enabled});
        toggle.checked = savedStopOnExit;
        toggle.disabled = false;
        if (enabled !== undefined) feedback.textContent = savedStopOnExit ? '已保存，退出时会关闭工作区。' : '已保存，退出后工作区将继续运行。';
    } catch (failure) {
        feedback.textContent = '';
        error.textContent = String(failure);
        error.hidden = false;
        toggle.checked = savedStopOnExit;
        retry.hidden = false;
    }
}

async function restore() {
    try {
        verified = await invoke('vm_platform_verified');
        if (!verified) {
            const market = new URL('/market', await invoke('vm_platform_gateway'));
            byId('platform-link').href = market.href;
            byId('platform-host').textContent = market.host;
        }
        directory = await invoke(verified ? 'vm_workspace_directory' : 'vm_default_directory');
        ready = true;
        busy = verified;
        status.textContent = params.get('status') || '';
        const requestedPage = navigation.pageFor(params.get('page'))?.id;
        await openPage(requestedPage && requestedPage !== 'network' ? requestedPage : 'overview');
        if (!verified && page === 'overview') byId('key').focus();
        if (!verified) return;
        running = await invoke('vm_attach_workspace', {directory});
        busy = false;
        render();
        if (running) {
            if (params.get('action') === 'stop') {
                await stopWorkspace();
                return;
            }
            if (page === 'tools') window.modelcatUpdateTools?.(running);
            if ((page === 'overview' && !params.has('status')) || !navigation.pageFor(page).host) await openPage(page);
        }
    } catch (error) {
        ready = true;
        busy = false;
        status.textContent = String(error);
        render();
        if (!verified && page === 'overview') {
            byId('platform-status').textContent = '暂时无法读取本机的工作区状态，请重新打开 ModelCat。';
        }
    }
}

navigation.onNavigate(destination => {
    returnPath.length = 0;
    void openPage(destination, true);
});
function showWorkspaceStopping() {
    returnPath.length = 0;
    workspaceArrival?.finishEarly();
    clearTimeout(stageTimer);
    handingOff = starting = false;
    page = 'overview';
    busy = true;
    stopping = true;
    status.textContent = '';
    render();
}

async function stopWorkspace() {
    if (busy || !running) return;
    showWorkspaceStopping();
    try {
        await invoke('vm_stop');
    } catch (error) {
        busy = false;
        stopping = false;
        status.textContent = String(error);
        render();
    }
}
byId('start').onclick = () => {
    if (busy || handingOff) return;
    if (running) void openPage('overview', true);
    else void startWorkspace();
};
byId('offline-start').onclick = byId('tools-start-workspace').onclick = startWorkspace;
byId('platform-logout').onclick = async () => {
    if (busy || !verified) return;
    busy = true;
    stopping = true;
    const button = byId('platform-logout');
    button.textContent = running ? '正在停止工作区并退出…' : '正在退出…';
    status.textContent = '';
    render();
    try {
        await invoke('vm_logout_platform');
        location.replace(location.pathname);
    } catch (error) {
        status.textContent = '退出登录失败：' + String(error);
        stopping = busy = false;
        button.textContent = '退出登录';
        render();
    }
};
byId('back-to-sign-in').onclick = () => openPage('overview', true);
byId('tools-network').onclick = byId('open-network').onclick = async event => {
    const origin = hostOrigin(event.currentTarget);
    if (!running) await startWorkspace();
    if (running && page === origin.page) {
        rememberOrigin(origin);
        await openPage('network', true);
    }
};
toggle.onchange = () => updateStopOnExit(toggle.checked);
byId('exit-retry').onclick = () => updateStopOnExit();

byId('choose').onclick = async () => {
    busy = true;
    render();
    try {
        const selected = await invoke('vm_choose_directory', {directory});
        if (selected) {
            directory = selected;
            status.textContent = '';
        }
    } catch (error) {
        status.textContent = String(error);
    } finally {
        busy = false;
        render();
    }
};

byId('platform-form').onsubmit = async event => {
    event.preventDefault();
    if (verifying || verified) return;
    if (navigator.onLine === false) {
        updateConnectivity();
        return;
    }
    const submit = byId('platform-submit');
    const key = byId('key');
    const feedback = byId('platform-status');
    verifying = true;
    verificationFailed = false;
    verificationInterrupted = false;
    key.readOnly = true;
    key.removeAttribute('aria-invalid');
    submit.disabled = true;
    submit.textContent = '正在验证…';
    feedback.textContent = '';
    try {
        await invoke('vm_verify_platform', {key: key.value});
        key.value = '';
        verified = true;
        await openPage('overview', true);
    } catch (error) {
        verificationFailed = true;
        const message = String(error);
        feedback.textContent = message;
        if (/拒绝了此 API Key|API Key 格式无效/.test(message)) key.setAttribute('aria-invalid', 'true');
    } finally {
        verifying = false;
        key.readOnly = false;
        submit.disabled = false;
        submit.textContent = verificationFailed ? '重试验证' : '验证并继续';
        updateConnectivity();
        if (verificationFailed && page === 'overview') key.focus();
    }
};

window.addEventListener('online', updateConnectivity);
window.addEventListener('offline', updateConnectivity);

void updateAppearance();
updateStopOnExit();
Promise.all([
    window.__TAURI__.event.listen('tauri://blur', skipWorkspaceArrival),
    window.__TAURI__.event.listen('workspace-stopping', showWorkspaceStopping),
    window.__TAURI__.event.listen('workspace-start-progress', event => {
        if (starting && !handingOff) showStartupStage(event.payload);
    })
]).catch(() => {}).then(restore);
