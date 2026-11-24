#!/bin/sh
set -eu

action=$1
tool=$2
case "$tool" in claude|codex) ;; *) exit 2 ;; esac
container="modelcat-tool-$tool"
tools_dir="$HOME/.modelcat-tools"
root="$tools_dir/$tool"
workspace="$root/workspace"
profile="$root/profile"
api="$HOME/.modelcat-tool-api"
image="modelcat-tool-$tool:local"

docker() { sudo -n docker "$@"; }
fail() { printf '%s\n' "$*" >&2; exit 1; }
exists() { docker container inspect "$container" >/dev/null 2>&1; }
base=modelcat-tool-base:noble
installed() {
    docker image inspect "$image" >/dev/null 2>&1 ||
        { [ -x "$root/program" ] && docker image inspect "$base" >/dev/null 2>&1; }
}
network_mode() { if [ -f "$root/offline" ]; then echo none; else echo bridge; fi; }
running() { [ "$(docker inspect --format '{{.State.Running}}' "$container" 2>/dev/null)" = true ]; }
has_model_socket() {
    docker inspect --format '{{index .Config.Labels "ai.opendot.modelcat.tool-api"}}' "$container" 2>/dev/null | grep -qx unix
}
owned_directory() { [ -d "$1" ] && [ ! -L "$1" ] && [ -O "$1" ]; }
owned_marker() { [ -f "$1" ] && [ ! -L "$1" ] && [ -O "$1" ]; }
authorized() {
    for directory in "$tools_dir" "$root" "$workspace" "$profile"; do
        owned_directory "$directory" || return 1
    done
    owned_marker "$root/authorized"
}

preflight() {
    docker info >/dev/null 2>&1 || fail 'Docker is not ready in the workspace VM'
    authorized || fail 'Authorize the tool workspace before starting the container'
    [ -S "$api/model.sock" ] && [ -r "$api/model.key" ] && [ -x "$api/relay" ] ||
        fail 'The workspace model API is not ready'
    available=$(awk '/^MemAvailable:/ {print $2}' /proc/meminfo)
    [ "$available" -ge 393216 ] || fail '虚拟机可用内存不足 384 MiB，请在资源上限中增加内存'

}

case "$action" in
    authorize)
        for directory in "$tools_dir" "$root" "$workspace" "$profile"; do
            [ -e "$directory" ] || [ -L "$directory" ] || install -d -m 700 "$directory"
            owned_directory "$directory" || fail 'The tool directory must be owned by the workspace user and cannot be a symbolic link'
        done
        marker="$root/authorized"
        if [ -e "$marker" ] || [ -L "$marker" ]; then
            owned_marker "$marker" || fail 'The tool authorization marker is invalid'
        fi
        touch "$root/authorized"
        printf '%s\n' "$workspace"
        ;;
    stage)
        preflight
        if ! installed; then mktemp -d /tmp/modelcat-tool.XXXXXXXXXX; fi
        ;;
    install-space)
        required=$3
        if ! docker image inspect "$base" >/dev/null 2>&1; then
            required=$((required + $4))
            echo base
        fi
        free=$(df -Pk "$root" | awk 'NR == 2 {print $4}')
        [ "$free" -ge "$required" ] || fail "虚拟磁盘可用 $((free / 1024)) MiB，本次安装至少需要 $(((required + 1023) / 1024)) MiB。可在资源上限中增加磁盘容量"
        ;;
    start|restart)
        stage=${3:-}
        if [ -n "$stage" ]; then
            case "$stage" in /tmp/modelcat-tool.*) ;; *) fail 'Invalid tool staging directory' ;; esac
            trap 'rm -rf -- "$stage"' EXIT
        fi
        network=$(network_mode)
        if [ "$action" = start ] && exists && has_model_socket && running; then
            exit 0
        fi
        preflight
        if ! installed; then
            [ -n "$stage" ] || fail 'Tool files are missing'
            if ! docker image inspect modelcat-tool-base:noble >/dev/null 2>&1; then
                docker import "$stage/ubuntu-base.tar.gz" modelcat-tool-base:noble >/dev/null
            fi
            chmod 555 "$stage/program"
            mv -f "$stage/program" "$root/program"
        fi
        if exists && { ! has_model_socket || [ "$(docker inspect --format '{{.HostConfig.NetworkMode}}' "$container")" != "$network" ]; }; then
            docker rm --force "$container" >/dev/null
        fi
        if exists; then
            if [ "$action" = restart ]; then
                docker restart --time 5 "$container" >/dev/null
            else
                docker start "$container" >/dev/null
            fi
        else
            set --
            if ! docker image inspect "$image" >/dev/null 2>&1; then
                image=$base
                set -- --mount "type=bind,source=$root/program,target=/usr/local/bin/modelcat-program,readonly"
            fi
            docker run --detach --name "$container" --network "$network" "$@" --log-driver local \
                --label ai.opendot.modelcat.tool-api=unix \
                --cap-drop ALL --security-opt no-new-privileges --read-only \
                --pids-limit 128 --memory 512m --memory-swap 512m --cpus 1 \
                --user "$(id -u):$(id -g)" --tmpfs /tmp:rw,nosuid,nodev,size=128m \
                --mount "type=bind,source=$workspace,target=/workspace" \
                --mount "type=bind,source=$profile,target=/profile" \
                --mount "type=bind,source=$api,target=/modelcat-api,readonly" \
                --env HOME=/profile --workdir /workspace "$image" \
                sh -c '/usr/local/bin/modelcat-program --version && exec /modelcat-api/relay tool-relay' >/dev/null
        fi
        sleep 1
        [ "$(docker inspect --format '{{.State.Running}}' "$container")" = true ] || {
            docker logs --tail 15 "$container" >&2
            fail 'The tool failed its startup check'
        }
        ;;
    stop)
        if exists; then docker stop --time 5 "$container" >/dev/null; fi
        ;;
    status)
        docker info >/dev/null 2>&1 || fail 'Docker is not ready in the workspace VM'
        state=not_created
        if exists; then state=$(docker inspect --format '{{.State.Status}}' "$container"); fi
        authorized_state=false
        autostart=false
        if authorized; then authorized_state=true; fi
        if [ "$authorized_state" = true ] && owned_marker "$root/autostart"; then autostart=true; fi
        network=$(network_mode)
        if [ "$state" = running ]; then network=$(docker inspect --format '{{.HostConfig.NetworkMode}}' "$container"); fi
        printf '{"state":"%s","authorized":%s,"autostart":%s,"internet":%s}\n' "$state" "$authorized_state" "$autostart" "$(if [ "$network" = none ]; then echo false; else echo true; fi)"
        ;;
    logs)
        if exists; then docker logs --timestamps --tail 200 "$container" 2>&1; fi
        ;;
    online|offline)
        authorized || fail '请先创建工具目录'
        if running; then fail '请先停止工具，再更改外网访问'; fi
        if [ "$action" = offline ]; then touch "$root/offline"; else rm -f "$root/offline"; fi
        ;;
    autostart)
        case "$3" in
            enable)
                authorized || fail 'Authorize the tool workspace first'
                marker="$root/autostart"
                if [ -e "$marker" ] || [ -L "$marker" ]; then
                    owned_marker "$marker" || fail 'The tool autostart marker is invalid'
                fi
                touch "$marker"
                ;;
            disable)
                if owned_directory "$tools_dir" && owned_directory "$root"; then rm -f "$root/autostart"; fi
                ;;
            *) exit 2 ;;
        esac
        ;;
    run)
        shift 2
        case "$1" in tty) flags=-it ;; stdin) flags=-i ;; *) exit 2 ;; esac
        shift
        docker exec "$flags" "$container" sh -c '
            tool=$1; shift
            key=$(cat /modelcat-api/model.key)
            case "$tool" in
                claude)
                    export ANTHROPIC_BASE_URL=http://127.0.0.1:19702 ANTHROPIC_AUTH_TOKEN="$key"
                    export CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1
                    exec /usr/local/bin/modelcat-program "$@"
                    ;;
                codex)
                    export MODELCAT_API_KEY="$key"
                    exec /usr/local/bin/modelcat-program \
                        -c '\''web_search="disabled"'\'' \
                        -c '\''model_provider="modelcat"'\'' \
                        -c '\''model_providers.modelcat={ name="ModelCat", base_url="http://127.0.0.1:19702/v1", env_key="MODELCAT_API_KEY", wire_api="responses" }'\'' "$@"
                    ;;
            esac
        ' modelcat-tool "$tool" "$@"
        ;;
    *) exit 2 ;;
esac
