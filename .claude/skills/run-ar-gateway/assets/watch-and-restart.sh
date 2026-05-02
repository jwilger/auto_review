#!/usr/bin/env bash
set -uo pipefail

# Anchor to the repo root so the script works regardless of where it's
# invoked from. `git rev-parse --show-toplevel` is more robust than
# computing a relative path from $BASH_SOURCE because the script lives
# in `.claude/skills/run-ar-gateway/assets/` but operates on the repo root.
cd "$(git rev-parse --show-toplevel)"

GATEWAY_TAB="ar-gateway"
WATCH_TAB="ar-watch"
INTERVAL="${INTERVAL:-30}"

log() { printf '[%s] %s\n' "$(date '+%H:%M:%S')" "$*"; }

restart_gateway() {
    log "switching to $GATEWAY_TAB tab"
    zellij action go-to-tab-name "$GATEWAY_TAB" >/dev/null 2>&1 || {
        log "ERROR: tab $GATEWAY_TAB not found"
        return 1
    }
    sleep 0.3
    log "sending Ctrl-C"
    zellij action write 3
    # wait for port to free
    for _ in $(seq 1 30); do
        ss -ltn 2>/dev/null | grep -q ':8080 ' || break
        sleep 0.5
    done
    log "launching ar-gateway"
    zellij action write-chars 'direnv exec . ./result/bin/ar-gateway'
    zellij action write 13
    sleep 0.3
    zellij action go-to-tab-name "$WATCH_TAB" >/dev/null 2>&1 || true
}

# Track origin/main's tip across iterations. Comparing to HEAD is wrong
# when checked out on a feature branch — branch tip != origin/main even
# when origin/main hasn't moved, which causes a permanent "needs rebuild"
# state and a restart loop. The right question is "did origin/main move
# since the last poll?", which requires remembering its previous value.
LAST_REMOTE=""

log "watcher started; polling origin/main every ${INTERVAL}s"

while true; do
    if ! git fetch --quiet origin main 2>/dev/null; then
        log "git fetch failed; will retry"
        sleep "$INTERVAL"
        continue
    fi
    REMOTE=$(git rev-parse origin/main)

    # First iteration: record the baseline without rebuilding. The user
    # already built before launching the watcher, so we treat the current
    # origin/main as "what we're running" and only act on subsequent moves.
    if [[ -z "$LAST_REMOTE" ]]; then
        LAST_REMOTE=$REMOTE
        log "tracking origin/main at ${REMOTE:0:7}"
        sleep "$INTERVAL"
        continue
    fi

    if [[ "$LAST_REMOTE" != "$REMOTE" ]]; then
        log "origin/main moved: ${LAST_REMOTE:0:7} -> ${REMOTE:0:7}"
        LAST_REMOTE=$REMOTE
        BRANCH=$(git rev-parse --abbrev-ref HEAD)
        if [[ "$BRANCH" != "main" ]]; then
            log "checked out on '$BRANCH', not main; skipping rebuild"
            log "(merge or switch to main and the next move will trigger a rebuild)"
            sleep "$INTERVAL"
            continue
        fi
        if ! git pull --ff-only origin main; then
            log "git pull --ff-only failed (diverged?); skipping"
            sleep "$INTERVAL"
            continue
        fi
        log "building..."
        if nix build .#ar-gateway; then
            log "build OK; restarting gateway"
            restart_gateway || log "restart had issues"
        else
            log "build FAILED; gateway left running on old binary"
        fi
    fi
    sleep "$INTERVAL"
done
