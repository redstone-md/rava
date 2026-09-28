#!/usr/bin/env python3
"""Move top-level items out of a Rust file into a submodule, keeping call sites unchanged.

The crate has two files holding 46% of its lines (jit.rs 27k, lib.rs 24k) against a project rule
of strict module separation. Splitting those by hand is twenty repetitions of the same four
mistakes, so this does the mechanical part and leaves the judgement -- which items form a domain
-- to the caller.

    python bench/extract_mod.py SRC NEWMOD "item1,item2,..." "doc line"

* SRC is the module file (e.g. crates/rava-interp/src/jit/mod.rs)
* NEWMOD is created beside it and declared + `pub(crate) use`d from SRC, so every existing
  `crate::jit::thing` path keeps resolving and no caller is touched.
* An item's span runs from its leading doc comments/attributes to the next top-level item, so
  documentation travels with what it documents -- except a trailing doc block, which belongs to
  the NEXT item and is left behind.
* Private `fn`s are raised to `pub(crate)`: a glob re-export cannot carry private items, and the
  alternative (hunting each call site) is what this script exists to avoid.
"""
import io
import re
import sys

TOP = re.compile(
    r'^(?:pub(?:\([^)]*\))? )?'
    r'(?:unsafe )?(?:extern "C" )?'
    r'(?:fn|struct|enum|impl|mod|static|const|trait|type|macro_rules)\b'
)


def spans_of(lines, names):
    tops = [i for i, l in enumerate(lines) if TOP.match(l)]
    tops.append(len(lines))
    out = []
    for idx, i in enumerate(tops[:-1]):
        for n in names:
            pat = (r'^(?:pub(?:\([^)]*\))? )?(?:unsafe )?(?:extern "C" )?'
                   r'(?:fn|struct|enum|mod|static|const|type) ' + re.escape(n) + r'\b')
            # An inherent or trait impl belongs to its type and has to travel with it, otherwise
            # the type moves and its methods stay behind referring to a type they can no longer see.
            impl = (r'^impl(?:<[^>]*>)? (?:[\w:<>\' ]+ for )?' + re.escape(n) + r'\b')
            if re.match(pat, lines[i]) or re.match(impl, lines[i]):
                s = i
                while s > 0 and (lines[s - 1].lstrip().startswith(('///', '#[', '//!'))
                                 or lines[s - 1].lstrip().startswith('//')):
                    s -= 1
                e = tops[idx + 1]
                # A trailing doc block or attribute documents the NEXT item; leave it behind.
                # Blank lines can sit between an attribute and the doc block it precedes, so skip
                # those first -- stopping at one leaves the attribute stranded at end of file.
                t = e
                while t > s and lines[t - 1].strip() == '':
                    t -= 1
                while t > s and lines[t - 1].lstrip().startswith(('///', '#[')):
                    e, t = t - 1, t - 1
                    while t > s and lines[t - 1].strip() == '':
                        t -= 1
                out.append((s, e, n))
                break
    return sorted(out)


def main():
    src, newmod, items, doc = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
    names = [x.strip() for x in items.split(',') if x.strip()]
    lines = io.open(src, encoding='utf-8').read().replace('﻿', '').split('\n')

    found = spans_of(lines, names)
    got = {n for _, _, n in found}
    for n in names:
        if n not in got:
            print('  MISS', n)

    merged = []
    for a, b, n in found:
        if merged and a <= merged[-1][1]:
            merged[-1] = (merged[-1][0], max(b, merged[-1][1]))
        else:
            merged.append((a, b))

    moved, keep, prev = [], [], 0
    for a, b in merged:
        moved.extend(lines[a:b])
        keep.extend(lines[prev:a])
        prev = b
    keep.extend(lines[prev:])

    body = '\n'.join(moved).rstrip()
    body = re.sub(r'^fn ', 'pub(crate) fn ', body, flags=re.M)
    body = re.sub(r'^static ', 'pub(crate) static ', body, flags=re.M)
    # Same reason as `fn`: a glob re-export cannot carry a private const.
    body = re.sub(r'^const ', 'pub(crate) const ', body, flags=re.M)
    # Types too: a private struct/enum moved out of the module its users live in is invisible to
    # them, and the glob re-export cannot carry it back.
    for kw in ('struct', 'enum', 'type', 'trait'):
        body = re.sub(r'^%s ' % kw, 'pub(crate) %s ' % kw, body, flags=re.M)
    # An `extern "C" fn` is referenced by ADDRESS from the symbol table the JIT registers, so it
    # needs the same visibility bump as a plain fn.
    body = re.sub(r'^extern "C" fn ', 'pub(crate) extern "C" fn ', body, flags=re.M)

    name = newmod.rsplit('/', 1)[-1].rsplit('.', 1)[0]
    header = '//! ' + doc + '\n//!\n//! Re-exported from the parent module, so no call site outside it changed.\n\n'
    io.open(newmod, 'w', encoding='utf-8').write(header + body + '\n')

    i = 0
    while i < len(keep) and (keep[i].startswith('//!') or keep[i].strip() == ''):
        i += 1
    keep[i:i] = ['mod %s;' % name, 'pub(crate) use %s::*;' % name, '']
    io.open(src, 'w', encoding='utf-8').write('\n'.join(keep))
    print('%s: %d lines moved, %s now %d' % (newmod, len(moved), src, len(keep)))


main()
