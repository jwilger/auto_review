#!/usr/bin/env python3
import argparse
import json


def inline_reply_payload(body: str, path: str, position: int) -> dict[str, object]:
    return {
        "body": body,
        "path": path,
        "new_position": position,
        "old_position": 0,
    }


def _main() -> int:
    parser = argparse.ArgumentParser(description="Forgejo review helper")
    parser.add_argument("command", choices=["inline-reply-payload"])
    parser.add_argument("--body", required=True)
    parser.add_argument("--path", required=True)
    parser.add_argument("--position", required=True, type=int)
    args = parser.parse_args()
    print(json.dumps(inline_reply_payload(args.body, args.path, args.position), indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(_main())
