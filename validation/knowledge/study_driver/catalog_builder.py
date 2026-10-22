"""Gold-free private mechanical catalog generation from sealed acceptance rows.

This consumes artifact identities and frozen argv only. It does not inspect Gold,
change acceptance argv, execute candidate code, stage containers or call models.
The resulting catalog/role sources stay private; public output is hashes/counts.
"""
from __future__ import annotations
import hashlib
import json
import os
from pathlib import Path

from . import request_journal as checkpoints
from . import source_binding
from . import trusted_launcher as launcher
from . import python_byte_outcome_driver as byte_driver
from . import python_rpc_contract as codec
from . import rust_driver_factory
from .. import evaluation_rust_rpc as rust_api


def _blob(root,name,raw):
    if Path(name).name!=name or not isinstance(raw,bytes):raise ValueError("literal role source required")
    path=root/name;fd=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
    with os.fdopen(fd,"wb") as stream:stream.write(raw);stream.flush();os.fsync(stream.fileno())
    directory=os.open(root,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
    try:os.fsync(directory)
    finally:os.close(directory)
    return {"path":str(path),"sha256":hashlib.sha256(raw).hexdigest(),"filename":name}


def build(rows,executables,*,closure,directory,worker_uid):
    """Create one new catalog directory, only after exact-source admission.

    This is source/data preparation, not qualification. Public role source names
    contain no case IDs. Complete frozen argv remains private and unchanged.
    """
    if not closure.installed or not closure.verify():raise ValueError("reviewed source closure required")
    root=Path(directory);checkpoints._directory(root.parent);root.mkdir(mode=0o700)
    if type(worker_uid) is not int or worker_uid<=0 or worker_uid==os.getuid():raise ValueError("distinct unprivileged worker uid required")
    module=lambda name:closure.buffers["validation.knowledge.study_driver."+name]
    python_schema=_blob(root,"python-rpc-contract.data",checkpoints.canonical(codec.SCHEMA))
    if python_schema["sha256"]!=codec.SCHEMA_SHA256:raise ValueError("Python schema serialization differs")
    python_worker=_blob(root,"candidate_python_rpc.py",module("candidate_python_rpc"))
    python_protected=_blob(root,"protected_python_outcome_driver.py",module("protected_python_outcome_driver"))
    rust_schema=_blob(root,"rust-rpc-contract.data",checkpoints.canonical({"protocol":2,"module":rust_api.RPC_MODULE,"function":rust_api.RPC_FUNCTION,"decision_schema_sha256":rust_api.schema_sha256()}))
    rust_module=closure.buffers["validation.knowledge.evaluation_rust_rpc"]
    rust_runtime=closure.buffers["validation.knowledge.study_workers.rust_rpc_runtime"]
    rust_worker=_blob(root,"candidate_rust_rpc.py",rust_driver_factory.bundle(rust_module,rust_runtime,candidate=True,schema_sha256=rust_schema["sha256"]))
    rust_protected=_blob(root,"protected_rust_outcome_driver.py",rust_driver_factory.bundle(rust_module,rust_runtime,candidate=False,schema_sha256=rust_schema["sha256"]))
    families=[];roles={};counts={"inert_bytes":0,"python_rpc":0,"rust_rpc":0}
    for row in rows:
        if row["type"]!="small-patch":continue
        commands=launcher.render_acceptance(row,executables)
        artifacts=[]
        for artifact in row["acceptance"]["artifacts"]:
            raw=source_binding.read_pinned(artifact["private_path"],artifact["sha256"],protected=True,max_bytes=1024**2)
            artifacts.append((artifact,raw))
        if len(commands)==1 and len(artifacts)==1:
            try:byte_driver.recognize(artifacts[0][1],artifacts[0][0]["sha256"])
            except (ValueError,SyntaxError,UnicodeError):pass
            else:counts["inert_bytes"]+=1;continue
        cargo=any(any(Path(token).name=="cargo" for token in command["argv"]) for command in commands)
        python=any(tuple(command["argv"])[1:3]==("-m","unittest") for command in commands)
        if cargo==python:raise ValueError("unrecognized or ambiguous frozen mechanical family")
        family=("rust-decision-rpc-v2-" if cargo else "python-receipt-rpc-v2-")+hashlib.sha256(checkpoints.canonical(sorted(a["sha256"] for a,_ in artifacts))).hexdigest()[:16]
        prerequisites=[];acceptance=[]
        for index,command in enumerate(commands):
            argv=list(command["argv"]);setup=cargo and index==0
            if cargo and not setup:
                if "test" not in argv or "--no-run" in argv:raise ValueError("closed Cargo assertion shape required")
                argv.insert(argv.index("--") if "--" in argv else len(argv),"--no-run")
            # Mandatory runtime variables are independently installed by the
            # controller. Per-step environment contains only frozen extras.
            extras={key:value for key,value in command["environment"].items() if key not in ("PYTHONDONTWRITEBYTECODE","TMPDIR","TMP","TEMP")}
            prerequisites.append({"id":"prepare-"+str(index),"argv":argv,"cwd":command["cwd"],"environment":extras,
                "kind":"setup" if setup else "prerequisite","writes":["candidate","runtime"] if setup else ["runtime"],"protected_readonly":not setup})
            acceptance.append({"kind":"setup" if setup else "assertion","writes":["candidate","runtime"] if setup else ["runtime"],"protected_readonly":not setup})
        protected=rust_protected if cargo else python_protected;worker=rust_worker if cargo else python_worker;schema=rust_schema if cargo else python_schema
        families.append({"family":family,"artifact_sha256":sorted(a["sha256"] for a,_ in artifacts),
            "frozen_acceptance_commands":[{"argv":list(c["argv"]),"cwd":c["cwd"],"environment":dict(c["environment"])} for c in commands],
            "prerequisite_steps":prerequisites,"acceptance_roles":acceptance,"driver":{"filename":protected["filename"],"sha256":protected["sha256"]}})
        # The staged schema always has one exact target name; its source filename
        # is deliberately different so two schemas can coexist in this catalog.
        schema=dict(schema,filename="rpc-contract.json")
        roles[family]={"worker_driver":worker,"schema":schema,"protected_driver":protected,"worker_uid":worker_uid,
            "operations":["health","call"] if cargo else list(codec.OPERATIONS),"exception_types":["RustPanic","RustRpcError"] if cargo else list(codec.EXCEPTIONS),
            "require_rust":cargo,"prepare_rust_source":cargo,"max_requests":4096,"max_frame_bytes":65536,"max_total_bytes":16*1024**2}
        counts["rust_rpc" if cargo else "python_rpc"]+=1
    if counts!={"inert_bytes":1,"python_rpc":2,"rust_rpc":1}:raise ValueError("expected all four frozen mechanical shapes")
    recipe={"schema_version":2,"families":families}
    recipe_sha=checkpoints.atomic_private(root,"family-recipes.private.json",recipe,replace=False)
    byte_module=closure.modules["validation.knowledge.study_driver.python_byte_outcome_driver"]
    result={"family_catalog":{"path":str(root/"family-recipes.private.json"),"sha256":recipe_sha,
            "driver_sources":{python_protected["filename"]:python_protected["path"],rust_protected["filename"]:rust_protected["path"]},
            "byte_driver":{"path":byte_module["path"],"sha256":byte_module["sha256"]}},"paired_roles":roles,"counts":counts}
    checkpoints.atomic_private(root,"catalog.private.json",result,replace=False)
    closure.verify()
    return result
