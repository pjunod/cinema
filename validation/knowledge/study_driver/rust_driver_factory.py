"""Gold-free deterministic self-contained Rust role-driver construction.

Only reviewed exact source buffers are bundled. Neither generated role imports
Python modules from candidate source. The candidate role returns decision values;
the protected role owns frozen Cargo argv, framework counts and outcomes.
"""
from __future__ import annotations
import ast
import hashlib


def bundle(rpc_source, runtime_source, *, candidate, schema_sha256):
    if type(rpc_source) is not bytes or type(runtime_source) is not bytes or type(candidate) is not bool:
        raise ValueError("exact reviewed role-source buffers required")
    tree = ast.parse(runtime_source)
    imports = [node for node in tree.body if isinstance(node, ast.ImportFrom)
               and node.module == "validation.knowledge.evaluation_rust_rpc"]
    if len(imports) != 1:
        raise ValueError("closed runtime helper import shape required")
    node = imports[0];lines = runtime_source.decode().splitlines(keepends=True)
    lines[node.lineno - 1:node.end_lineno] = ["from _study_rpc_types import (" + ",".join(alias.name for alias in node.names) + ")\n"]
    runtime = "".join(lines).encode()
    prefix = ("import sys,types\n"
        "_rpc_types=types.ModuleType('_study_rpc_types');_rpc_types.__file__=__file__\n"
        "sys.modules['_study_rpc_types']=_rpc_types\n"
        "exec(compile(" + repr(rpc_source) + ",__file__,'exec',dont_inherit=True),_rpc_types.__dict__)\n"
        "_runtime=types.ModuleType('_study_rpc_runtime');_runtime.__file__=__file__\n"
        "sys.modules['_study_rpc_runtime']=_runtime\n"
        "exec(compile(" + repr(runtime) + ",__file__,'exec',dont_inherit=True),_runtime.__dict__)\n"
        "EXPECTED_SCHEMA=" + repr(schema_sha256) + "\n")
    result = (prefix + (CANDIDATE if candidate else PROTECTED)).encode()
    if len(result) > 8 * 1024**2:
        raise ValueError("bounded role source required")
    return result


CANDIDATE = r'''
import hashlib,json,os,subprocess
_worker=None
_BUILD=('/usr/local/rustup/toolchains/1.97.1-aarch64-unknown-linux-gnu/bin/cargo','build','--offline','--locked','-p','plurx-core','--features','hiqlite-store','--bin','study_rpc')
_BINARY='/work/.runtime/target/debug/study_rpc'
def dispatch(method,args,kwargs,context):
    global _worker
    if context.get('protocol')!=2 or context.get('schema_sha256')!=EXPECTED_SCHEMA or type(args) is not list or kwargs!={}:
        raise ValueError('closed Rust role context required')
    if method not in ('health','call'):raise ValueError('operation outside Rust seam')
    if _worker is None:
        # Candidate build success is dependency availability only. No result here
        # can supply a protected assertion count or authenticated outcome.
        result=subprocess.run(_BUILD,cwd='/work',env=dict(os.environ),stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=1800,close_fds=True)
        if result.returncode:raise ValueError('candidate compile unavailable')
        _worker=_runtime.RustCandidateWorker((_BINARY,),cwd='/work',env=dict(os.environ),timeout=60)
    if method=='health':
        if args:raise ValueError('health takes no arguments')
        if _worker.process.poll() is not None:raise ValueError('worker exited')
        return {'value':{'ready':True}}
    return _worker.dispatch(method,args,kwargs,context)
'''


PROTECTED = r'''
import hashlib,json,os,pathlib,re,stat,subprocess
rpc=globals().get('rpc')
def _unavailable():return {'protocol':2,'outcome':'unavailable','completed':False}
def _digest(value):return hashlib.sha256(json.dumps(value,sort_keys=True,separators=(',',':'),allow_nan=False).encode()).hexdigest()
def _read_log(relative):
    if not re.fullmatch(r'target/quiet/[A-Za-z0-9._-]+\.log',relative):raise ValueError('closed quiet log path required')
    directory=os.open('/work',os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
    try:
        parts=relative.split('/')
        for component in parts[:-1]:
            child=os.open(component,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=directory);os.close(directory);directory=child
        fd=os.open(parts[-1],os.O_RDONLY|os.O_NOFOLLOW,dir_fd=directory)
        with os.fdopen(fd,'rb') as stream:
            before=os.fstat(stream.fileno())
            if not stat.S_ISREG(before.st_mode) or before.st_size>16*1024**2:raise ValueError('bounded protected log required')
            raw=stream.read(16*1024**2+1);after=os.fstat(stream.fileno())
        identity=lambda i:(i.st_dev,i.st_ino,i.st_size,i.st_mtime_ns,i.st_ctime_ns)
        if len(raw)!=before.st_size or identity(before)!=identity(after):raise ValueError('protected log changed')
        return raw
    finally:os.close(directory)
def framework_counts(observed):
    # scripts/quiet's final line is produced by the immutable protected wrapper,
    # after Cargo and every returned candidate value. It chooses one protected
    # logfile, never a candidate-supplied assertion marker or path.
    text=observed.stdout.decode('utf-8','strict')
    footer=re.search(r'^── (ok|exit [0-9]+) in [0-9]+s · full log: (target/quiet/[A-Za-z0-9._-]+\.log) ──\n?\Z',text,re.M)
    if footer is None:raise ValueError('complete trusted wrapper observation required')
    raw=_read_log(footer[2]);log=raw.decode('utf-8','strict')
    starts=list(re.finditer(r'^running ([0-9]+) tests?\r?$',log,re.M))
    summaries=list(re.finditer(r'^test result: (ok|FAILED)\. ([0-9]+) passed; ([0-9]+) failed; ([0-9]+) ignored; ([0-9]+) measured; ([0-9]+) filtered out; finished in [0-9.]+s\r?$',log,re.M))
    if not starts or not summaries:raise ValueError('trusted framework counts unavailable')
    summary=summaries[-1]
    # The first discovery line is framework-controlled and precedes the first
    # candidate RPC. A candidate reason string printed inside a failing assert
    # cannot replace the terminal framework summary emitted after that failure.
    planned=int(starts[0][1]);passed=int(summary[2]);failed=int(summary[3]);ignored=int(summary[4]);measured=int(summary[5])
    tail=log[summary.end():].strip()
    if tail and not re.fullmatch(r'error: test failed, to rerun pass `[^`\n]+`',tail):raise ValueError('unexpected post-framework output')
    if planned<=0 or passed+failed!=planned or ignored or measured or summary.start()<starts[0].end():raise ValueError('incomplete selected assertion suite')
    if failed:
        if summary[1]!='FAILED' or observed.returncode!=101 or footer[1]!='exit 101':raise ValueError('failure observation mismatch')
    elif summary[1]!='ok' or observed.returncode!=0 or footer[1]!='ok':raise ValueError('success observation mismatch')
    return planned,failed,hashlib.sha256(raw).hexdigest()
def run(mode,argv,context):
    try:
        if mode not in ('prerequisite','acceptance') or not callable(rpc) or type(argv) is not list or not argv:return _unavailable()
        step=context.get('step',{})
        if context.get('protocol')!=2 or step.get('argv') not in (argv,tuple(argv)):return _unavailable()
        if rpc('health',[],{})!={'value':{'ready':True}}:return _unavailable()
        observed=_runtime.run_exact_assertion_command(tuple(argv),rpc,cwd=step['cwd'],env=dict(context['effective_environment']),timeout=1800)
        if step.get('kind')=='setup':
            if observed.returncode:return _unavailable()
            completed=1;failed=0;outcome='setup-passed';kind='setup'
            log_digest=hashlib.sha256(observed.stdout+observed.stderr).hexdigest()
        elif mode=='prerequisite':
            if observed.returncode:return _unavailable()
            completed=1;failed=0;outcome='prerequisite-passed';kind='prerequisite'
            log_digest=hashlib.sha256(observed.stdout+observed.stderr).hexdigest()
        else:
            if step.get('kind')!='assertion' or argv[0]!='/work/scripts/quiet':return _unavailable()
            completed,failed,log_digest=framework_counts(observed)
            if observed.rpc_calls<=0:return _unavailable()
            outcome='assertion-failed' if failed else 'passed';kind='assertion'
        evidence={'argv':argv,'completed_checks':completed,'failed_checks':failed,'rpc_calls':observed.rpc_calls,'log_sha256':log_digest,'stdout_sha256':hashlib.sha256(observed.stdout).hexdigest(),'stderr_sha256':hashlib.sha256(observed.stderr).hexdigest()}
        return {'protocol':2,'outcome':outcome,'completed':True,'evidence':{'kind':kind,'identity':'protected-rust-framework-rpc-v2','sha256':_digest(evidence),'completed_checks':completed,'failed_checks':failed,'skipped_checks':0}}
    except (Exception,SystemExit):return _unavailable()
'''
