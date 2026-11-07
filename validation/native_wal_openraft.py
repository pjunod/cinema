"""Bind an unprinted Make recipe to its authenticated native WAL execution."""
import re
from validation import unit_openraft_package as package
from validation.python_unit_receipts import require


def interval(record, raw):
    from validation import qualification_unit_continuation as commands
    require(record['origin']['kind'] == 'authenticated-ci'
            and record['origin']['execution']['job_status'] == 'success'
            and record['exit_code'] == 0, 'Unprinted Openraft recipe needs successful native WAL authority')
    package.source_descriptor(record['source_commit'])
    recipe = commands.historical_recipe(record, 'wal')
    external = [(name, command) for name, command in commands.historical_canonical(record['source_commit']).items()
                if name.startswith('wal:') and commands.rust_identity(command)['cargo'].get('--manifest-path') == package.MANIFEST_VARIABLE]
    require(len(external) == 1, 'Ambiguous external WAL recipe')
    name, canonical = external[0]
    matches = [i for i, value in enumerate(recipe) if value == canonical]
    require(len(matches) == 1 and 0 < matches[0] < len(recipe) - 1,
            'Unprinted Openraft recipe lacks unique neighboring commands')
    ordinal = matches[0]
    lines = raw.splitlines()
    headers = [i for i, line in enumerate(lines) if re.fullmatch(
        r'\s*Running unittests src/lib.rs \(target/openraft-regression/debug/deps/openraft-[0-9a-f]{16}\)', line)]
    require(len(headers) == 1,
            'Native Openraft target is absent or ambiguous')
    start = headers[0]
    anchors = []
    for i, line in enumerate(lines):
        if i and lines[i - 1].endswith('\\'):
            continue
        if not re.match(r'^(?:(?:[A-Z_]+=[^ ]+|scripts/require-test-count) +)*(?:cargo|/[^ ]+/cargo) (?:test|run|clippy|check)(?: |$)', line):
            continue
        command, _ = commands.logged_command(lines, i)
        anchors.append((i, command))
    prior = [(i, command) for i, command in anchors if i < start]
    later = [(i, command) for i, command in anchors if i > start]
    require(prior and later
            and commands.display_tokens(prior[-1][1], 'wal') == commands.display_tokens(recipe[ordinal - 1], 'wal')
            and commands.display_tokens(later[0][1], 'wal') == commands.display_tokens(recipe[ordinal + 1], 'wal'),
            'Native Openraft execution is outside its exact Make recipe neighbors')
    compile_rows = [line for line in lines[prior[-1][0]:start] if re.fullmatch(
        r'\s*Compiling openraft v0\.9\.25 \(/[^\n]+/registry/src/index\.crates\.io-[0-9a-f]{16}/openraft-0\.9\.25\)', line)]
    require(len(compile_rows) == 1, 'Native locked Openraft package path unavailable')
    body = '\n'.join(lines[start:later[0][0]])
    require(re.findall(r'^test (.+?) \.\.\. (ok|FAILED|ignored)(?:, .*|)$', body, re.M)
            == [(package.SELECTOR, 'ok')]
            and len(re.findall(r'^test result:', body, re.M)) == 1
            and re.search(r'^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; 196 filtered out; finished in [0-9.]+s$', body, re.M),
            'Native Openraft complete outcome differs')
    return {'body': body, 'first_line': start + 1, 'last_line': later[0][0],
            'previous_line': prior[-1][0] + 1, 'historical_name': name}


def execution(record, item, raw):
    from validation import qualification_unit_continuation as commands
    span = interval(record, raw)
    require(record['origin']['command'] == commands.COMMANDS['wal']
            and record['origin']['execution']['first_line'] == span['first_line']
            and record['origin']['execution']['historical_name'] == span['historical_name']
            and commands.rust_identity(item['invocation'])['cargo']['--manifest-path'] == package.MANIFEST_VARIABLE,
            'Wrong unprinted external package execution scope')
    return span['body']
