#!/usr/bin/env python3
"""Derives spec/keys.json from Chromium's key tables.

    python3 codegen/derive_keys.py <dom_code_data.inc> <dom_key_data.inc> [revision]

The two files live in ui/events/keycodes/dom/ of the Chromium repository. They
are the most complete machine-readable statement of two W3C tables:

- UI Events KeyboardEvent `code` values, each with its USB HID usage
  (`WGUIPhysicalKey` is that usage: `page << 16 | id`);
- UI Events KeyboardEvent `key` values (`WGUIKey` named keys).

spec/keys.json is pinned and append-only: once a name has a number, it keeps
it. This script refuses to write a table that would change or drop one, so
re-deriving from a newer Chromium can only add keys.
"""

import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "spec", "keys.json")

# `key` values Chromium encodes as the control character they produce. They
# are named keys in the W3C table, and keep that character as their number.
CONTROL_KEYS = ("Backspace", "Tab", "Enter", "Escape", "Delete")

# Keys other toolkits name that the Chromium tables do not. Physical keys
# without a HID usage get numbers on page 0, from 0x0100 up; named keys get
# numbers in section 0x40, which the W3C table does not use.
EXTRA_PHYSICAL = [
    # "Keypad #" of the HID keyboard page; W3C `NumpadHash`.
    ("NumpadHash", 0x0700CC),
    # W3C `NumpadStar`: the star of a phone or remote keypad (not NumpadMultiply).
    ("NumpadStar", 0x000130),
] + [("F%d" % n, 0x000100 + n) for n in range(25, 36)]
EXTRA_NAMED = [("F%d" % n, 0x4000 + n) for n in range(25, 36)]


def physical(text):
    rows = re.findall(
        r'^\s*DOM_CODE\(\s*(0x[0-9a-fA-F]+)\s*,[^)]*?,\s*(NULL|"[^"]*")\s*,\s*[A-Z0-9_]+\s*\)',
        text, re.M | re.S)
    return [(name.strip('"'), int(usage, 16)) for usage, name in rows if name != "NULL"]


def named(text):
    rows = re.findall(
        r'^\s*DOM_KEY_(MAP|UNI)\(\s*"([^"]+)"\s*,\s*[A-Z0-9_]+\s*,\s*(0x[0-9a-fA-F]+)\s*\)', text, re.M)
    out = []
    for kind, name, value in rows:
        if name == "Unidentified":
            continue  # WGUIKey 0
        if name.startswith("."):
            continue  # Chromium-internal, never a `key` value on the web
        if kind == "UNI" and name not in CONTROL_KEYS:
            raise SystemExit("unexpected character-encoded key " + name)
        out.append((name, int(value, 16)))
    return out


def pinned(table):
    return [(name, int(value, 16)) for name, value in table]


def merge(kind, old, new):
    """`old` rows first and unchanged, then what `new` adds."""
    known = dict(old)
    for name, value in new:
        if name in known and known[name] != value:
            raise SystemExit("%s key %s would change from %#x to %#x" % (kind, name, known[name], value))
    names = set(known)
    values = dict((value, name) for name, value in old)
    out = list(old)
    for name, value in new:
        if name in names:
            continue
        if value in values:
            raise SystemExit("%s keys %s and %s share %#x" % (kind, name, values[value], value))
        if not re.match(r"^[A-Za-z][A-Za-z0-9]*$", name):
            raise SystemExit("%s key %r is not an identifier" % (kind, name))
        out.append((name, value))
        names.add(name)
        values[value] = name
    return out


def main():
    if len(sys.argv) < 3:
        raise SystemExit(__doc__)
    with open(sys.argv[1]) as f:
        codes = physical(f.read())
    with open(sys.argv[2]) as f:
        keys = named(f.read())
    revision = sys.argv[3] if len(sys.argv) > 3 else "unknown"

    old = {"physical": [], "named": []}
    if os.path.exists(OUT):
        with open(OUT) as f:
            old = json.load(f)
    table = {
        "_comment": [
            "Pinned key tables, derived by codegen/derive_keys.py. Append-only:",
            "a name never changes its number and is never removed.",
            "physical: W3C UI Events `code` values with their USB HID usage (page << 16 | id).",
            "named: W3C UI Events `key` values; a WGUIKey is 0x01000000 | this number.",
        ],
        "source": {
            "repository": "https://chromium.googlesource.com/chromium/src",
            "revision": revision,
            "files": [
                "ui/events/keycodes/dom/dom_code_data.inc",
                "ui/events/keycodes/dom/dom_key_data.inc",
            ],
        },
        "physical": merge("physical", pinned(old["physical"]), codes + EXTRA_PHYSICAL),
        "named": merge("named", pinned(old["named"]), keys + EXTRA_NAMED),
    }
    lines = ["{"]
    lines.append('  "_comment": ' + json.dumps(table["_comment"], indent=4).replace("\n]", "\n  ]") + ",")
    lines.append('  "source": ' + json.dumps(table["source"], indent=4).replace("\n}", "\n  }") + ",")
    for kind, width in (("physical", 6), ("named", 4)):
        lines.append('  "%s": [' % kind)
        rows = table[kind]
        for i, (name, value) in enumerate(rows):
            lines.append('    ["%s", "0x%0*X"]%s' % (name, width, value, "," if i + 1 < len(rows) else ""))
        lines.append("  ]" + ("," if kind == "physical" else ""))
    lines.append("}")
    with open(OUT, "w") as f:
        f.write("\n".join(lines) + "\n")
    print("%d physical keys, %d named keys" % (len(table["physical"]), len(table["named"])))


if __name__ == "__main__":
    main()
