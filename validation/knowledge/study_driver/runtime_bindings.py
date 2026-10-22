"""Custodian runtime bindings. Source-only draft; importing performs no I/O.

No gold literals, provider construction, runtime auto-start, or model tools.
Only a reviewed supervisor may instantiate these bindings after admission.
"""
from dataclasses import dataclass, asdict, replace
from pathlib import Path, PurePosixPath
import hashlib, json, os, re, shutil, stat, tarfile, time, uuid, threading, inspect

class BindingRefusal(RuntimeError): pass

def require(value, code):
    if not value: raise BindingRefusal(code)
def canonical(value): return json.dumps(value,sort_keys=True,separators=(",",":"),ensure_ascii=False).encode()
def sha(value): return hashlib.sha256(value).hexdigest()
def file_sha(path):
    h=hashlib.sha256()
    with open(path,"rb") as stream:
        for chunk in iter(lambda:stream.read(1024*1024),b""):h.update(chunk)
    return h.hexdigest()
def relative(value):
    p=PurePosixPath(value)
    require(isinstance(value,str) and value and not p.is_absolute() and ".." not in p.parts and "\\" not in value and "\x00" not in value and str(p)==value,"UnsafeRelativePath")
    return p

def no_links(path):
    p=Path(path);require(p.is_absolute(),"AbsoluteOwnedPathRequired")
    current=Path(p.anchor)
    for name in p.parts[1:]:
        current/=name;require(not current.is_symlink(),"SymlinkRefused")
    return p

def private_json(path):
    p=no_links(path);require(p.stat().st_mode&0o777==0o600,"PrivateModeRequired")
    return json.loads(p.read_bytes())

def durable(path,value):
    path=Path(path);require(path.parent.stat().st_mode&0o777==0o700,"PrivateDirectoryRequired")
    raw=canonical(value);fd=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
    with os.fdopen(fd,"wb") as stream:stream.write(raw);stream.flush();os.fsync(stream.fileno())
    return sha(raw)

def private_name(name):
    n=name.lower()
    return n in {".git",".aws",".ssh",".codex",".hermes",".docker",".cargo",".isolation","evaluation-private","gold","gold.json","auth.json"} or n.startswith((".env","credentials","secret")) or n.endswith((".pem",".key",".fleet.json"))

PUBLIC_SOURCE_EXCEPTIONS=frozenset(("crates/plurx-core/src/secrets.rs","crates/plurx-compat-jellyfin/src/credentials.rs"))

def grader_relative(value):
    require(isinstance(value,str) and bool(value),"UnsafeGraderRelativePath")
    path=PurePosixPath(value)
    require(not path.is_absolute() and str(path)==value and value!="." and ".." not in path.parts and "\\" not in value and "\x00" not in value and "\n" not in value and "\r" not in value,"UnsafeGraderRelativePath")
    return value

def write_grader(root,relative_path,content):
    """Create immutable artifacts through no-follow directory descriptors."""
    path=PurePosixPath(grader_relative(relative_path))
    require(not any(private_name(part) for part in path.parts),"PrivateGraderPath")
    directory=os.open(root,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
    try:
        for part in path.parts[:-1]:
            try:os.mkdir(part,mode=0o755,dir_fd=directory)
            except FileExistsError:pass
            child=os.open(part,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=directory);os.close(directory);directory=child
        descriptor=os.open(path.name,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o444,dir_fd=directory)
        with os.fdopen(descriptor,"wb") as stream:stream.write(content);os.fchmod(stream.fileno(),0o444)
    finally:os.close(directory)

class TarBaseline:
    """Hash-verified source archive reader; never uses a live checkout."""
    def __init__(self,archive,*,archive_sha256,source_commit,source_tree,source_exceptions,source_permits=None,max_bytes=512*1024**2):
        self.path=no_links(archive);self.expected=archive_sha256;self.commit=source_commit;self.tree=source_tree
        require(re.fullmatch(r"[a-f0-9]{64}",archive_sha256) and re.fullmatch(r"[a-f0-9]{40}",source_commit) and re.fullmatch(r"[a-f0-9]{40}",source_tree),"InvalidArchiveIdentity")
        require(set(source_exceptions)==PUBLIC_SOURCE_EXCEPTIONS and all(re.fullmatch(r"[a-f0-9]{64}",v) for v in source_exceptions.values()),"ExactPublicSourceExceptionsRequired")
        self.exceptions=dict(source_exceptions);self.rows={};self.max_bytes=max_bytes
        self.permits={};self.permit_ancestors=set()
        if source_permits is not None:
            require(isinstance(source_permits,dict) and source_permits.get("schema_version")==1 and source_permits.get("source_commit")==source_commit and source_permits.get("source_tree")==source_tree and source_permits.get("archive_sha256")==archive_sha256,"SourcePermitBaselineMismatch")
            for row in source_permits["members"]:
                name=str(relative(row["path"]));parts=PurePosixPath(name).parts
                hard={".git",".ssh",".aws",".gnupg",".docker",".codex",".hermes","evaluation-private","auth.json"}
                require(not any(x.lower() in hard for x in parts),"NonoverridablePrivateSourceMember")
                require(name not in self.permits and re.fullmatch(r"[a-f0-9]{64}",row["sha256"]) and re.fullmatch(r"[a-f0-9]{40}",row["git_blob"]) and row["archive_sha256"]==archive_sha256,"InvalidSourcePermit")
                ancestors={str(x) for x in PurePosixPath(name).parents if str(x)!="."}
                require(set(row["required_ancestors"])==ancestors,"SourcePermitAncestorMismatch")
                self.permits[name]=dict(row);self.permit_ancestors.update(ancestors)
        # An open no-follow descriptor pins the archive; contents are checked before/after parsing.
        fd=os.open(self.path,os.O_RDONLY|os.O_NOFOLLOW);self.stream=os.fdopen(fd,"rb")
        self.initial=os.fstat(fd);require(stat.S_ISREG(self.initial.st_mode),"RegularArchiveRequired")
        require(self._hash()==self.expected,"ArchiveDigestMismatch")
        self.stream.seek(0);self.tar=tarfile.open(fileobj=self.stream,mode="r:gz")
        total=0
        for member in self.tar:
            require(member.name=="repo" or member.name.startswith("repo/"),"ArchivePrefixMismatch")
            name=member.name.removeprefix("repo/").rstrip("/")
            if name=="repo" or not name:continue
            path=relative(name)
            private=any(private_name(p) for p in path.parts)
            require(not private or (member.isfile() and (name in self.exceptions or name in self.permits)) or (member.isdir() and name in self.permit_ancestors),"PrivateArchiveMember")
            require(member.isfile() or member.isdir(),"ArchiveLinksOrSpecialFilesRefused")
            if member.isdir():continue
            require(name not in self.rows and member.size>=0,"DuplicateArchiveMember")
            total+=member.size;require(total<=max_bytes and len(self.rows)<100000,"ArchiveSizeBound")
            self.rows[name]=member
        require((set(self.exceptions)|set(self.permits))<=set(self.rows),"MissingPublicSourceException")
        for name,row in self.permits.items():
            raw=self.read_raw(name);oid=hashlib.sha1(b"blob "+str(len(raw)).encode()+b"\0"+raw).hexdigest()
            require(sha(raw)==row["sha256"] and oid==row["git_blob"],"TrackedSourcePermitDigestMismatch")
        for name,expected in self.exceptions.items():require(sha(self.read_raw(name))==expected,"PublicSourceExceptionDigestMismatch")
        require(self._hash()==self.expected and self._stable(),"ArchiveChangedDuringIndex")
    def _stable(self):
        now=os.fstat(self.stream.fileno());return (now.st_dev,now.st_ino,now.st_size,now.st_mtime_ns)==(self.initial.st_dev,self.initial.st_ino,self.initial.st_size,self.initial.st_mtime_ns)
    def _hash(self):
        self.stream.seek(0);h=hashlib.sha256()
        for chunk in iter(lambda:self.stream.read(1024*1024),b""):h.update(chunk)
        return h.hexdigest()
    def read_raw(self,path):
        name=str(relative(path));require(name in self.rows and self._stable(),"ArchiveMemberUnavailable")
        stream=self.tar.extractfile(self.rows[name]);require(stream is not None,"ArchiveReadFailure")
        raw=stream.read(self.max_bytes+1);require(len(raw)==self.rows[name].size,"ArchiveMemberSizeMismatch");return raw
    def __call__(self,commit,path,git_blob_oid):
        require(commit==self.commit and re.fullmatch(r"[a-f0-9]{40}",git_blob_oid),"BaselineIdentityMismatch")
        raw=self.read_raw(path);oid=hashlib.sha1(b"blob "+str(len(raw)).encode()+b"\0"+raw).hexdigest()
        require(oid==git_blob_oid,"GitBlobIdentityMismatch");return raw
    def materialize(self,destination):
        require(self._hash()==self.expected and self._stable(),"ArchiveChangedBeforeMaterialization")
        root=no_links(destination);require(root.is_dir() and not any(root.iterdir()),"EmptyCandidateRequired")
        for name,member in self.rows.items():
            p=root/str(relative(name));p.parent.mkdir(parents=True,exist_ok=True,mode=0o755)
            raw=self.read_raw(name);fd=os.open(p,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
            with os.fdopen(fd,"wb") as stream:stream.write(raw)
            os.chmod(p,0o755 if member.mode&0o111 else 0o644)
        require(self._hash()==self.expected and self._stable(),"ArchiveChangedDuringMaterialization")
    def close(self):self.tar.close();self.stream.close()

class PinnedCallback:
    """Trusted supervisor injects callbacks; their defining source is hash-pinned."""
    def __init__(self,callback,*,source_path,source_sha256):
        require(callable(callback),"CallbackRequired");self.callback=callback;self.path=no_links(source_path);self.expected=source_sha256
        code=getattr(callback,"__code__",None) or getattr(getattr(callback,"__call__",None),"__code__",None)
        require(code is not None and Path(code.co_filename).resolve()==self.path.resolve(),"CallbackSourceMismatch")
        self.verify()
    def verify(self):require(file_sha(self.path)==self.expected,"CallbackSourceChanged")
    def __call__(self,*args,**kwargs):self.verify();return self.callback(*args,**kwargs)

class Admission:
    def __init__(self,pinned_lease_check):self.check=pinned_lease_check
    def assert_current(self):require(self.check() is True,"SupervisorQueueLeaseUnavailable")

class ReviewGate:
    def __init__(self,*,trusted_receipt_path,trusted_receipt_sha256,source_paths):
        self.path=no_links(trusted_receipt_path);self.expected=trusted_receipt_sha256;self.paths=dict(source_paths)
    def verify(self,reviewed_source_hashes,receipt):
        require(file_sha(self.path)==self.expected and receipt==self.expected,"ReviewReceiptIdentityMismatch")
        body=private_json(self.path)
        require(body.get("verdict")=="approved" and body.get("source_hashes")==reviewed_source_hashes and set(reviewed_source_hashes)==set(self.paths),"FinalAdversarialReviewRequired")
        for key,path in self.paths.items():require(file_sha(no_links(path))==reviewed_source_hashes[key],"ReviewedSourceChanged")
        require(body.get("runtime_authorized") is True,"RuntimeNotAuthorizedByReview")

class DurableRequestJournal:
    """Append-only private write-ahead events; any write failure poisons the run.

    Existing events are never silently resumed/retried. A paid request with a
    reserved event and no durable response remains uncertain after interruption.
    """
    def __init__(self,directory,*,manifest_sha256):
        self.root=no_links(directory)
        require(self.root.is_dir() and self.root.stat().st_mode&0o777==0o700 and not any(self.root.iterdir()),"FreshPrivateJournalRequired")
        require(re.fullmatch(r"sha256:[a-f0-9]{64}",manifest_sha256),"JournalManifestIdentityRequired")
        self.manifest=manifest_sha256;self.sequence=0;self.previous="";self.poisoned=False;self.lock=threading.Lock()
    def __call__(self,event):return self.append(event)
    def append(self,event):
        with self.lock:
            require(not self.poisoned and isinstance(event,dict),"RequestJournalUnavailable")
            self.poisoned=True
            row={"schema_version":1,"manifest_sha256":self.manifest,"sequence":self.sequence,"previous_sha256":self.previous,"event":event}
            raw=canonical(row);directory=os.open(self.root,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
            try:
                descriptor=os.open(str(self.sequence).zfill(8)+".json",os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600,dir_fd=directory)
                with os.fdopen(descriptor,"wb") as stream:stream.write(raw);stream.flush();os.fsync(stream.fileno())
                os.fsync(directory)
            finally:os.close(directory)
            self.previous=sha(raw);self.sequence+=1;self.poisoned=False
            return self.previous

class ReviewedNavigation:
    """Concrete factory + reviewed module/assets/config identity, not a boolean."""
    def __init__(self,factory,*,api,module_path,module_sha256,identity_sha256):
        module_path=no_links(module_path)
        require(type(factory) is api.VisibleRipwireFactory,"ConcreteVisibleNavigationRequired")
        require(Path(inspect.getfile(type(factory))).resolve()==module_path.resolve() and file_sha(module_path)==module_sha256,"NavigationModuleIdentityMismatch")
        require(sha(canonical(factory.identity))==identity_sha256,"NavigationFactoryIdentityMismatch")
        self.factory=factory;self.api=api;self.path=module_path;self.module_sha256=module_sha256;self.identity_sha256=identity_sha256
    def verify(self):
        require(type(self.factory) is self.api.VisibleRipwireFactory and file_sha(self.path)==self.module_sha256 and sha(canonical(self.factory.identity))==self.identity_sha256,"NavigationFactoryChanged")
    def __call__(self,spec,case):self.verify();return self.factory(spec,case)

class ManifestRecorder:
    def __init__(self,directory,admission):self.directory=no_links(directory);self.admission=admission
    def record(self,manifest,*,review_receipt_sha256,calibration_receipt_sha256,ev):
        self.admission.assert_current()
        require(re.fullmatch(r"[a-f0-9]{64}",review_receipt_sha256) and re.fullmatch(r"[a-f0-9]{64}",calibration_receipt_sha256),"PrerequisiteReceiptRequired")
        name="manifest-"+manifest.sha256.replace("sha256:","")+".json"
        payload={"manifest_sha256":manifest.sha256,"manifest":asdict(manifest),"review_receipt_sha256":review_receipt_sha256,"calibration_receipt_sha256":calibration_receipt_sha256}
        path=self.directory/name;digest=durable(path,payload)
        return ev.ManifestReceipt(manifest.sha256,"sha256:"+digest)

def verify_graph(graph,manifest):
    text=graph.to_json();require(isinstance(text,str),"InvalidGraphSerialization")
    actual="sha256:"+sha(text.encode());require(actual==manifest.knowledge_sha256,"ManifestKnowledgeGraphMismatch")
    return actual

def graph_query_factory(queries_api):
    """Derive every projection/key from the same verified immutable graph object."""
    def bind_graph(graph,*,projection_api,source_commit,source_tree,expected_knowledge_sha256):
        actual="sha256:"+sha(graph.to_json().encode());require(actual==expected_knowledge_sha256,"QueryGraphDigestMismatch")
        def bind(files,visibility,snapshot):
            require("sha256:"+sha(graph.to_json().encode())==actual,"QueryGraphChanged")
            projection=projection_api.project(graph,files,visibility)
            require(snapshot["head"]==source_commit and snapshot["tree"]==source_tree,"QueryBaselineMismatch")
            key=sha(canonical({"knowledge":actual,"visible":projection.visibility_digest,"commit":source_commit,"tree":source_tree}))
            scoped=dict(snapshot,key=key,worktree_id="visible-"+key,knowledge_view="sealed-visible-projection",repository_dirty=True,change_view="curated-visible-snapshot; baseline identity is provenance only")
            def query(request):
                require("sha256:"+sha(graph.to_json().encode())==actual,"QueryGraphChanged")
                require(request.kind in {"context","obligations","why","evidence","conflicts"} and request.max_depth in {0,6} and request.max_bytes==8192,"UnregisteredKnowledgeRequest")
                kwargs={"max_depth":request.max_depth,"max_bytes":request.max_bytes,"continuation":request.continuation}
                if request.kind in {"context","obligations"}:kwargs["paths"]=(request.target,)
                elif request.kind in {"why","evidence"}:kwargs["identity"]=request.target
                else:kwargs["cohort"]=request.target
                result=queries_api.answer(projection,scoped,command=request.kind,**kwargs)
                require(len(canonical(result))<=8192,"QueryEnvelopeBoundExceeded");return result
            return query
        return bind
    return bind_graph

def command_environment(command):
    """Remove only mandatory runtime values independently fixed by the executor."""
    env=dict(command["environment"])
    for key,value in {"PYTHONDONTWRITEBYTECODE":"1","TMPDIR":"/work/.runtime/tmp","TMP":"/work/.runtime/tmp","TEMP":"/work/.runtime/tmp"}.items():
        require(env.pop(key,None)==value,"FrozenEnvironmentMismatch")
    return tuple(sorted(env.items()))

class FrozenFamilyRecipes:
    """Pinned inert engineering recipes, kept separate from all semantic Gold.

    Acceptance argv/cwd/env must exactly equal the sealed rendered pipeline.
    Extra baseline prerequisite commands and protected outcome drivers require
    their own source review; merely loading a recipe grants no qualification.
    """
    def __init__(self,path,expected_sha256,*,api,driver_sources):
        self.path=no_links(path);require(file_sha(self.path)==expected_sha256,"FamilyRecipeDigestMismatch")
        data=private_json(self.path);require(set(data)=={"schema_version","families"} and data["schema_version"]==2,"FamilyRecipeSchemaMismatch")
        self.api=api;self.rows=tuple(data["families"]);self.drivers=dict(driver_sources);self.expected=expected_sha256
    def resolve(self,artifacts,commands):
        require(file_sha(self.path)==self.expected,"FamilyRecipeChanged")
        frozen=[{"argv":list(row["argv"]),"cwd":row["cwd"],"environment":dict(row["environment"])} for row in commands]
        artifact_ids=sorted(row["sha256"] for row in artifacts)
        matches=[row for row in self.rows if row["artifact_sha256"]==artifact_ids and row["frozen_acceptance_commands"]==frozen]
        require(len(matches)<=1,"AmbiguousFamilyRecipe")
        if not matches:return None
        row=matches[0];require(set(row)=={"family","artifact_sha256","frozen_acceptance_commands","prerequisite_steps","acceptance_roles","driver"},"FamilyRecipeFieldsMismatch")
        driver=row["driver"];require(set(driver)=={"filename","sha256"} and driver["filename"] in self.drivers,"UnknownOutcomeDriver")
        source=no_links(self.drivers[driver["filename"]]);require(source.name==driver["filename"] and file_sha(source)==driver["sha256"],"OutcomeDriverDigestMismatch")
        require(len(row["acceptance_roles"])==len(commands),"AcceptanceRoleCountMismatch")
        acceptance=[]
        for index,(command,role) in enumerate(zip(commands,row["acceptance_roles"])):
            require(set(role) in ({"kind","writes"},{"kind","writes","protected_readonly"}),"AcceptanceRoleFieldsMismatch")
            acceptance.append(self.api.ExecutionStep("acceptance-"+str(index),tuple(command["argv"]),command["cwd"],command_environment(command),frozenset(role["writes"]),role["kind"],role.get("protected_readonly",True)))
        prerequisites=[]
        for step in row["prerequisite_steps"]:
            require(set(step) in ({"id","argv","cwd","environment","writes","kind"},{"id","argv","cwd","environment","writes","kind","protected_readonly"}),"PrerequisiteStepFieldsMismatch")
            prerequisites.append(self.api.ExecutionStep(step["id"],tuple(step["argv"]),step["cwd"],tuple(sorted(step["environment"].items())),frozenset(step["writes"]),step["kind"],step.get("protected_readonly",True)))
        plan=self.api.FamilyPlanV2(row["family"],tuple(prerequisites),tuple(acceptance),self.api.TrustedSourceFile(driver["filename"],driver["sha256"]))
        return plan,source

@dataclass
class Stage:
    root:Path
    candidate:Path
    grader:Path
    runtime:Path
    commands:tuple=()
    spec:dict|None=None
    artifacts:tuple=()
    supported_grader:str|None=None
    baseline_stage:object=None
    family_v2:object=None
    paired_role:object=None
    worker_candidate:Path|None=None
    worker_grader:Path|None=None
    worker_runtime:Path|None=None
    closed:bool=False

class SourceStager:
    """Full frozen source goes only to the unprivileged private executor mount."""
    def __init__(self,*,baseline,isolation_api,owned_root,engine,endpoint,image,python_executable,rustc_executable,cargo_executable,admission,registry_seeder=None,byte_driver_api=None,outcome_driver_path=None,outcome_driver_sha256=None,family_recipes=None,paired_roles=None,execution_options=None):
        self.baseline=baseline;self.api=isolation_api;self.root=no_links(owned_root);self.engine=engine;self.endpoint=endpoint;self.image=image
        require(self.root.is_dir() and self.root.stat().st_mode&0o777==0o700 and "evaluation-private" not in self.root.parts and "/tmp" not in str(self.root),"DedicatedPrivateExecutionRootRequired")
        self.python=python_executable;self.rustc=rustc_executable;self.cargo=cargo_executable;self.admission=admission;self.seeder=registry_seeder
        self.byte_driver_api=byte_driver_api;self.driver_path=outcome_driver_path;self.driver_sha256=outcome_driver_sha256;self.family_recipes=family_recipes
        self.execution_options=dict(execution_options or {})
        require(set(self.execution_options)<= {"timeout_seconds","max_output_bytes","memory_bytes","cpus","pids_limit","runtime_max_bytes","cache_max_bytes"},"ClosedExecutionResourceOptions")
        if self.seeder is not None and hasattr(self.seeder,"image"):
            require(self.seeder.image==self.image,"DependencySeedImageMismatch")
        self.paired=None
        if paired_roles is not None:
            from .paired_runtime import PairedRoleStager
            self.paired=PairedRoleStager(self,paired_roles)
    def prepare(self,spec,replacements,artifacts,commands=()):
        self.admission.assert_current();require(spec["source_commit"]==self.baseline.commit and spec["source_tree"]==self.baseline.tree,"StageBaselineMismatch")
        root=self.root/uuid.uuid4().hex;root.mkdir(mode=0o700)
        handle=Stage(root,root/"candidate",root/"grader",root/"runtime",spec=dict(spec),artifacts=tuple(artifacts),commands=tuple(commands))
        for p in (handle.candidate,handle.grader,handle.runtime):p.mkdir(mode=0o700)
        try:
            self.baseline.materialize(handle.candidate)
            for path,projection in spec["source_projections"].items():
                if projection.get("git_blob_oid") is None:continue
                raw=self.baseline(self.baseline.commit,path,projection["git_blob_oid"])
                require(sha(raw)==projection["baseline_sha256"],"StagedProjectionBaselineMismatch")
            mutation=spec.get("source_transform");target=spec.get("source_transform_target")
            if mutation is not None:
                p=handle.candidate/str(relative(target));text=p.read_text();require(text.count(mutation["before"])==1,"StageMutationMismatch")
                p.write_text(text.replace(mutation["before"],mutation["after"],1));require(sha(p.read_bytes())==spec["source_projections"][target]["materialized_sha256"],"StageMutationDigestMismatch")
            require(set(replacements)<=set(spec["allowed_edit_paths"]),"StagerEditPathViolation")
            for path,row in replacements.items():
                p=handle.candidate/str(relative(path));require(p.is_file() and not p.is_symlink() and row["mode"] in {0o644,0o755},"UnsafeReplacement")
                p.write_bytes(row["content"].encode());os.chmod(p,row["mode"])
            for artifact in artifacts:
                relative_path=grader_relative(artifact["relative_path"])
                require(PurePosixPath(relative_path).name==artifact["filename"] and sha(artifact["content"])==artifact["sha256"],"InvalidGraderArtifact")
                write_grader(handle.grader,relative_path,artifact["content"])
            resolved=self.family_recipes.resolve(artifacts,commands) if self.family_recipes is not None and commands else None
            if resolved is not None:
                handle.family_v2,driver_source=resolved
                name=grader_relative(handle.family_v2.driver.path)
                require(not (handle.grader/name).exists(),"OutcomeDriverNameCollision")
                write_grader(handle.grader,name,driver_source.read_bytes())
            if resolved is None and self.byte_driver_api is not None and len(artifacts)==1:
                artifact=artifacts[0]
                try:self.byte_driver_api.recognize(artifact["content"],artifact["sha256"])
                except (ValueError,SyntaxError,UnicodeError,AttributeError,IndexError,TypeError):pass
                else:
                    source=no_links(self.driver_path);require(file_sha(source)==self.driver_sha256,"OutcomeDriverSourceMismatch")
                    name="python_byte_outcome_driver.py";require(not (handle.grader/name).exists(),"OutcomeDriverNameCollision")
                    write_grader(handle.grader,name,source.read_bytes())
                    handle.supported_grader=grader_relative(artifact["relative_path"])
            for name in ("tmp","home","cargo","rustup","target","cache"):(handle.runtime/name).mkdir(mode=0o700)
            # Copy only source, never baked targets; every distinct stage has a fresh target.
            # Fresh mtimes additionally prevent stale Cargo metadata in any future permitted pilot cache.
            now=time.time_ns()
            for directory,folders,files in os.walk(handle.candidate):
                for name in files:os.utime(Path(directory)/name,ns=(now,now),follow_symlinks=False)
            (handle.candidate/".runtime").mkdir(mode=0o700)
            if self.seeder is not None:self.seeder(handle.runtime)
            if handle.family_v2 is not None:
                require(self.paired is not None,"Version2PairedRoleCatalogRequired")
                self.paired.prepare(handle)
            else:
                from .paired_runtime import readable_tree,runtime_layout
                readable_tree(handle.candidate);readable_tree(handle.grader);runtime_layout(handle.runtime)
            return handle
        except Exception:
            shutil.rmtree(root);raise
    @staticmethod
    def step_command(step):
        return {"argv":step.argv,"cwd":step.cwd,"environment":dict(step.environment,**{
            "PYTHONDONTWRITEBYTECODE":"1","TMPDIR":"/work/.runtime/tmp",
            "TMP":"/work/.runtime/tmp","TEMP":"/work/.runtime/tmp"})}
    @staticmethod
    def family_identity(plan):
        from .paired_runtime import family_identity
        return family_identity(plan)
    @staticmethod
    def source_manifest(root):
        from .paired_runtime import measure_tree
        return measure_tree(root,candidate=True)
    def configuration(self,handle,command,_worker=False,_step=None):
        cwd=command["cwd"];require(isinstance(cwd,str) and (cwd=="/work" or cwd.startswith("/work/")) and ".." not in PurePosixPath(cwd).parts and str(PurePosixPath(cwd))==cwd,"ExactWorkingDirectoryRequired");self.admission.assert_current()
        env=dict(command_environment(command))
        argv=tuple(command["argv"]);rust=argv[0]!="" and (argv[0] in {self.cargo,self.rustc,"/work/scripts/quiet"})
        if handle.paired_role is not None:rust=rust or handle.paired_role.require_rust
        exceptions=tuple(self.api.TrustedSourceFile(path,value) for path,value in self.baseline.exceptions.items() if path not in self.baseline.permits)
        wrappers=()
        if argv[0]=="/work/scripts/quiet":
            wrappers=(self.api.TrustedSourceFile("scripts/quiet",sha(self.baseline.read_raw("scripts/quiet"))),)
        registration=(argv,)
        if handle.family_v2 is not None:
            registration=tuple(dict.fromkeys(step.argv for step in (*handle.family_v2.prerequisite_steps,*handle.family_v2.acceptance_steps)))
            rust=rust or any(row[0] in {self.cargo,self.rustc,"/work/scripts/quiet"} for row in registration)
            if any(row[0]=="/work/scripts/quiet" for row in registration):
                wrappers=(self.api.TrustedSourceFile("scripts/quiet",sha(self.baseline.read_raw("scripts/quiet"))),)
        readonly=False
        if handle.family_v2 is not None and not _worker:
            candidates=handle.family_v2.acceptance_steps
            if _step is not None:
                require(any(_step is step for step in handle.family_v2.prerequisite_steps),"PinnedPrerequisiteStepRequired")
                candidates=(_step,)
            selected=[step for step in candidates
                      if step.argv==argv and step.cwd==cwd and step.environment==tuple(sorted(env.items()))]
            require(selected and len({step.protected_readonly for step in selected})==1,"AmbiguousFrozenStepConfiguration")
            readonly=selected[0].protected_readonly
        return self.api.IsolationConfig(candidate_readonly=readonly,engine=self.engine,endpoint=self.endpoint,image=self.image,owned_root=handle.root,candidate_root=handle.candidate,grader_root=handle.grader,runtime_root=handle.runtime,acceptance_argv=registration,python_executable=self.python,rustc_executable=self.rustc,cargo_executable=self.cargo,require_rust=rust,environment=tuple(sorted(env.items())),uid=os.getuid(),gid=os.getgid(),source_commit=self.baseline.commit,source_archive_sha256=self.baseline.expected,trusted_source_files=exceptions,trusted_wrappers=wrappers,working_directory=cwd,trusted_source_members=tuple(self.api.TrustedSourceMember(path,row["sha256"],row["git_blob"]) for path,row in sorted(self.baseline.permits.items())),**self.execution_options)
    def supports_family(self,artifacts,commands):
        if self.family_recipes is not None:
            resolved=self.family_recipes.resolve(artifacts,commands)
            if resolved is not None:
                require(self.paired is not None and resolved[0].family in self.paired.roles,"MissingPairedFamilyBinding")
                return True
        if self.byte_driver_api is None or len(artifacts)!=1 or len(commands)!=1:return False
        artifact=artifacts[0];argv=tuple(commands[0]["argv"])
        if argv!=(self.python,"/grader/"+grader_relative(artifact["relative_path"]),"/work"):return False
        try:self.byte_driver_api.recognize(artifact["content"],artifact["sha256"])
        except (ValueError,SyntaxError,UnicodeError,AttributeError,IndexError,TypeError):return False
        source=no_links(self.driver_path);require(file_sha(source)==self.driver_sha256,"OutcomeDriverSourceMismatch")
        return True
    def family_plan(self,handle,commands):
        if handle.family_v2 is not None:return handle.family_v2
        if handle.supported_grader is None or len(commands)!=1:return None
        relative_path=grader_relative(handle.supported_grader)
        argv=tuple(commands[0]["argv"])
        if len(argv)!=3 or argv!=(self.python,"/grader/"+relative_path,"/work"):return None
        driver=self.api.TrustedSourceFile("python_byte_outcome_driver.py",self.driver_sha256)
        return self.api.FamilyPlan(family="immutable-python-inert-byte-sha256-v1",prerequisite_argv=(argv,),acceptance_argv=(argv,),driver=driver)
    def prerequisite_stage(self,handle,commands):
        plan=self.family_plan(handle,commands)
        if plan is None:return None
        spec=dict(handle.spec,source_transform=None,source_transform_target=None)
        baseline=self.prepare(spec,{},handle.artifacts,commands);handle.baseline_stage=baseline
        plan=self.family_plan(baseline,commands)
        if getattr(plan,"schema_version",1)==2:
            first=plan.prerequisite_steps[0]
            command={"argv":first.argv,"cwd":first.cwd,"environment":dict(first.environment,**{"PYTHONDONTWRITEBYTECODE":"1","TMPDIR":"/work/.runtime/tmp","TMP":"/work/.runtime/tmp","TEMP":"/work/.runtime/tmp"})}
        else:command=commands[0]
        config=self.configuration(baseline,command,_step=first) if getattr(plan,"schema_version",1)==2 else self.configuration(baseline,command)
        return baseline,config,plan
    def adopt_qualified_runtime(self,handle,baseline_stage,prerequisite):
        """Use issued final runtimes and pristine initial protected source only."""
        from .paired_runtime import copy_qualified_runtime
        require(prerequisite.status=="qualified" and prerequisite.private_path is not None,"QualifiedRuntimeRequired")
        receipt=no_links(prerequisite.private_path);require(file_sha(receipt)==prerequisite.digest,"PrerequisiteReceiptChanged")
        body=private_json(receipt)
        require(body.get("schema_version")==2 and body.get("status")=="qualified","Version2QualifiedRuntimeRequired")
        require(baseline_stage.root.parent==self.root and handle.root.parent==self.root
                and baseline_stage.runtime!=handle.runtime,"OwnedDistinctRuntimeRequired")
        copy_qualified_runtime(baseline_stage.runtime,handle.runtime,body["after"]["runtime"])
        if handle.paired_role is not None:
            require(self.paired is not None,"PairedRuntimeBindingRequired")
            self.paired.adopt(handle,baseline_stage,body)
        # A copied baseline target must rebuild against this stage's source.
        now=time.time_ns()
        roots=(handle.candidate,handle.worker_candidate) if handle.worker_candidate is not None else (handle.candidate,)
        for root in roots:
            for directory,folders,files in os.walk(root,followlinks=False):
                no_links(directory)
                for name in files:os.utime(Path(directory)/name,ns=(now,now),follow_symlinks=False)

    def cache_identity(self,handle,commands,verified_probe,prerequisite):
        require(verified_probe.status=="verified" and verified_probe.private_path is not None,"VerifiedProbeRequired")
        p=no_links(verified_probe.private_path);require(file_sha(p)==verified_probe.digest.removeprefix("sha256:"),"ProbeDigestMismatch")
        body=private_json(p);require(body["status"]=="verified" and body["cleanup_proved"] is True,"ProbeCleanupRequired")
        require(prerequisite.status=="qualified" and re.fullmatch(r"[a-f0-9]{64}",prerequisite.binding_digest),"QualifiedFamilyIdentityRequired")
        config=body["configuration"];sources=config["sources"]
        require(config["image"]==self.image and config["source_archive"]["sha256"]==self.baseline.expected,"ProbeSourceImageMismatch")
        all_sources={"protected":sources};images={"protected":config["image"]};observed={"protected":body["observations"]["observed_probes"]["versions"]}
        policies={"protected":{key:config[key] for key in ("environment","limits","user","runtimes","candidate_readonly","working_directory","supervisor","probe","outcome_controller","executor")}}
        if handle.paired_role is not None:
            worker=handle.family_v2.worker
            current=replace(worker.config,environment=command_environment(commands[0]),working_directory=config["working_directory"])
            # Every cache reuse performs real current containment for BOTH roles.
            proof=self.api.probe(current)
            require(proof.status=="verified" and proof.private_path is not None,"WorkerProbeRequiredForCache")
            path=no_links(proof.private_path);require(file_sha(path)==proof.digest,"WorkerProbeDigestMismatch")
            worker_body=private_json(path);require(worker_body["cleanup_proved"] is True,"WorkerCacheCleanupRequired")
            worker_config=worker_body["configuration"];all_sources["worker"]=worker_config["sources"];images["worker"]=worker_config["image"]
            observed["worker"]=worker_body["observations"]["observed_probes"]["versions"]
            policies["worker"]={key:worker_config[key] for key in ("environment","limits","user","runtimes","candidate_readonly","working_directory","supervisor","probe","outcome_controller","executor")}
            handle.worker_verification_digest=proof.digest
        source_identity={key:value["candidate"] for key,value in all_sources.items()}
        grader_identity={key:value["grader"] for key,value in all_sources.items()}
        runtime_identity={key:{field:value[field] for field in ("runtime","caches")} for key,value in all_sources.items()}
        return {"source_bytes_modes_sha256":sha(canonical(source_identity)),"grader_bytes_modes_sha256":sha(canonical(grader_identity)),
            "image_sha256":sha(canonical(images)),"toolchain_runtime_dependencies_sha256":sha(canonical({"sources":runtime_identity,"observed":observed,"qualified_family_binding":prerequisite.binding_digest})),
            "environment_policy_sha256":sha(canonical({"roles":policies,"commands":commands,"family":self.family_identity(self.family_plan(handle,commands)),"runtime_bindings":file_sha(Path(__file__)),"paired_runtime":file_sha(Path(__file__).with_name("paired_runtime.py"))}))}
    def close(self,handle):
        # Evidence stays durable/private. Isolation receipts prove container cleanup.
        require(handle.root.parent==self.root and not handle.root.is_symlink(),"UnownedStage")
        if handle.closed:return
        if handle.baseline_stage is not None:self.close(handle.baseline_stage)
        durable(handle.root/"stage-closed.json",{"closed":True,"candidate_retained_private":True})
        handle.closed=True

    def prepare_preflight(self):
        spec={"source_commit":self.baseline.commit,"source_tree":self.baseline.tree,"source_projections":{},"source_transform":None,"source_transform_target":None,"allowed_edit_paths":[]}
        handle=self.prepare(spec,{},())
        command={"argv":(self.cargo,"--version"),"cwd":"/work","environment":{"PYTHONDONTWRITEBYTECODE":"1","TMPDIR":"/work/.runtime/tmp","TMP":"/work/.runtime/tmp","TEMP":"/work/.runtime/tmp"}}
        return handle,self.configuration(handle,command)

class FrozenGraphLoader:
    """Baseline source plus four hash-pinned curated data files, never live main."""
    RECORDS=frozenset(("validation/knowledge/records.toml","validation/knowledge/coverage.toml","validation/knowledge/conflicts.toml","validation/knowledge/pilot.json"))
    def __init__(self,*,baseline,owned_root,model_api,admission,review_check):
        self.baseline=baseline;self.root=no_links(owned_root);self.model=model_api;self.admission=admission;self.review=review_check
    def load(self,curated_files,*,expected_graph_sha256):
        self.admission.assert_current();require(self.review() is True,"GraphLoaderNotReviewed")
        require(set(curated_files)==self.RECORDS,"ExactCuratedDataSetRequired")
        destination=self.root/("graph-"+uuid.uuid4().hex);destination.mkdir(mode=0o700);self.baseline.materialize(destination)
        for path,entry in curated_files.items():
            source=no_links(entry["path"]);require(file_sha(source)==entry["sha256"],"CuratedDataDigestMismatch")
            p=destination/str(relative(path));p.parent.mkdir(parents=True,exist_ok=True,mode=0o755)
            require(not p.is_symlink(),"CuratedDataSymlink");p.write_bytes(source.read_bytes());os.chmod(p,0o600)
        graph=self.model.load_graph(destination,revision=self.baseline.commit)
        require(sha(graph.to_json().encode())==expected_graph_sha256.removeprefix("sha256:"),"FrozenKnowledgeGraphDigestMismatch")
        return graph


def prepare_acceptance(*,config,graph,review_gate,admission,stager,adapter_api,cache_directory):
    """Explicit reviewed supervisor hook; never called at import or preparation."""
    review_gate.verify(config["reviewed_source_hashes"],config["review_receipt"])
    verify_graph(graph,config["manifest"])
    admission.assert_current()
    handle,preflight=stager.prepare_preflight()
    cache=adapter_api.ValidatedAcceptanceCache(cache_directory)
    runner=adapter_api.IsolatedAcceptance(isolation_api=stager.api,stager=stager,preflight_config=preflight,source_commit=stager.baseline.commit,source_tree=stager.baseline.tree,cache=cache)
    # Keep the private preflight stage alive for the real security/runtime probe.
    runner.private_preflight_stage=handle
    return runner
