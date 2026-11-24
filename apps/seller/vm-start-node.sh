#!/bin/sh
set -eu

stage=$1
trap 'rm -rf -- "$stage"' EXIT
IFS= read -r host_proxy
umask 077
printf '%s' "$host_proxy" > "$stage/automatic-proxy"
unset host_proxy
umask 022

if ! command -v docker >/dev/null 2>&1; then
    # Lima's current cloud-config contains deprecated keys. Exit 2 is recoverable.
    sudo -n timeout 180 cloud-init status --wait >&2 || [ "$?" -eq 2 ]
    if [ -f /etc/apt/sources.list.d/ubuntu.sources ]; then
        sudo -n sed -i 's#http://ports.ubuntu.com/ubuntu-ports#https://ports.ubuntu.com/ubuntu-ports#g' \
            /etc/apt/sources.list.d/ubuntu.sources
    fi
    sudo -n apt-get -o Acquire::Retries=3 -o Acquire::Languages=none update
    sudo -n env DEBIAN_FRONTEND=noninteractive apt-get \
        -o DPkg::Lock::Timeout=120 -o Acquire::Retries=3 install -y --no-install-recommends docker.io
fi
sudo -n systemctl start docker
sudo -n docker info >/dev/null

data="$HOME/.modelcat"
tool_api="$HOME/.modelcat-tool-api"
install -d -m 700 "$data"
install -m 600 "$stage/automatic-proxy" "$data/automatic-proxy.new"
mv -f "$data/automatic-proxy.new" "$data/automatic-proxy"
install -d -m 700 "$tool_api"
install -m 755 "$stage/modelcat" "$tool_api/relay.new"
mv -f "$tool_api/relay.new" "$tool_api/relay"
checksum=$(sha256sum "$stage/modelcat")
image="modelcat-node:local-${checksum%% *}"
if ! sudo -n docker image inspect "$image" >/dev/null 2>&1; then
    rootfs="$stage/rootfs"
    install -D -m 755 "$stage/modelcat" "$rootfs/usr/local/bin/modelcat"
    install -D -m 644 /etc/ssl/certs/ca-certificates.crt \
        "$rootfs/etc/ssl/certs/ca-certificates.crt"
    dependencies=$(ldd "$rootfs/usr/local/bin/modelcat")
    if printf '%s\n' "$dependencies" | grep -q 'not found'; then
        printf '%s\n' "$dependencies" >&2
        exit 1
    fi
    printf '%s\n' "$dependencies" | awk '$1 ~ /^\// {print $1} $3 ~ /^\// {print $3}' |
        while IFS= read -r library; do
            install -D -m 755 "$library" "$rootfs$library"
        done
    sudo -n docker build --tag "$image" -f - "$rootfs" <<'DOCKERFILE'
FROM scratch
COPY . /
ENTRYPOINT ["/usr/local/bin/modelcat"]
CMD ["serve"]
DOCKERFILE
fi

# A workspace upgraded from the VM experiment may still have the old node running.
legacy="$HOME/.local/bin/modelcat"
if [ -x "$legacy" ] && ! sudo -n docker container inspect modelcat-node >/dev/null 2>&1; then
    if NO_PROXY=127.0.0.1 no_proxy=127.0.0.1 "$legacy" status >/dev/null 2>&1; then
        NO_PROXY=127.0.0.1 no_proxy=127.0.0.1 "$legacy" stop
    fi
fi

active_image=$(sudo -n docker container inspect --format '{{.Config.Image}}' modelcat-node 2>/dev/null || true)
if [ -n "$active_image" ] && [ "$active_image" != "$image" ]; then
    if [ "$(sudo -n docker inspect --format '{{.State.Running}}' modelcat-node)" = true ]; then
        if ! sudo -n timeout 90 docker stop --timeout -1 modelcat-node >/dev/null; then
            echo 'Could not finish draining the ModelCat node. Retry after its requests complete.' >&2
            exit 1
        fi
        exit_code=$(sudo -n docker inspect --format '{{.State.ExitCode}}' modelcat-node)
        if [ "$exit_code" -ne 0 ]; then
            printf 'ModelCat node exited with code %s during upgrade\n' "$exit_code" >&2
            exit 1
        fi
    fi
    sudo -n docker rm modelcat-node
    active_image=
fi
if [ -z "$active_image" ]; then
    sudo -n docker run --detach --name modelcat-node \
        --network host --user "$(id -u):$(id -g)" \
        --cap-drop ALL --security-opt no-new-privileges \
        --mount "type=bind,source=$data,target=/data" \
        --mount "type=bind,source=$tool_api,target=/tool-api" \
        --env HOME=/data --env MODELCAT_DATA=/data \
        --env MODELCAT_VM_WORKSPACE=1 \
        --env MODELCAT_TOOL_API_DIR=/tool-api "$image"
else
    sudo -n docker start modelcat-node
fi

for attempt in $(seq 1 30); do
    if [ -S "$tool_api/model.sock" ] && sudo -n docker exec modelcat-node modelcat status >/dev/null 2>&1; then
        exit 0
    fi
    sleep 1
done
sudo -n docker logs --tail 20 modelcat-node >&2
echo 'ModelCat did not become ready inside its container' >&2
exit 1
