"""Trusted inert-byte acceptance family; no candidate imports or subprocesses.

Recognize an immutable Python hash-check grader's complete safe AST, then
perform exactly that byte comparison. Other Python/Rust grader shapes are
unsupported, never failed. Protocol transport belongs to the isolated outer
controller; this module performs no I/O at import.
"""
import ast, copy, hashlib, json, os, re, stat
from pathlib import PurePosixPath

class UnsupportedFamily(ValueError): pass

def require(condition,reason):
    if not condition:raise UnsupportedFamily(reason)

def canonical(value):return json.dumps(value,sort_keys=True,separators=(",",":"),ensure_ascii=False).encode()

def grader_relative(value):
    require(isinstance(value,str) and bool(value),"UnsafeGraderRelativePath")
    path=PurePosixPath(value)
    require(not path.is_absolute() and str(path)==value and value!="." and ".." not in path.parts and "\\" not in value and "\x00" not in value and "\n" not in value and "\r" not in value,"UnsafeGraderRelativePath")
    return value

def read_nofollow(root,path,*,max_bytes=512*1024**2):
    relative=PurePosixPath(path)
    require(path and not relative.is_absolute() and str(relative)==path and ".." not in relative.parts and "\\" not in path and "\x00" not in path,"UnsafeInputPath")
    descriptor=os.open(root,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
    try:
        for part in relative.parts[:-1]:
            child=os.open(part,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=descriptor);os.close(descriptor);descriptor=child
        fd=os.open(relative.parts[-1],os.O_RDONLY|os.O_NOFOLLOW,dir_fd=descriptor)
        with os.fdopen(fd,"rb") as stream:
            before=os.fstat(stream.fileno());require(stat.S_ISREG(before.st_mode) and before.st_size<=max_bytes,"BoundedRegularInputRequired")
            raw=stream.read(max_bytes+1);after=os.fstat(stream.fileno())
            require(len(raw)==before.st_size and (before.st_dev,before.st_ino,before.st_size,before.st_mtime_ns,before.st_ctime_ns)==(after.st_dev,after.st_ino,after.st_size,after.st_mtime_ns,after.st_ctime_ns),"InputChangedDuringRead")
            return raw
    finally:os.close(descriptor)

_TEMPLATE='''import hashlib
import pathlib
import sys
sys.dont_write_bytecode = True
root = pathlib.Path(sys.argv[1]).resolve()
p = root / "PATH"
expected_sha256 = "DIGEST"
if hashlib.sha256(p.read_bytes()).hexdigest() != expected_sha256:
    raise SystemExit("FAILURE")
print("SUCCESS")
'''

def recognize(grader_bytes,expected_grader_sha256):
    """Recognize the whole grader, allowing only inert string parameters."""
    require(re.fullmatch(r"[a-f0-9]{64}",expected_grader_sha256) is not None and hashlib.sha256(grader_bytes).hexdigest()==expected_grader_sha256,"ImmutableGraderMismatch")
    require(len(grader_bytes)<=65536,"GraderSizeBound")
    tree=ast.parse(grader_bytes.decode("utf-8"))
    if tree.body and isinstance(tree.body[0],ast.Expr) and isinstance(tree.body[0].value,ast.Constant) and isinstance(tree.body[0].value.value,str):tree.body.pop(0)
    require(len(tree.body)==9,"UnsupportedGraderShape")
    try:
        path=tree.body[5].value.right.value
        expected=tree.body[6].value.value
        failure=tree.body[7].body[0].exc.args[0].value
        success=tree.body[8].value.args[0].value
    except (AttributeError,IndexError,TypeError):raise UnsupportedFamily("UnsupportedGraderShape") from None
    # The exact AST check below is authoritative; extraction never evaluates code.
    require(all(isinstance(value,str) for value in (path,expected,failure,success)) and re.fullmatch(r"[a-f0-9]{64}",expected),"UnsupportedGraderParameters")
    normalized=copy.deepcopy(tree)
    normalized.body[5].value.right.value="PATH";normalized.body[6].value.value="DIGEST"
    normalized.body[7].body[0].exc.args[0].value="FAILURE";normalized.body[8].value.args[0].value="SUCCESS"
    require(ast.dump(normalized,include_attributes=False)==ast.dump(ast.parse(_TEMPLATE),include_attributes=False),"UnsupportedGraderShape")
    relative=PurePosixPath(path);require(path and not relative.is_absolute() and str(relative)==path and ".." not in relative.parts and "\\" not in path and "\x00" not in path,"UnsafeGraderTarget")
    return {"target":path,"expected_sha256":expected,"grader_sha256":expected_grader_sha256}

def run(mode,argv,context):
    """Called only by the authenticated isolated outer controller.

    The grader is inert source data. This driver never executes it or imports
    any candidate module; it implements the fully recognized byte assertion.
    """
    try:
        require(mode in {"prerequisite","acceptance"},"UnknownPhase")
        require(isinstance(argv,list) and len(argv)==3 and argv[1].startswith("/grader/") and argv[2]=="/work","UnsupportedArgv")
        name=grader_relative(argv[1].removeprefix("/grader/"))
        require(context.get("family")=="immutable-python-inert-byte-sha256-v1" and re.fullmatch(r"[a-f0-9]{64}",context["plan_digest"]),"InvalidFamilyContext")
        require(mode=="prerequisite" or re.fullmatch(r"[a-f0-9]{64}",context["prerequisite_digest"]),"MissingFamilyQualification")
        grader=read_nofollow("/grader",name,max_bytes=65536);grader_sha=hashlib.sha256(grader).hexdigest()
        plan=recognize(grader,grader_sha);raw=read_nofollow("/work",plan["target"])
        actual=hashlib.sha256(raw).hexdigest();matched=actual==plan["expected_sha256"]
        classification="prerequisite-passed" if mode=="prerequisite" else "passed" if matched else "assertion-failed"
        evidence=hashlib.sha256(canonical({"phase":mode,"plan":context["plan_digest"],"baseline_source_digest":context["baseline_source_digest"],"prerequisite_digest":context["prerequisite_digest"],"grader":grader_sha,"observed_bytes":actual})).hexdigest()
        return {"protocol":1,"outcome":classification,"completed":True,"evidence":{"kind":"prerequisite" if mode=="prerequisite" else "inert-bytes","identity":"immutable-python-inert-byte-sha256-v1","sha256":evidence}}
    except (UnsupportedFamily,OSError,ValueError,UnicodeError,SyntaxError,KeyError,TypeError):
        return {"protocol":1,"outcome":"unavailable","completed":False}
