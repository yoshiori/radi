#!/usr/bin/env python3
"""Normalize a LISTEN GraphQL introspection dump for stable diffs.

Introspection responses come back with types/fields in an arbitrary order,
so a raw `curl | jq` snapshot churns on every fetch. This script sorts every
order-irrelevant array (types, fields, args, enum values, directives, ...)
so the committed JSON only changes when the schema actually changes.
"""

import json
import sys


def sort_type(t: dict) -> None:
    for key in ("fields", "inputFields"):
        if t.get(key):
            t[key].sort(key=lambda x: x["name"])
            for item in t[key]:
                if item.get("args"):
                    item["args"].sort(key=lambda a: a["name"])
    if t.get("enumValues"):
        t["enumValues"].sort(key=lambda f: f["name"])
    for key in ("interfaces", "possibleTypes"):
        if t.get(key):
            t[key].sort(key=lambda r: r.get("name") or "")


def normalize(doc: dict) -> dict:
    schema = doc["data"]["__schema"]
    schema["types"].sort(key=lambda t: t.get("name") or "")
    schema["directives"].sort(key=lambda d: d.get("name") or "")
    for d in schema["directives"]:
        if d.get("args"):
            d["args"].sort(key=lambda a: a["name"])
        if d.get("locations"):
            d["locations"].sort()
    for t in schema["types"]:
        sort_type(t)
    return doc


def main() -> None:
    doc = json.load(sys.stdin)
    normalize(doc)
    json.dump(doc, sys.stdout, indent=4, sort_keys=True)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
