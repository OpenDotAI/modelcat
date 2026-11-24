(() => {
    const root = document.documentElement;
    document.addEventListener('pointerdown', () => root.removeAttribute('data-keyboard-navigation'), true);
    document.addEventListener('keydown', event => {
        if (event.key === 'Tab' || event.key.startsWith('Arrow')) {
            root.setAttribute('data-keyboard-navigation', '');
        }
    }, true);

    const sidebar = document.querySelector('.sidebar');
    const host = sidebar.dataset.shell === 'host';
    const desktop = host || new URLSearchParams(location.search).get('desktop') === '1';
    const pages = [
        { id: 'overview', group: 'run', label: desktop ? '首页' : '工作区', icon: 'overview', host: desktop },
        { id: 'requests', group: 'run', label: '请求记录', icon: 'activity' },
        { id: 'models', group: 'models', label: '模型配置', icon: 'models' },
        { id: 'local', group: 'models', label: '本地调用', icon: 'terminal' },
        { id: 'tools', group: 'models', label: '工具管理', icon: 'tools', host: true },
        { id: 'seller', group: 'seller', label: '卖家中心', icon: 'seller' },
        { id: 'billing', group: 'seller', label: '收益与记录', icon: 'billing' },
        { id: 'account', label: '账号', icon: 'account', host: true, footer: true },
        { id: 'settings', label: '设置', icon: 'settings', host: true, footer: true },
        { id: 'network', label: '网络设置', icon: 'network', footer: true }
    ];

    const logo = document.createElement('img');
    logo.src = 'modelcat.svg';
    logo.width = logo.height = 36;
    logo.alt = '';
    const brand = document.createElement('div');
    brand.className = 'brand';
    brand.append(logo, 'ModelCat');

    const primary = document.createElement('nav');
    primary.className = 'sidebar-primary';
    primary.setAttribute('aria-label', '主导航');
    const footer = document.createElement('nav');
    footer.className = 'sidebar-footer';
    footer.setAttribute('aria-label', desktop ? '账号与设置' : '设置');

    const groups = new Map([
        ['run', '运行'],
        ['models', '模型与工具'],
        ['seller', '平台接单']
    ].map(([id, name]) => {
        const group = document.createElement('div');
        group.className = 'sidebar-group';
        group.setAttribute('role', 'group');
        const label = document.createElement('p');
        label.id = 'nav-' + id;
        label.textContent = name;
        group.setAttribute('aria-labelledby', label.id);
        group.append(label);
        return [id, group];
    }));

    const controls = new Map();
    for (const page of pages) {
        if ((!desktop && page.host) || (desktop && page.id === 'network')) continue;
        const forwardsToHost = !host && page.host && page.id !== 'overview';
        const control = document.createElement(forwardsToHost ? 'a' : 'button');
        if (forwardsToHost) control.href = 'tauri://localhost/vm.html?page=' + page.id;
        else control.type = 'button';
        control.className = 'sidebar-entry';
        control.dataset.icon = page.icon;
        control.textContent = page.label;
        const parent = page.footer ? footer : groups.get(page.group) || primary;
        if (page.group && !parent.parentNode) primary.append(parent);
        parent.append(control);
        controls.set(page.id, control);
    }
    sidebar.append(brand, primary, footer);

    function pageFor(id) {
        return pages.find(page => page.id === id);
    }

    function select(id, focus = false, networkOrigin = 'settings') {
        const current = desktop && id === 'network' ? networkOrigin : id;
        for (const [key, control] of controls) {
            if (key === current) control.setAttribute('aria-current', 'page');
            else control.removeAttribute('aria-current');
        }
        const title = document.getElementById('page-title');
        const page = pageFor(id);
        if (title.textContent !== page.label) title.textContent = page.label;
        if (id !== 'overview') window.modelcatVisuals.setIcon(title, page.icon);
        if (focus) title.focus();
    }

    function onNavigate(handler) {
        for (const [id, control] of controls) {
            if (control.tagName === 'BUTTON') control.addEventListener('click', () => handler(id));
        }
    }

    window.modelcatNavigation = { pageFor, select, onNavigate };
})();
