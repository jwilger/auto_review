---
name: run-ar-gateway
description: Build ar-gateway from main and run it with an auto-rebuild watcher in two visible zellij tabs in the current session. Use this skill whenever the user asks to "launch ar-gateway", "run the gateway", "start ar-gateway", "watch main and restart ar-gateway", "build and run on main", or any variant of running this project's gateway with main-tracking auto-rebuild. Also use it to recover the setup after a reboot, after `git switch`, after killing the tabs by accident, or any time the user wants the visible-tab workflow re-established. Prefer this over ad-hoc commands; it captures the working incantations (direnv, zellij action sequences, Ctrl-C-then-restart) that are easy to get wrong.
---

# Run ar-gateway with auto-rebuild watcher

## What this does

Sets up two visible tabs in the current zellij session:

1. **`ar-gateway` tab** — runs `./result/bin/ar-gateway` with the project's environment loaded via direnv. Listens on `:8080`.
2. **`ar-watch` tab** — runs `.claude/skills/run-ar-gateway/assets/watch-and-restart.sh`, which polls `origin/main` every 30s, pulls + `nix build`s on update, then drives the `ar-gateway` tab (Ctrl-C → wait for port → relaunch) so the user can see both the rebuild log and the new gateway output.

The watcher script is bundled with the skill and runs in place — no copy/install step.

## When NOT to use

- Don't use if the user wants ar-gateway running headless / as a daemon — they explicitly want it in a visible zellij tab.
- Don't use outside a zellij session — the `zellij action` calls below require the assistant's shell to be inside the same zellij session as the tabs being driven.

## Preconditions to verify before doing anything

Run these in parallel and bail early with an explanation if any fail:

```sh
zellij list-sessions | grep -E '\(current\)'                  # must be inside a zellij session
which direnv && direnv status | head -5                       # direnv loaded for this dir
test -f flake.nix && test -f .envrc                           # at project root
git -C /home/jwilger/projects/auto_review rev-parse HEAD      # repo healthy
```

If `.envrc` isn't allowed, run `direnv allow` once. Don't proceed if the user is on a non-`main` branch unless they've said so — ask.

## Procedure

### Step 1 — Check for stale state

A previous run may have left tabs or processes around. Reuse them when possible; clean up when needed.

```sh
zellij action query-tab-names                                 # do tabs already exist?
ss -ltn | grep ':8080 '                                       # is port already bound?
pgrep -af '/result/bin/ar-gateway'                            # any gateway running?
pgrep -af 'watch-and-restart'                                 # watcher running?
```

Decision tree:
- Both tabs exist + gateway healthy on `:8080` + watcher running → tell the user it's already up; do nothing.
- Tabs missing but a gateway/watcher is running outside zellij → kill them (`kill <pid>`), then proceed fresh. Don't `kill -9` unless `kill` fails after a few seconds.
- Tabs exist but stale → close them via `zellij action close-tab` (focus the tab first with `go-to-tab-name`) and proceed fresh.

### Step 2 — Sync and build

```sh
git fetch origin main
# If HEAD != origin/main, fast-forward:
git pull --ff-only origin main
nix build .#ar-gateway
```

The build is usually a Nix cache hit and finishes near-instantly. If it actually compiles, that takes a while — let it run, don't time it out.

### Step 3 — Make the watcher script executable

The watcher lives in this skill's `assets/` directory and is invoked directly from there — no copy step needed. Just make sure the executable bit is set (it should be, but a fresh clone or skill update can drop it):

```sh
chmod +x .claude/skills/run-ar-gateway/assets/watch-and-restart.sh
```

To change watcher behavior, edit `.claude/skills/run-ar-gateway/assets/watch-and-restart.sh` directly. It's the single source of truth.

### Step 4 — Launch the gateway tab

```sh
zellij action new-tab --cwd "$PWD" --name ar-gateway
sleep 1
zellij action write-chars 'direnv exec . ./result/bin/ar-gateway'
zellij action write 13          # Enter — DO NOT use \n; the MCP/CLI doesn't translate it
```

Why `direnv exec .` instead of relying on shell direnv hook: a fresh zellij tab may not have direnv hooked into its shell yet, and we want the env loaded synchronously before the binary starts.

Verify within ~3s:

```sh
sleep 3
ss -ltn | grep -q ':8080 ' && curl -fsS -m 3 http://localhost:8080/healthz
```

If `:8080` is not bound, **don't** retry blindly — read the tab's pane to see the error (`zellij action dump-screen` after focusing the tab, or use the zellij MCP `zellij_dump_screen` tool). Common causes: port already in use, missing `LLM_*` env (check `.envrc`), `result/bin/ar-gateway` symlink stale (rebuild).

### Step 5 — Launch the watcher tab

```sh
zellij action new-tab --cwd "$PWD" --name ar-watch
sleep 1
zellij action write-chars './.claude/skills/run-ar-gateway/assets/watch-and-restart.sh'
zellij action write 13
sleep 1
zellij action go-to-tab 1       # return focus to the user's working tab
```

### Step 6 — Final report

Report to the user, in 3-5 lines:
- Built revision (`git rev-parse --short HEAD`)
- Gateway PID + healthz status
- Watcher PID
- Tab names created/reused

Don't dump full logs; the user can switch to the tabs themselves.

## Behavioral notes

- **Newlines in `zellij action`**: pass Enter as `zellij action write 13` (the decimal byte for CR). `write-chars '\n'` does NOT work — it sends the literal two characters. This bit us before; trust the pattern.
- **Tab focus is global state**: every `write-chars` / `write` goes to the currently focused pane. After driving a tab, switch back to tab 1 so you don't accidentally type into the gateway pane on the next tool call.
- **Don't add a sleep loop in user-facing prompts**: if you must wait for the port to come up, use a bounded `for _ in $(seq 1 30); do ... break ... done` loop, never an unbounded `while true`.
- **The watcher restarts the gateway by sending Ctrl-C**: that's `zellij action write 3`. Give it ~2s to release the port before sending the new command.
- **Don't run ar-gateway via `nix run` or `result/bin/ar-gateway --help`**: the binary has no `--help` flag — invoking it always starts the server. If you want to test invocation, you must be ready to actually run the server (and free port 8080 first).

## Recovery cheatsheet

| Symptom | Fix |
|---|---|
| Port 8080 in use, no zellij tab | `pkill -f ar-gateway`, then re-run skill |
| Watcher tab shows "git fetch failed" repeatedly | Check network / Forgejo creds in `.envrc`; the watcher will recover on next interval |
| Watcher rebuilt but gateway didn't restart | `zellij action go-to-tab-name ar-gateway`, look at the pane — likely the Ctrl-C didn't reach a shell prompt because gateway hung. Send Ctrl-C twice |
| Gateway tab shows `Address already in use` | A previous gateway process is still bound. `pkill -f /result/bin/ar-gateway`, then in the gateway tab, press Up + Enter to relaunch |
