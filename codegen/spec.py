"""Model of spec/gui.json and spec/keys.json: names, wasm32 layout, lowering.

The naming and layout rules are the ones wasmer-webgpu's generator uses for
webgpu.h (this file started as a copy of its spec.py), so the two headers read
alike. What is specific to this API is checked here, once, for every emitter:

- a function returns a status, nothing, or is one of the listed exceptions;
- an import takes scalars and pointers only;
- an event record holds fixed-width fields only, and no record outgrows
  WGUI_MAX_EVENT_SIZE.

guest/tests/abi_check.gen.c makes the C compiler confirm the layout.
"""

import json
import os
import re

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

PREFIX = "WGUI"
FN_PREFIX = "wgui"

# wasm32 C ABI: (C type, size, align, core wasm type)
PRIMS = {
    "bool": (PREFIX + "Bool", 4, 4, "i32"),
    "uint32": ("uint32_t", 4, 4, "i32"),
    "int32": ("int32_t", 4, 4, "i32"),
    "uint64": ("uint64_t", 8, 8, "i64"),
    "usize": ("size_t", 4, 4, "i32"),
    "float32": ("float", 4, 4, "f32"),
    "float64": ("double", 8, 8, "f64"),
    "key": (PREFIX + "Key", 4, 4, "i32"),
    # Only behind a pointer.
    "char": ("char", 1, 1, None),
    # Only inside event records.
    "event_string": (PREFIX + "EventString", 8, 4, None),
}
PTR = 4  # sizeof(void*) == sizeof(size_t) on wasm32
HEADER_SIZE = 32
# What an event record may be made of.
RECORD_PRIMS = ("uint32", "int32", "uint64", "float32", "float64", "key", "event_string")

RUST_KEYWORDS = {"type", "loop", "match", "ref", "in", "mod", "move", "use", "fn", "impl", "self", "override", "final",
                 "super", "default", "static", "const", "enum", "struct", "where", "while", "for", "if", "else",
                 "true", "false", "as", "break", "continue", "crate", "dyn", "extern", "let", "mut", "pub", "return",
                 "trait", "unsafe", "async", "await", "abstract", "become", "box", "do", "macro", "priv", "typeof",
                 "unsized", "virtual", "yield", "try", "gen"}


def pascal(s):
    out, upper = [], True
    for c in s:
        if upper:
            out.append(c.upper())
            upper = False
        elif c == "_":
            upper = True
        else:
            out.append(c)
    return "".join(out)


def camel(s):
    out, upper = [], False
    for c in s:
        if upper:
            out.append(c.upper())
            upper = False
        elif c == "_":
            upper = True
        else:
            out.append(c)
    return "".join(out)


def singular(s):
    if s == "entries":
        return "entry"
    return s[:-1] if s.endswith("s") else s


def snake(s):
    """Rust identifier for a spec name."""
    s = re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "_", s).lower()
    return s + "_" if s in RUST_KEYWORDS else s


def screaming(s):
    """SCREAMING_SNAKE for a spec name or a W3C CamelCase key name."""
    s = re.sub(r"(?<=[a-z])(?=[A-Z0-9])|(?<=[A-Z0-9])(?=[A-Z][a-z])", "_", s)
    return s.upper()


def parse_int(v):
    if isinstance(v, int):
        return v
    return int(v, 16) if v.lower().startswith("0x") else int(v)


class Ty(object):
    """A member or argument type reference."""

    def __init__(self, d):
        raw = d["type"]
        m = re.match(r"^array<(.*)>$", raw)
        self.raw = raw
        self.array = bool(m)
        base = m.group(1) if m else raw
        self.pointer = d.get("pointer")
        self.optional = bool(d.get("optional"))
        self.name = d.get("name")
        self.doc = d.get("doc", "")
        self.default = d.get("default")
        # An enum field that may hold values past the named ones.
        self.open = bool(d.get("open"))
        if base in PRIMS:
            self.kind, self.base = "prim", base
        elif base == "string":
            self.kind, self.base = "string", base
        elif base == "c_void":
            self.kind, self.base = "void", base
        else:
            self.kind, self.base = base.split(".", 1)
            if self.kind not in ("enum", "bitflag", "struct", "object"):
                raise SystemExit("unknown type " + raw)


class Member(object):
    def __init__(self, d):
        self.ty = Ty(d)
        self.name = d["name"]
        self.doc = d.get("doc", "")
        self.offset = None  # of the first field (the count, for arrays)

    @property
    def c_name(self):
        return camel(self.name)

    @property
    def c_count_name(self):
        return camel(singular(self.name)) + "Count"


class Struct(object):
    def __init__(self, d, kind=None):
        self.name = d["name"]
        self.kind = kind or d["type"]  # extensible | standalone | extension | header | record
        self.written = bool(d.get("written"))  # the host fills it in
        self.doc = d.get("doc", "")
        self.members = [Member(m) for m in d.get("members", [])]
        self.size = self.align = None

    @property
    def c_name(self):
        return PREFIX + pascal(self.name)

    @property
    def rust_name(self):
        return pascal(self.name)

    @property
    def init_name(self):
        return "%s_%s_INIT" % (PREFIX, self.name.upper())


class Enum(object):
    def __init__(self, name, doc, entries):
        self.name = name
        self.doc = doc
        self.entries = entries  # [(name, value, doc)]

    @classmethod
    def parse(cls, d):
        entries = []
        for i, e in enumerate(d["entries"]):
            if e is None:
                continue
            entries.append((e["name"], parse_int(e["value"]) if "value" in e else i, e.get("doc", "")))
        return cls(d["name"], d.get("doc", ""), entries)

    @property
    def c_name(self):
        return PREFIX + pascal(self.name)

    @property
    def rust_name(self):
        return pascal(self.name)

    def c_entry(self, entry):
        return self.c_name + "_" + pascal(entry)

    def value(self, entry):
        for name, value, _ in self.entries:
            if name == entry:
                return value
        raise SystemExit("%s has no entry %s" % (self.name, entry))


class Bitflag(object):
    def __init__(self, d):
        self.name = d["name"]
        self.doc = d.get("doc", "")
        self.width = d.get("width", 64)
        raw = d["entries"]
        values = {}
        bit = 0
        for e in raw:
            if "value_combination" in e:
                continue
            if "value" in e:
                values[e["name"]] = parse_int(e["value"])
            elif e["name"] == "none":
                values[e["name"]] = 0
            else:
                values[e["name"]] = 1 << bit
                bit += 1
        self.entries = []  # [(name, value, doc)]
        for e in raw:
            if "value_combination" in e:
                v = 0
                for n in e["value_combination"]:
                    v |= values[n]
            else:
                v = values[e["name"]]
            if v >> self.width:
                raise SystemExit("%s.%s does not fit %d bits" % (self.name, e["name"], self.width))
            self.entries.append((e["name"], v, e.get("doc", "")))

    @property
    def c_name(self):
        return PREFIX + pascal(self.name)

    def c_entry(self, entry):
        return self.c_name + "_" + pascal(entry)

    def value(self, entry):
        for name, value, _ in self.entries:
            if name == entry:
                return value
        raise SystemExit("%s has no entry %s" % (self.name, entry))


class Fn(object):
    """One function of the API, which is also one import."""

    def __init__(self, owner, d):
        self.owner = owner  # spec name of the object, None for globals
        self.name = d["name"]
        self.doc = d.get("doc", "")
        self.args = [Ty(a) for a in d.get("args", [])]
        self.returns = d.get("returns")  # None | status | uint32 | bool
        self.blocking = bool(d.get("blocking"))
        # Where the host lets the program's signals in (see `let_signals_in`).
        self.signals = bool(d.get("signals"))
        self.exception = d.get("stub_result") == "exception"
        self.c_name = FN_PREFIX + (pascal(owner) if owner else "") + pascal(self.name)

    @property
    def rust_name(self):
        return snake(self.name) if self.owner is None else snake(self.owner + "_" + self.name)


class Event(object):
    def __init__(self, d, classes):
        self.name = d["name"]
        self.cls = d["class"]
        self.index = d["index"]
        self.record = d.get("record")  # spec name of the record struct, None for header-only
        self.doc = d.get("doc", "")
        self.value = (classes[self.cls] << 8) | self.index


class Spec(object):
    def __init__(self):
        with open(os.path.join(ROOT, "spec", "gui.json")) as f:
            self.raw = json.load(f)
        with open(os.path.join(ROOT, "spec", "keys.json")) as f:
            keys = json.load(f)
        raw = self.raw
        self.import_module = raw["import_module"]
        self.version = (raw["version"]["major"] << 16) | raw["version"]["minor"]
        self.doc = raw["doc"]

        self.physical_keys = [(name, parse_int(value)) for name, value in keys["physical"]]
        self.named_keys = [(name, parse_int(value)) for name, value in keys["named"]]
        self.keys_source = keys["source"]

        self.event_classes = dict((c["name"], c["value"]) for c in raw["event_classes"])
        self.events = [Event(e, self.event_classes) for e in raw["events"]]

        self.enums = [Enum.parse(e) for e in raw["enums"]]
        # Two enums come from tables rather than from a list in gui.json.
        self.enums.append(Enum(
            "event_type",
            "What an event record is. The high byte is its class.",
            [(e.name, e.value, e.doc) for e in self.events]))
        self.enums.append(Enum(
            "physical_key",
            "A key by where it is on the keyboard, whatever the layout prints on it: the KeyboardEvent `code` "
            "values of the W3C UI Events specification. Each number is the key's USB HID usage, page << 16 | id "
            "(page 0x07 is the keyboard page, whose ids are also SDL's scancodes); keys without a usage are on "
            "page 0.",
            [("Unidentified", 0, "")] + [(name, value, "") for name, value in self.physical_keys]))
        self.raw_named_enums = set(["physical_key"])  # entry names are already identifiers
        self.enum_by_name = dict((e.name, e) for e in self.enums)

        self.bitflags = [Bitflag(b) for b in raw["bitflags"]]
        self.bitflag_by_name = dict((b.name, b) for b in self.bitflags)
        for cls, number in self.event_classes.items():
            if self.bitflag_by_name["event_class"].value(cls) != 1 << number:
                raise SystemExit("event class %s: its bit is not 1 << %d" % (cls, number))

        self.constants = []
        for c in raw["constants"]:
            self.constants.append((c["name"], c["type"], c["value"], c.get("doc", "")))
        self.constant_by_name = dict((c[0], c) for c in self.constants)

        self.objects = [(o["name"], o.get("doc", "")) for o in raw["objects"]]
        self.object_names = [o[0] for o in self.objects]

        self.structs = [Struct(s) for s in raw["structs"]]
        self.header = Struct(dict(raw["event_header"], name="event_header"), kind="header")
        self.records = [Struct(r, kind="record") for r in raw["event_records"]]
        self.struct_by_name = dict((s.name, s) for s in self.structs + self.records + [self.header])
        for s in self.structs + [self.header] + self.records:
            self._layout(s)
        if self.header.size != HEADER_SIZE:
            raise SystemExit("the event header is %d bytes, not %d" % (self.header.size, HEADER_SIZE))
        self.max_event_size = parse_int(self.constant_by_name["max_event_size"][2])
        for r in self.records:
            self._check_record(r)
        for e in self.events:
            if e.record is not None and e.record not in self.struct_by_name:
                raise SystemExit("event %s names unknown record %s" % (e.name, e.record))
        values = [e.value for e in self.events]
        if len(set(values)) != len(values):
            raise SystemExit("two events share a type value")

        self.functions = [Fn(None, f) for f in raw["functions"]]
        for o in raw["objects"]:
            for m in o.get("methods", []):
                self.functions.append(Fn(o["name"], m))
        for fn in self.functions:
            self._check_fn(fn)
        names = [f.c_name for f in self.functions]
        if len(set(names)) != len(names):
            raise SystemExit("two functions share a name")

    # -- naming ---------------------------------------------------------------

    def enum_entry_c(self, enum, entry):
        e = self.enum_by_name[enum]
        if enum in self.raw_named_enums:
            return e.c_name + "_" + entry
        return e.c_entry(entry)

    def enum_entry_rust(self, enum, entry):
        if enum in self.raw_named_enums:
            return entry
        return pascal(entry)

    def c_base(self, ty):
        if ty.kind == "prim":
            return PRIMS[ty.base][0]
        if ty.kind == "string":
            return PREFIX + "StringView"
        if ty.kind == "void":
            return "void"
        return PREFIX + pascal(ty.base)

    def c_type(self, ty):
        t = self.c_base(ty)
        if ty.pointer == "immutable":
            return "const " + t + "*"
        if ty.pointer == "mutable":
            return t + "*"
        return t

    # -- layout ---------------------------------------------------------------

    def size_align(self, ty):
        """Size and alignment of one non-array member of this type."""
        if ty.pointer:
            return PTR, PTR
        if ty.kind == "prim":
            if ty.base == "char":
                raise SystemExit("char is only used behind a pointer")
            return PRIMS[ty.base][1], PRIMS[ty.base][2]
        if ty.kind == "string":
            return 8, 4
        if ty.kind in ("enum", "object"):
            return 4, 4
        if ty.kind == "bitflag":
            w = self.bitflag_by_name[ty.base].width // 8
            return w, w
        if ty.kind == "void":
            raise SystemExit("void is only used behind a pointer")
        if ty.kind == "struct":
            s = self.struct_by_name[ty.base]
            if s.size is None:
                self._layout(s)
            return s.size, s.align
        raise SystemExit("no layout for " + ty.raw)

    def _layout(self, s):
        if s.size is not None:
            return
        off, align = 0, 1
        if s.kind == "extensible":
            off, align = PTR, PTR  # nextInChain
        elif s.kind == "extension":
            off, align = 8, 4  # WGUIChainedStruct
        elif s.kind == "record":
            off, align = HEADER_SIZE, 8
        for m in s.members:
            if m.ty.array:
                sz, al = 2 * PTR, PTR
            else:
                sz, al = self.size_align(m.ty)
            off = (off + al - 1) // al * al
            m.offset = off
            off += sz
            align = max(align, al)
        s.align = align
        s.size = (off + align - 1) // align * align

    def _check_record(self, r):
        expected = HEADER_SIZE
        for m in r.members:
            t = m.ty
            ok = not t.array and not t.pointer and (
                (t.kind == "prim" and t.base in RECORD_PRIMS)
                or t.kind in ("enum", "bitflag", "object")
                or (t.kind == "struct" and self.struct_by_name[t.base].kind == "standalone"))
            if not ok:
                raise SystemExit("record %s.%s: %s is not a fixed-width field" % (r.name, m.name, t.raw))
            if m.offset != expected:
                # Padding the C compiler would insert silently is spelled out as a reserved field instead.
                raise SystemExit("record %s.%s: %d bytes of implicit padding before it" % (
                    r.name, m.name, m.offset - expected))
            expected = m.offset + self.size_align(t)[0]
        if r.size != expected or r.size % 8:
            raise SystemExit("record %s: its fixed part (%d bytes) must end on a multiple of 8" % (r.name, expected))
        if r.size > self.max_event_size:
            raise SystemExit("record %s is larger than WGUI_MAX_EVENT_SIZE" % r.name)

    # -- functions ------------------------------------------------------------

    def _check_fn(self, fn):
        if fn.returns not in (None, "status", "uint32", "bool"):
            raise SystemExit("%s returns %s: results are a status or nothing" % (fn.c_name, fn.returns))
        if fn.returns in ("uint32", "bool") and not fn.exception:
            raise SystemExit("%s returns a value: a stubbed import would answer 0, so 0 must be marked as a safe "
                             "answer with \"stub_result\": \"exception\"" % fn.c_name)
        for a in fn.args:
            if a.kind == "struct" and not a.pointer and not a.array:
                raise SystemExit("%s(%s): structs are passed by pointer" % (fn.c_name, a.name))
            if a.kind == "prim" and a.base in ("char", "event_string") and not a.pointer:
                raise SystemExit("%s(%s): %s is not an argument type" % (fn.c_name, a.name, a.base))
            if a.kind == "void" and not a.pointer:
                raise SystemExit("%s(%s): void needs a pointer" % (fn.c_name, a.name))
            if a.array and a.pointer != "immutable":
                raise SystemExit("%s(%s): array arguments are inputs" % (fn.c_name, a.name))

    def c_return(self, fn):
        return {None: "void", "status": PREFIX + "Status", "uint32": "uint32_t", "bool": PREFIX + "Bool"}[fn.returns]

    def c_params(self, fn):
        """[(c type, c name)] exactly as declared: arrays are count + pointer, strings are pointer + length."""
        out = []
        if fn.owner is not None:
            out.append((PREFIX + pascal(fn.owner), camel(fn.owner)))
        for a in fn.args:
            if a.array:
                out.append(("size_t", camel(singular(a.name)) + "Count"))
                out.append(("const " + self.c_base(a) + "*", camel(a.name)))
            elif a.kind == "string":
                out.append(("const char*", camel(a.name)))
                out.append(("size_t", camel(a.name) + "Length"))
            else:
                out.append((self.c_type(a), camel(a.name)))
        return out

    def c_decl(self, fn, name=None):
        params = ", ".join("%s %s" % p for p in self.c_params(fn)) or "void"
        return "%s %s(%s)" % (self.c_return(fn), name or fn.c_name, params)

    def lower_params(self, fn):
        """[(name, core wasm type)] of fn's import."""
        out = []
        if fn.owner is not None:
            out.append((snake(fn.owner), "i32"))
        for a in fn.args:
            n = snake(a.name)
            if a.array:
                out.append((snake(singular(a.name)) + "_count", "i32"))
                out.append((n, "i32"))
            elif a.kind == "string":
                out.append((n, "i32"))
                out.append((n + "_length", "i32"))
            elif a.pointer or a.kind in ("object", "enum"):
                out.append((n, "i32"))
            elif a.kind == "bitflag":
                out.append((n, "i64" if self.bitflag_by_name[a.base].width == 64 else "i32"))
            else:
                out.append((n, PRIMS[a.base][3]))
        return out

    def lower_result(self, fn):
        return None if fn.returns is None else "i32"
