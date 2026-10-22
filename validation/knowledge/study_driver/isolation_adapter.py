"""Private isolation integration; no runtime at import.

Trusted stager methods: prepare(spec,replacements,artifacts),
configuration(handle,command), prerequisite_stage(handle,commands),
cache_identity(handle,commands,verified_probe,qualified_prerequisite),
and close(handle). cache_identity measures complete reconstructed source bytes
and modes, staged immutable grader bytes/modes, immutable image, runtime,
toolchain/dependency/cache identity and execution-policy identity. It contains
no volatile timing or host temporary-name component. No gold object is staged.
"""
import hashlib,json,os,re,time
from pathlib import Path, PurePosixPath

class AcceptanceAdapterError(RuntimeError):pass

def digest(raw):return hashlib.sha256(raw).hexdigest()
def canonical(value):return json.dumps(value,sort_keys=True,separators=(",",":"),ensure_ascii=False).encode()
def checked_public(value,allowed):
    if not isinstance(value,dict) or set(value)!={"status","digest"} or value["status"] not in allowed or not isinstance(value["digest"],str) or re.fullmatch(r"(?:sha256:)?[0-9a-f]{64}",value["digest"]) is None:
        raise AcceptanceAdapterError("InvalidIsolationPublicReceipt")
    return value

def grader_relative(value):
    if not isinstance(value,str) or not value:raise AcceptanceAdapterError("UnsafeGraderRelativePath")
    path=PurePosixPath(value)
    if path.is_absolute() or str(path)!=value or value=="." or ".." in path.parts or "\\" in value or "\x00" in value or "\n" in value or "\r" in value:
        raise AcceptanceAdapterError("UnsafeGraderRelativePath")
    return value

def artifact_layout(artifacts,commands):
    """Preserve sealed grader argv locations; ambiguity is never guessed."""
    if not artifacts:return ()
    names=[artifact["filename"] for artifact in artifacts]
    if len(names)!=len(set(names)) or any(PurePosixPath(grader_relative(name)).name!=name for name in names):raise AcceptanceAdapterError("AmbiguousArtifactBasename")
    references=set()
    for command in commands:
        for token in command["argv"]:
            if not isinstance(token,str):raise AcceptanceAdapterError("InvalidGraderArgv")
            if token=="/grader":raise AcceptanceAdapterError("UnresolvedGraderDirectoryReference")
            if token.startswith("/grader/"):references.add(grader_relative(token[len("/grader/"):]))
    if not references or any(PurePosixPath(path).name not in names for path in references):raise AcceptanceAdapterError("UnresolvedGraderArtifactReference")
    directories={PurePosixPath(path).parent.as_posix() for path in references}
    result=[]
    for artifact in artifacts:
        matches={path for path in references if PurePosixPath(path).name==artifact["filename"]}
        if len(matches)>1:raise AcceptanceAdapterError("AmbiguousGraderArtifactReference")
        if matches:relative=next(iter(matches))
        else:
            if len(directories)!=1:raise AcceptanceAdapterError("AmbiguousGraderSiblingDirectory")
            parent=next(iter(directories));relative=artifact["filename"] if parent=="." else parent+"/"+artifact["filename"]
        result.append(dict(artifact,relative_path=grader_relative(relative)))
    if len({row["relative_path"] for row in result})!=len(result):raise AcceptanceAdapterError("GraderArtifactCollision")
    return tuple(result)

class ValidatedAcceptanceCache:
    """Private content-addressed complete receipts; never cache unavailable."""
    def __init__(self,directory):
        self.directory=Path(directory)
        if not self.directory.is_dir() or self.directory.is_symlink() or self.directory.stat().st_mode&0o777!=0o700:raise AcceptanceAdapterError("UnprotectedAcceptanceCache")
    def load(self,key):
        path=self.directory/(key+".json")
        if not path.exists():return None
        if path.is_symlink() or path.stat().st_mode&0o777!=0o600:raise AcceptanceAdapterError("UnprotectedAcceptanceReceipt")
        record=json.loads(path.read_bytes());payload=record["payload"]
        if digest(canonical(payload))!=record["sha256"] or payload["key"]!=key or payload["result"]["status"] not in {"passed","failed"} or payload["result"].get("isolation_verified") is not True or payload.get("complete_validated_receipt") is not True or payload.get("cleanup_verified") is not True or payload["result"].get("classification") not in {"passed","assertion-failed","compile-failed"} or re.fullmatch(r"[a-f0-9]{64}",payload["result"].get("family_binding_digest","")) is None:
            raise AcceptanceAdapterError("UnvalidatedAcceptanceCacheEntry")
        return record
    def store(self,key,result):
        if result.get("status") not in {"passed","failed"} or result.get("isolation_verified") is not True or result.get("classification") not in {"passed","assertion-failed","compile-failed"} or re.fullmatch(r"[a-f0-9]{64}",result.get("family_binding_digest","")) is None:raise AcceptanceAdapterError("UncacheableAcceptance")
        payload={"key":key,"result":result,"complete_validated_receipt":True,"cleanup_verified":True}
        record={"payload":payload,"sha256":digest(canonical(payload))};path=self.directory/(key+".json")
        raw=json.dumps(record,sort_keys=True,indent=2).encode()+b"\n"
        try:
            fd=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
        except FileExistsError:
            existing=self.load(key)
            if existing["payload"]["result"]["status"]!=result["status"]:raise AcceptanceAdapterError("ConflictingAcceptanceReceipt")
            return existing
        with os.fdopen(fd,"wb") as stream:stream.write(raw)
        return record

class IsolatedAcceptance:
    def __init__(self,*,isolation_api,stager,preflight_config,source_commit,source_tree,cache):
        self.api=isolation_api;self.stager=stager;self.preflight_config=preflight_config
        self.source_commit=source_commit;self.source_tree=source_tree;self.cache=cache;self.prequalified={}
    def _artifacts(self,case,commands):
        result=[]
        for artifact in case["acceptance"]["artifacts"]:
            p=Path(artifact["private_path"])
            if p.is_symlink():raise AcceptanceAdapterError("AcceptanceSymlink")
            raw=p.read_bytes()
            if digest(raw)!=artifact["sha256"]:raise AcceptanceAdapterError("AcceptanceDigestMismatch")
            result.append({"filename":p.name,"content":raw,"sha256":artifact["sha256"]})
        return artifact_layout(result,commands)
    def _spec(self,case):
        meta=case["frozen_metadata"]
        return {"id":case["id"],"source_commit":self.source_commit,"source_tree":self.source_tree,"source_projections":case["source_projections"],"source_transform":meta.get("source_transform"),"source_transform_target":meta.get("source_transform_target"),"allowed_edit_paths":case["allowed_edit_paths"]}
    def _qualification_key(self,case,artifacts,commands):
        return digest(canonical({"source":self._spec(case),"artifacts":[a["sha256"] for a in artifacts],"commands":commands}))
    def study_prerequisites(self,cases_and_commands):
        """All frozen acceptance families must qualify before any model spend."""
        prepared=[];unsupported=0
        for case,commands in cases_and_commands:
            artifacts=self._artifacts(case,commands)
            if not self.stager.supports_family(artifacts,commands):unsupported+=1
            prepared.append((case,commands,artifacts))
        if unsupported:return {"status":"unavailable","supported_count":len(prepared)-unsupported,"unsupported_count":unsupported}
        receipts=[]
        for case,commands,artifacts in prepared:
            handle=self.stager.prepare(self._spec(case),{},artifacts,commands)
            try:
                staged=self.stager.prerequisite_stage(handle,commands)
                if staged is None:return {"status":"unavailable","supported_count":len(prepared),"unsupported_count":0}
                baseline,config,family=staged;probe=self.api.probe(config)
                public=checked_public(probe.public(),{"verified","unavailable"})
                if public["status"]!="verified":return {"status":"unavailable","supported_count":len(prepared),"unsupported_count":0}
                prerequisite=self.api.qualify_family(config,family,probe)
                public=checked_public(prerequisite.public(),{"qualified","unavailable"})
                if public["status"]!="qualified" or prerequisite.family!=family.family:return {"status":"unavailable","supported_count":len(prepared),"unsupported_count":0}
                key=self._qualification_key(case,artifacts,commands);self.prequalified[key]=(family,prerequisite,baseline)
                receipts.append(public)
            finally:self.stager.close(handle)
        return {"status":"qualified","supported_count":len(prepared),"unsupported_count":0,"digest":digest(canonical(receipts))}
    def preflight(self):
        if self.preflight_config is None or getattr(self.preflight_config,"require_rust",False) is not True:return {"status":"unavailable"}
        return checked_public(self.api.probe(self.preflight_config).public(),{"verified","unavailable"})
    def _configuration(self,handle,command):
        config=self.stager.configuration(handle,command)
        if any(Path(token).name in {"cargo","rustc"} for token in command["argv"]) and getattr(config,"require_rust",False) is not True:raise AcceptanceAdapterError("RustProbeRequired")
        return config
    def evaluate(self,case,replacements,commands):
        started=time.monotonic();artifacts=self._artifacts(case,commands)
        if not commands:raise AcceptanceAdapterError("MissingAcceptanceCommands")
        spec=self._spec(case)
        handle=self.stager.prepare(spec,replacements,tuple(artifacts),commands);result=None;key=None;cached=None
        try:
            # A security/version probe alone never qualifies a dependency/test family.
            result={"status":"unavailable","classification":"unavailable","isolation_verified":False,"execution_cache_status":"not_reusable","actual_steps":0,"reason":"UnsupportedOrUnqualifiedFamily"}
            qualified=self.prequalified.get(self._qualification_key(case,artifacts,commands))
            if qualified is not None:
                family,prerequisite,qualified_baseline=qualified
                current_family=self.stager.family_plan(handle,commands)
                if self.stager.family_identity(current_family)!=self.stager.family_identity(family):raise AcceptanceAdapterError("FamilyPlanChangedAfterQualification")
                family=current_family
                baseline_public={"status":"verified","digest":prerequisite.digest}
                prerequisite_public=checked_public(prerequisite.public(),{"qualified","unavailable"})
            else:
                qualified_stage=self.stager.prerequisite_stage(handle,commands)
                if qualified_stage is None:result["elapsed_seconds"]=time.monotonic()-started;return result
                baseline,baseline_config,family=qualified_stage;qualified_baseline=baseline
                baseline_probe=self.api.probe(baseline_config)
                baseline_public=checked_public(baseline_probe.public(),{"verified","unavailable"})
                if baseline_public["status"]!="verified":result["elapsed_seconds"]=time.monotonic()-started;return result
                prerequisite=self.api.qualify_family(baseline_config,family,baseline_probe)
                prerequisite_public=checked_public(prerequisite.public(),{"qualified","unavailable"})
            if prerequisite_public["status"]!="qualified":result["elapsed_seconds"]=time.monotonic()-started;return result
            if prerequisite.family!=family.family or re.fullmatch(r"[a-f0-9]{64}",prerequisite.binding_digest) is None:raise AcceptanceAdapterError("InvalidFamilyQualification")
            if getattr(family,"schema_version",1)==2:
                current_family=self.stager.family_plan(handle,commands)
                if self.stager.family_identity(current_family)!=self.stager.family_identity(family):raise AcceptanceAdapterError("FamilyPlanChangedAfterQualification")
                self.stager.adopt_qualified_runtime(handle,qualified_baseline,prerequisite)
                family=self.stager.family_plan(handle,commands)
            # Reuse also requires actual current containment and qualified family proof.
            first_config=self._configuration(handle,commands[0]);first_proof=self.api.probe(first_config)
            first_public=checked_public(first_proof.public(),{"verified","unavailable"})
            if first_public["status"]!="verified":result["elapsed_seconds"]=time.monotonic()-started;return result
            measured=self.stager.cache_identity(handle,commands,first_proof,prerequisite)
            fields={"source_bytes_modes_sha256","grader_bytes_modes_sha256","image_sha256","toolchain_runtime_dependencies_sha256","environment_policy_sha256"}
            if set(measured)!=fields or any(not isinstance(v,str) or re.fullmatch(r"(?:sha256:)?[0-9a-f]{64}",v) is None for v in measured.values()):raise AcceptanceAdapterError("IncompleteAcceptanceIdentity")
            key=digest(canonical({"case_id":case["id"],"measured":measured,"qualified_family_binding":prerequisite.binding_digest,"fixed_commands":commands,"grader_artifact_hashes":[a["sha256"] for a in artifacts]}))
            cached=self.cache.load(key)
            if cached is not None:
                prior=cached["payload"]["result"]
                result={"status":prior["status"],"classification":prior["classification"],"isolation_verified":True,"family_binding_digest":prerequisite.binding_digest,"prerequisite_receipt_sha256":prerequisite_public["digest"],"receipt_sha256":prior["receipt_sha256"],"execution_cache_status":"reused","reused_receipt_ref":"sha256:"+cached["sha256"],"verification_receipt_sha256":first_public["digest"],"actual_steps":0,"actual_execution_seconds":0.0,"original_actual_execution_seconds":prior["actual_execution_seconds"]}
            else:
                receipts=[baseline_public,prerequisite_public];seconds=0.0;steps=0;status="passed";classification="passed"
                ordered=commands[:1] if getattr(family,"schema_version",1)==2 else commands
                for index,command in enumerate(ordered):
                    config=first_config if index==0 else self._configuration(handle,command)
                    proof=first_proof if index==0 else self.api.probe(config)
                    public=checked_public(proof.public(),{"verified","unavailable"});receipts.append(public)
                    if public["status"]!="verified":status="unavailable";classification="unavailable";break
                    before=time.monotonic()
                    if getattr(family,"schema_version",1)==2:
                        execution=self.api.execute_plan(config,proof,family=family,prerequisite=prerequisite)
                        payload=Path(execution.private_path).read_bytes()
                        if digest(payload)!=execution.digest:raise AcceptanceAdapterError("ExecutionReceiptChanged")
                        steps+=len(json.loads(payload)["steps"])
                    else:
                        execution=self.api.execute(config,tuple(command["argv"]),proof,family=family,prerequisite=prerequisite);steps+=1
                    seconds+=time.monotonic()-before
                    public=checked_public(execution.public(),{"passed","failed","unavailable"});receipts.append(public)
                    classified=getattr(execution,"classification","unavailable")
                    qualified=getattr(execution,"prerequisite_digest","")==prerequisite.digest
                    correct_pair=(public["status"]=="passed" and classified=="passed") or (public["status"]=="failed" and classified in {"assertion-failed","compile-failed"})
                    if not qualified or not correct_pair:status="unavailable";classification="unavailable";break
                    if public["status"]!="passed":status=public["status"];classification=classified;break
                result={"status":status,"classification":classification,"isolation_verified":status in {"passed","failed"},"family_binding_digest":prerequisite.binding_digest,"prerequisite_receipt_sha256":prerequisite_public["digest"],"receipt_sha256":digest(canonical(receipts)),"execution_cache_status":"actual" if status in {"passed","failed"} else "not_reusable","actual_steps":steps,"actual_execution_seconds":seconds}
        finally:
            # API receipts prove container cleanup; a staging-close failure prevents caching/return.
            self.stager.close(handle)
        result["elapsed_seconds"]=time.monotonic()-started
        if cached is None and key is not None and result["status"] in {"passed","failed"}:
            record=self.cache.store(key,result);result["actual_receipt_ref"]="sha256:"+record["sha256"]
        return result
