(() => {
    const byId = id => document.getElementById(id);
    const form = byId('account-form');
    const account = byId('account');
    const proxy = byId('account-proxy');
    proxy.addEventListener('invalid', () => {
        window.modelcatVisuals.setPanelOpen(document.querySelector('[aria-controls="login-proxy-fields"]'), true);
    });
    const start = byId('account-start');
    const cancel = byId('account-cancel');
    const recheck = byId('account-recheck');
    const logout = byId('account-logout');
    const status = byId('account-status');
    const profiles = byId('account-profile');
    const addProfile = byId('account-add-profile');
    const profileForm = byId('account-profile-form');
    const profileName = byId('account-profile-name');
    const selectionKey = () => 'modelcat.account.selection.' + byId('path-label').textContent;
    let state = '';
    let busy = false;
    let poll;

    function render(message) {
        const waiting = state === 'waiting';
        const signedIn = state === 'signed_in';
        status.textContent = message || ({
            waiting: '请在弹出的 OpenAI 页面完成授权，登录后会返回 ModelCat。',
            signed_in: '',
            signed_out: ''
        }[state] ?? '正在检查 Codex 登录状态…');
        status.hidden = !status.textContent;
        window.modelcatVisuals.stateBadge(byId('codex-login-state'), signedIn ? 'online' : waiting ? 'pending' : 'idle',
            signedIn ? '已登录' : waiting ? '等待授权' : state === 'signed_out' ? '未登录' : '状态未知');
        start.hidden = state !== 'signed_out';
        byId('account-login-hint').hidden = start.hidden;
        addProfile.hidden = waiting || (!signedIn && profiles.options.length <= 1);
        cancel.hidden = !waiting;
        logout.hidden = !signedIn;
        for (const button of form.querySelectorAll('button')) button.disabled = busy;
        recheck.disabled = busy;
        for (const field of profileForm.elements) field.disabled = busy || waiting;
        proxy.disabled = busy || waiting;
        profiles.disabled = addProfile.disabled = busy || waiting;
    }

    async function action(name) {
        if (busy) return;
        clearTimeout(poll);
        busy = true;
        render(name === 'start' ? '正在打开 OpenAI 授权页…' : undefined);
        let error;
        try {
            const result = await window.__TAURI__.core.invoke('codex_account', { action: name, proxy: proxy.value, profileId: profiles.value });
            state = result.state;
        } catch (failure) {
            if (name === 'status') state = '';
            error = String(failure);
        } finally {
            busy = false;
            render(error);

            if (state === 'waiting' && !account.hidden) poll = setTimeout(() => action('status'), 1500);
        }
    }

    async function loadProfiles(name) {
        const result = await window.__TAURI__.core.invoke('codex_profiles', {name});
        const selected = result.selected || localStorage.getItem(selectionKey()) || profiles.value;
        profiles.replaceChildren(...result.profiles.map(profile => new Option(profile.label, profile.id)));
        profiles.value = result.profiles.some(profile => profile.id === selected) ? selected : 'default';
        profiles.closest('.form-field').hidden = result.profiles.length === 1;
        localStorage.setItem(selectionKey(), profiles.value);
    }
    profiles.onchange = () => {
        localStorage.setItem(selectionKey(), profiles.value);
        state = '';
        action('status');
    };
    function showProfileForm(open) {
        profileForm.hidden = !open;
        addProfile.setAttribute('aria-expanded', String(open));
        if (open) {
            profileName.value = '';
            profileName.focus();
            profileName.select();
        } else {
            profileForm.reset();
            profileName.setCustomValidity('');
            addProfile.focus();
        }
    }
    addProfile.onclick = () => showProfileForm(profileForm.hidden);
    byId('account-dismiss').onclick = () => showProfileForm(false);
    profileName.oninput = () => profileName.setCustomValidity('');
    profileForm.onsubmit = async event => {
        event.preventDefault();
        if (busy || state === 'waiting') return;
        if (!proxy.reportValidity()) return;
        const names = new Set([...profiles.options].map(option => option.text));
        let name = profileName.value.trim();
        if (!name) {
            let number = names.size + 1;
            while (names.has('账号 ' + number)) number++;
            name = '账号 ' + number;
        }
        if (names.has(name)) {
            profileName.setCustomValidity('此备注已使用，请换一个。');
            profileName.reportValidity();
            return;
        }
        busy = true;
        render('正在准备授权…');
        try {
            await loadProfiles(name);
            busy = false;
            showProfileForm(false);
            state = 'signed_out';
            await action('start');
            if (!account.hidden) (state === 'waiting' ? cancel : start).focus();
        } catch (error) {
            busy = false;
            render(String(error));
        }
    };

    form.onsubmit = event => {
        event.preventDefault();
        action('start');
    };
    recheck.onclick = () => window.modelcatUpdateAccount();
    cancel.onclick = () => action('cancel');
    logout.onclick = () => action('logout');
    window.modelcatUpdateAccount = () => {
        clearTimeout(poll);
        if (account.hidden || busy) return;
        busy = true;
        render('正在检查账号…');
        loadProfiles(null).then(() => {
            busy = false;
            return action('status');
        }, error => {
            state = '';
            busy = false;
            render(String(error));
        });
    };
})();
