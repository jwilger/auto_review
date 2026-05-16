---
description: Run focused or full repository verification through just recipes.
agent: build
---

Verify the current work: $ARGUMENTS

Prefer focused checks first, then broader gates as needed:

```sh
just fmt
just clippy
just test
just deny
just build
just ci
```

Use `just ci` for the aggregate routine gate. State any skipped gate and why.
