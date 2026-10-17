"""Read complete compiled JUnit4 discovery metadata without executing tests.

Metadata determines source-cohort membership, never historical runtime outcomes.
Unsupported runners, inheritance, skips and class layouts refuse adoption.
"""
from __future__ import annotations

class InventoryError(ValueError):
    pass

def require(condition, message):
    if not condition:
        raise InventoryError(message)
NON_TEST_ROOTS = frozenset({'java/lang/Object', 'java/lang/Enum', 'java/lang/RuntimeException', 'java/lang/Exception', 'java/lang/Throwable', 'java/io/IOException', 'kotlin/coroutines/jvm/internal/ContinuationImpl', 'kotlin/coroutines/jvm/internal/SuspendLambda', 'kotlin/jvm/internal/AdaptedFunctionReference', 'kotlin/jvm/internal/FunctionReferenceImpl', 'okhttp3/ResponseBody'})

def class_header(raw):
    """Bounded classfile header and constant-pool scan, without loading classes."""
    import struct
    require(isinstance(raw, bytes) and 10 < len(raw) <= 8 * 1024 * 1024, 'Classfile size outside bound')
    offset = 0

    def take(size):
        nonlocal offset
        require(0 <= size <= len(raw) - offset, 'Truncated classfile')
        part = raw[offset:offset + size]
        offset += size
        return part

    def u1():
        return take(1)[0]

    def u2():
        return struct.unpack('>H', take(2))[0]
    require(take(4) == b'\xca\xfe\xba\xbe', 'Not a JVM classfile')
    u2()
    u2()
    count = u2()
    pool = {}
    at = 1
    while at < count:
        tag = u1()
        if tag == 1:
            pool[at] = ('utf8', modified_utf8(take(u2())))
        elif tag == 7:
            pool[at] = ('class', u2())
        elif tag in {3, 4, 9, 10, 11, 12, 17, 18}:
            take(4)
        elif tag in {5, 6}:
            take(8)
            at += 1
        elif tag in {8, 16, 19, 20}:
            take(2)
        elif tag == 15:
            take(3)
        else:
            raise InventoryError('Unsupported classfile constant-pool tag')
        at += 1
    require(at == count, 'Invalid two-slot constant pool boundary')

    def name(index):
        require(index in pool and pool[index][0] == 'class', 'Missing class pool identity')
        entry = pool[index][1]
        require(entry in pool and pool[entry][0] == 'utf8', 'Missing class name')
        return pool[entry][1]
    flags = u2()
    identity = name(u2())
    parent = name(u2())
    interfaces = [name(u2()) for _ in range(u2())]
    descriptors = sorted({value for kind, value in pool.values() if kind == 'utf8' and value.startswith('Lorg/junit/')})
    return {'name': identity, 'super': parent, 'interfaces': interfaces, 'flags': flags, 'junit_descriptors': descriptors}

def excluded_generated_classes(headers, selected):
    """Every omitted class is a closed non-test hierarchy, never an arbitrary $."""
    require(isinstance(headers, list) and headers, 'Missing complete compiled class roster')
    by_name = {row['name']: row for row in headers}
    require(len(by_name) == len(headers), 'Repeated class header identity')
    require(not set(by_name).intersection(NON_TEST_ROOTS), 'Compiled roster shadows a reserved external non-test root')
    require(set(selected) <= set(by_name), 'Selected metadata class outside complete roster')

    def non_test(name, seen):
        require(name not in seen, 'Cyclic compiled hierarchy')
        if name in NON_TEST_ROOTS:
            return
        require(name in by_name, 'Unreviewed external superclass of generated helper')
        row = by_name[name]
        require(not row['junit_descriptors'], 'Excluded helper inherits a JUnit-bearing class')
        non_test(row['super'], seen | {name})
        for interface in row['interfaces']:
            require(not interface.startswith(('org/junit/', 'junit/')), 'Excluded helper implements JUnit discovery')
    excluded = []
    for name, row in by_name.items():
        if name in selected:
            continue
        require('$' in name and (not row['junit_descriptors']), 'Excluded class is not a proven generated non-test helper')
        require(name.split('$', 1)[0] in by_name, 'Generated helper has no compiled enclosing owner')
        non_test(name, set())
        excluded.append(name)
    return sorted(excluded)

def native_class_details(raw):
    """Read actual JUnit annotations, method descriptors and class SourceFile."""
    import struct
    offset = 0

    def take(size):
        nonlocal offset
        require(0 <= size <= len(raw) - offset, 'Truncated class details')
        v = raw[offset:offset + size]
        offset += size
        return v

    def u1():
        return take(1)[0]

    def u2():
        return struct.unpack('>H', take(2))[0]

    def u4():
        return struct.unpack('>I', take(4))[0]
    require(10 < len(raw) <= 8 * 1024 * 1024 and take(4) == b'\xca\xfe\xba\xbe', 'Invalid class details')
    u2()
    u2()
    count = u2()
    pool = {}
    at = 1
    while at < count:
        tag = u1()
        if tag == 1:
            pool[at] = modified_utf8(take(u2()))
        elif tag == 7:
            take(2)
        elif tag in {3, 4, 9, 10, 11, 12, 17, 18}:
            take(4)
        elif tag in {5, 6}:
            take(8)
            at += 1
        elif tag in {8, 16, 19, 20}:
            take(2)
        elif tag == 15:
            take(3)
        else:
            raise InventoryError('Unsupported class constant')
        at += 1
    require(at == count, 'Invalid two-slot constant pool boundary')

    def utf(index):
        require(index in pool, 'Missing class UTF8 metadata')
        return pool[index]

    def element(depth):
        require(depth < 16, 'Annotation nesting outside bound')
        tag = chr(u1())
        if tag in 'BCDFIJSZsc':
            u2()
        elif tag == 'e':
            u2()
            u2()
        elif tag == '@':
            annotation(depth + 1)
        elif tag == '[':
            for _ in range(u2()):
                element(depth + 1)
        else:
            raise InventoryError('Unsupported annotation element')

    def annotation(depth):
        name = utf(u2())
        for _ in range(u2()):
            utf(u2())
            element(depth + 1)
        return name

    def attributes():
        annotations = []
        source_file = None
        for _ in range(u2()):
            name = utf(u2())
            length = u4()
            end = offset + length
            require(end <= len(raw), 'Class attribute outside bound')
            if name in {'RuntimeVisibleAnnotations', 'RuntimeInvisibleAnnotations'}:
                annotations.extend(((name, annotation(0)) for _ in range(u2())))
                require(offset == end, 'Annotation attribute trailing bytes')
            elif name == 'SourceFile':
                require(length == 2 and source_file is None, 'Ambiguous SourceFile')
                source_file = utf(u2())
            else:
                take(length)
        return (annotations, source_file)
    u2()
    u2()
    u2()
    for _ in range(u2()):
        u2()
    fields = []
    for _ in range(u2()):
        flags = u2()
        name = utf(u2())
        descriptor = utf(u2())
        annotations, _ = attributes()
        fields.append({'name': name, 'flags': flags, 'descriptor': descriptor, 'annotations': annotations})
    methods = []
    for _ in range(u2()):
        flags = u2()
        name = utf(u2())
        descriptor = utf(u2())
        annotations, _ = attributes()
        methods.append({'name': name, 'flags': flags, 'descriptor': descriptor, 'annotations': annotations})
    annotations, source_file = attributes()
    require(offset == len(raw) and source_file, 'Incomplete compiled source identity')
    return {'source_file': source_file, 'annotations': annotations, 'fields': fields, 'methods': methods}

def native_junit4_inventory(classfiles):
    """Complete classfile roster; actual discovery, zero runtime outcome rows."""
    import hashlib
    require(isinstance(classfiles, dict) and 0 < len(classfiles) <= 4096 and (sum(map(len, classfiles.values())) <= 16 * 1024 * 1024), 'Compiled artifact roster outside bound')
    headers = []
    details = {}
    sha = {}
    for path, raw in classfiles.items():
        row = class_header(raw)
        require(path == row['name'] + '.class', 'Classfile path contradicts internal identity')
        headers.append(row)
        details[row['name']] = native_class_details(raw)
        sha[row['name']] = hashlib.sha256(raw).hexdigest()
    selected = [row['name'] for row in headers if '$' not in row['name'] or row['junit_descriptors']]
    excluded = excluded_generated_classes(headers, selected)
    classes = []
    ids = []
    forbidden = {'Lorg/junit/runner/RunWith;', 'Lorg/junit/Ignore;', 'Lorg/junit/Rule;', 'Lorg/junit/ClassRule;'}
    for row in headers:
        name = row['name']
        d = details[name]
        annotations = [a[1] for a in d['annotations']] + [a[1] for m in d['methods'] + d['fields'] for a in m['annotations']]
        require(not forbidden.intersection(annotations), 'Unsupported JUnit runner, skip or rule')
        require(not any((('RuntimeInvisibleAnnotations', 'Lorg/junit/Test;') in m['annotations'] for m in d['methods'])), 'Invisible Test annotation is not JUnit runtime discovery')
        tests = [m for m in d['methods'] if ('RuntimeVisibleAnnotations', 'Lorg/junit/Test;') in m['annotations']]
        if not tests:
            if name in selected:
                require(row['super'] in NON_TEST_ROOTS, 'Unknown inherited discovery in empty class')
            continue
        require('$' not in name and row['super'] == 'java/lang/Object' and row['flags'] & 1 and (not row['flags'] & 1536), 'JUnit class is not public concrete Object-derived')
        constructors = [m for m in d['methods'] if m['name'] == '<init>']
        require(len(constructors) == 1 and constructors[0]['flags'] & 1 and (constructors[0]['descriptor'] == '()V'), 'JUnit class lacks one public zeroarg constructor')
        methods = []
        for m in tests:
            require(m['annotations'].count(('RuntimeVisibleAnnotations', 'Lorg/junit/Test;')) == 1 and m['flags'] & 1 and (not m['flags'] & 1032) and (m['descriptor'] == '()V'), 'JUnit method is not public nonstatic nonabstract void zeroarg')
            require(m['name'] and '/' not in m['name'], 'Ambiguous JUnit method identity')
            methods.append(name.replace('/', '.') + '.' + m['name'])
        require(len(methods) == len(set(methods)), 'Duplicate compiled test method')
        classes.append({'class': name.replace('/', '.'), 'source_file': d['source_file'], 'class_sha256': sha[name], 'ids': sorted(methods)})
        ids.extend(methods)
    require(ids and len(ids) == len(set(ids)), 'Empty or ambiguous compiled JUnit identity universe')
    return {'kind': 'compiled-junit4-discovery-not-runtime-outcomes', 'classes': sorted(classes, key=lambda row: row['class']), 'ids': sorted(ids), 'excluded_generated_classes': excluded, 'all_class_headers': headers, 'unit_runtime_outcomes': None}

def modified_utf8(raw):
    try:
        value = raw.replace(b'\xc0\x80', b'\x00').decode('utf-8', errors='surrogatepass')
        return value.encode('utf-16-le', errors='surrogatepass').decode('utf-16-le', errors='surrogatepass')
    except UnicodeError as error:
        raise InventoryError('Malformed JVM modified UTF8') from error


def compiled_roster(workspace):
    """Read every debug JVM test class from the two compiled source sets."""
    from pathlib import Path
    workspace = Path(workspace)
    build = workspace / 'clients/android/app/build'
    roots = [build / 'intermediates/built_in_kotlinc/debugUnitTest/compileDebugUnitTestKotlin/classes',
             build / 'intermediates/javac/debugUnitTest/compileDebugUnitTestJavaWithJavac/classes']
    classfiles = {}
    total_bytes = 0
    entries = 0
    for root in roots:
        require(all(not parent.is_symlink() for parent in [root, *root.parents]
                    if parent == workspace or workspace in parent.parents),
                'Compiled source-set ancestry contains a symlink')
        if not root.exists():
            continue  # Gradle NO-SOURCE is permitted for the empty Java set.
        require(root.is_dir(), 'Compiled source-set root is not a directory')
        import os
        pending = [root]
        while pending:
            directory = pending.pop()
            with os.scandir(directory) as iterator:
                for entry in iterator:
                    entries += 1
                    require(entries <= 8192, 'Compiled source-set entry count outside bound')
                    path = Path(entry.path)
                    require(not entry.is_symlink(), 'Compiled class tree contains a symlink')
                    if entry.is_dir(follow_symlinks=False):
                        require(len(path.relative_to(root).parts) <= 32, 'Compiled class tree depth outside bound')
                        pending.append(path)
                        continue
                    require(entry.is_file(follow_symlinks=False), 'Compiled class tree contains an irregular file')
                    if path.suffix != '.class':
                        require(path.name.endswith('.kotlin_module'), 'Unknown compiled test artifact')
                        continue
                    name = path.relative_to(root).as_posix()
                    require(name not in classfiles, 'Repeated class across compiled source sets')
                    size = entry.stat(follow_symlinks=False).st_size
                    require(len(classfiles) < 4096 and size <= 8 * 1024 * 1024
                            and total_bytes + size <= 16 * 1024 * 1024, 'Compiled class roster outside bound')
                    allowance = min(8 * 1024 * 1024, 16 * 1024 * 1024 - total_bytes)
                    with path.open('rb') as stream:
                        raw = stream.read(allowance + 1)
                    require(len(raw) == size and len(raw) <= allowance, 'Compiled artifact grew or changed during bounded read')
                    total_bytes += len(raw)
                    classfiles[name] = raw
    require(classfiles, 'No current compiled JVM test classes')
    return classfiles


def verify_adopted(workspace, adopted):
    """Independently compare current compiled discovery with adopted membership."""
    inventory = native_junit4_inventory(compiled_roster(workspace))
    ids = sorted(row['class'] + '/' + method[len(row['class']) + 1:]
                 for row in inventory['classes'] for method in row['ids'])
    import hashlib
    import json
    require(adopted['mode'] == 'retained-source-cohort-v1' and adopted['ids_count'] == len(ids)
            and adopted['ids_sha256'] == hashlib.sha256(json.dumps(ids, separators=(',', ':')).encode()).hexdigest(),
            'Current compiled JVM discovery differs from adopted source cohort')
    owners = []
    for row in inventory['classes']:
        package = row['class'].rsplit('.', 1)[0].replace('.', '/')
        candidates = [workspace / 'clients/android/app/src/test' / directory / package / row['source_file']
                      for directory in ('java', 'kotlin')]
        sources = [path for path in candidates if path.is_file() and not path.is_symlink()]
        require(len(sources) == 1, 'Current compiled test class has no unique source owner')
        path = sources[0]
        require(not any(parent.is_symlink() for parent in path.parents if parent == workspace or workspace in parent.parents),
                'Current JVM source ancestry contains a symlink')
        owners.append([row['class'], path.relative_to(workspace).as_posix(), hashlib.sha256(path.read_bytes()).hexdigest()])
    require(adopted['jvm_source_owners_sha256'] == hashlib.sha256(json.dumps(sorted(owners), separators=(',', ':')).encode()).hexdigest(),
            'Current compiled JVM source owners differ from adopted evidence')
    require(inventory['unit_runtime_outcomes'] is None, 'Discovery asserted a runtime result')
    return ids


def main():
    import argparse
    import json
    import os
    from pathlib import Path
    from validation import python_unit_receipts as receipts
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--verify-adopted', action='store_true', required=True)
    parser.add_argument('--source-commit', required=True)
    args = parser.parse_args()
    candidate = receipts.sha(args.source_commit)
    require(candidate == os.environ['GITHUB_SHA'], 'Current JVM checkout differs from qualification source')
    proof = receipts.bounded_json(os.environ['PLURX_RETAINED_UNITS'].encode())
    require(proof['candidate_sha'] == candidate, 'JVM adoption belongs to another source')
    ids = verify_adopted(Path.cwd(), proof['command_units']['android_jvm'])
    print('Current compiled JVM source cohort verified: ' + str(len(ids)) + ' methods; zero bodies executed')


if __name__ == '__main__':
    main()
