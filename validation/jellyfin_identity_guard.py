"""Reject SQL replacement of entities whose opaque identities retire on DELETE."""
from __future__ import annotations
import re
from pathlib import Path

_RAW = re.compile(r'(?:br|rb|r)(#*)"')
_CHAR = re.compile(r"'(?:\\.|[^'\\])'")
_IDENTIFIER = r'(?:"[^"]+"|`[^`]+`|\[[^\]]+\]|[A-Za-z_][A-Za-z_0-9]*)'
_REPLACE = re.compile(
    rf'\b(?:INSERT\s+OR\s+REPLACE|REPLACE)\s+INTO\s+'
    rf'(?P<table>{_IDENTIFIER}(?:\s*\.\s*{_IDENTIFIER})?)', re.I,
)
_MAPPED = frozenset({'users', 'libraries', 'items', 'files'})

def rust_literals(source: str):
    """Read Rust string bodies, skipping comments; never execute source."""
    index = 0
    while index < len(source):
        if source.startswith('//', index):
            end = source.find('\n', index + 2)
            index = len(source) if end < 0 else end + 1
            continue
        if source.startswith('/*', index):
            depth = 1
            index += 2
            while index < len(source) and depth:
                if source.startswith('/*', index): depth += 1; index += 2
                elif source.startswith('*/', index): depth -= 1; index += 2
                else: index += 1
            continue
        raw = _RAW.match(source, index)
        if raw:
            endmark = '"' + raw.group(1)
            end = source.find(endmark, raw.end())
            if end < 0: return
            yield raw.end(), source[raw.end():end]
            index = end + len(endmark)
            continue
        if source[index] == "'":
            char = _CHAR.match(source, index)
            if char: index = char.end(); continue
        if source[index] != '"': index += 1; continue
        start = index + 1
        index = start
        while index < len(source):
            if source[index] == '\\': index += 2
            elif source[index] == '"': break
            else: index += 1
        body = source[start:index]
        body = re.sub(r'\\\r?\n\s*', '', body)
        body = re.sub(r'\\([nrt"\\])', lambda m: {'n':'\n','r':'\r','t':'\t','"':'"','\\':'\\'}[m[1]], body)
        yield start, body
        index += 1

def replacement_tables(sql: str) -> list[str]:
    sql = re.sub(r'/\*.*?\*/|--[^\n]*', ' ', sql, flags=re.S)
    tables = []
    for match in _REPLACE.finditer(sql):
        table = re.sub(r'[\s"`\[\]]', '', match['table']).split('.')[-1].lower()
        if table in _MAPPED: tables.append(table)
    return tables

def violations(root: Path) -> list[str]:
    results = []
    for path in sorted((root / 'crates').rglob('*')):
        if path.suffix not in {'.rs', '.sql'} or not path.is_file(): continue
        source = path.read_text(encoding='utf-8')
        literals = rust_literals(source) if path.suffix == '.rs' else [(0, source)]
        for offset, body in literals:
            for table in replacement_tables(body):
                line = source.count('\n', 0, offset) + 1
                results.append(f'{path.relative_to(root)}:{line}: replacement bypasses {table} identity retirement')
    return results
