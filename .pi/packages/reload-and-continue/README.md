# Reload and continue

After Pi saves a project skill, extension, package, or MCP config, that resource
isn't available to the current runtime yet. This package reloads the normal
interactive runtime and starts a new turn without asking the user to send
another message. It is developed and tested against **Pi 1.6.0**.

## Try it

In a trusted checkout, start interactive Pi 1.6.0, or use `/reload` once if
Pi was already open when you added the package. Then ask Pi:

> Create a project skill `reload-demo` under `.pi/skills/reload-demo/` that
> prints `reload works`. After saving it, call `reload_and_continue` by itself,
> with the continuation: “Confirm `.pi/skills/reload-demo/SKILL.md` is registered
> as a skill, then invoke it and report the result.”

You should see the reload, then a new turn with the verification instruction.
There should be no need to send another message. Remove the demo skill afterward.

## Why this mechanism

Pi 1.6.0 ships an example, `examples/extensions/reload-runtime.ts`, that uses
`terminate: true`, `agent_settled`, and a command calling `ctx.reload()`.
This package follows that example. Reload invalidates the old extension API;
a one-shot in-memory handoff binds to the fresh API during `session_start` and
submits the continuation only after the reload call returns. If the fresh
extension doesn't start, no continuation is sent. The first valid call in a
run wins; aborting that run drops the request. Pending state isn't saved or
reconstructed from history. Once sent, the continuation is a normal user
message and can appear in session history and model requests.

Call the tool on its own as the final action. Pi skips the next automatic model
request only when every tool result in that batch requests termination; mixing
it with other tools can delay reload until the run settles. It does not run in
print, JSON, or RPC mode. The package adds no npm dependencies, credentials,
or direct network access.

## Verify and revisit

From the repository root, run the regression against the pinned Pi release:

```sh
pi --version  # 1.6.0
python3 .pi/packages/reload-and-continue/tests/test_reload_and_continue.py -v
```

The test uses a real TUI in a temporary trusted project with an offline scripted
provider and also exercises RPC rejection. It checks that a resource created
mid-run is visible in the one automatic follow-up, that the first request wins,
and that abort doesn't reload or replay the handoff. Re-run it on Pi upgrades;
retire the bridge if Pi provides a native reload-and-follow-up API, or retire the
package if manual reload no longer causes a user interruption.
