window.modelcatDrawer = function (dialog, closeButton, {beforeClose, afterClose} = {}) {
    function onBackdrop(event) {
        if (event.target !== dialog) return false;
        const {left, right, top, bottom} = dialog.getBoundingClientRect();
        return event.clientX < left || event.clientX >= right || event.clientY < top || event.clientY >= bottom;
    }

    let pendingClose;
    async function close(restoreFocus = true) {
        if (!dialog.open || dialog.inert) return;
        dialog.inert = true;
        beforeClose?.();
        dialog.style.setProperty('--drawer-from', getComputedStyle(dialog).transform);
        dialog.dataset.closing = '';
        const exit = dialog.getAnimations().find(animation => animation.animationName === 'drawer-leave');
        pendingClose = exit;
        if (exit) await exit.finished.catch(() => {});
        if (dialog.open && dialog.inert && pendingClose === exit) finish(restoreFocus);
    }

    function finish(restoreFocus) {
        pendingClose = null;
        dialog.close();
        delete dialog.dataset.closing;
        dialog.style.removeProperty('--drawer-from');
        dialog.inert = false;
        afterClose?.(restoreFocus);
    }

    let pressedBackdrop = false;
    dialog.onpointerdown = event => { pressedBackdrop = event.button === 0 && onBackdrop(event); };
    dialog.onclick = event => {
        if (pressedBackdrop && onBackdrop(event) && !closeButton.disabled) void close();
        pressedBackdrop = false;
    };
    dialog.oncancel = event => {
        event.preventDefault();
        if (!closeButton.disabled) void close();
    };
    closeButton.onclick = () => close();
    return {open() { if (!dialog.open) dialog.showModal(); }, close, dismiss() {
        if (!dialog.open) return;
        if (!dialog.inert) beforeClose?.();
        finish(false);
    }};
};
