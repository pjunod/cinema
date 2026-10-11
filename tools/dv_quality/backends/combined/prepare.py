"""New scratch only; exact retained dependency inventory; no source-tree writes."""
import hashlib,json,os,shutil,sys
from pathlib import Path

def inventory(root):
    rows={}
    for base in ['prefix','include','lib','ffmpeg-prefix','rpu-tags']:
        for p in sorted((root/base).rglob('*')):
            name=str(p.relative_to(root))
            if p.is_symlink():rows[name]={'link':os.readlink(p)}
            elif p.is_file():rows[name]={'sha256':hashlib.sha256(p.read_bytes()).hexdigest(),'bytes':p.stat().st_size}
    return rows

def prepare(source,destination):
    source=Path(source).resolve();destination=Path(destination).absolute()
    if destination.exists() or destination.is_symlink():raise ValueError('fresh destination required')
    if destination==source or source in destination.parents:raise ValueError('outside source tree required')
    lock=json.loads((source/'dependency-lock.json').read_text())
    if inventory(source)!=lock:raise ValueError('dependency inventory changed')
    destination.mkdir()
    for n in ['prefix','include','lib','ffmpeg-prefix','rpu-tags']:shutil.copytree(source/n,destination/n,symlinks=True)
    if inventory(destination)!=lock:raise ValueError('post-copy dependency inventory changed')
    for n in ['image-id.txt','source.mkv','source-definition.json','bl-b2.mkv','el-b2.mkv','bl-b2.ffprobe.json','el-b2.ffprobe.json','check_association.py','run_combined.py','decode_layers','fel_export','mux_rgb','expected-inputs.json','encoder-dependency-identity.json','limits.json','limits-selection.json','check_combined.py','stage_contract.py','build-identity.json','prior-preregistered-controls.json','dependency-lock.json']:
        shutil.copy2(source/n,destination/n)
    shutil.copy2(source/'runner/target/release/dv-m2-owned-runner',destination/'owned-runner')
    (destination/'b2-normal/frames').mkdir(parents=True);shutil.copy2(destination/'source.mkv',destination/'b2-normal/compound.mkv')
if __name__=='__main__':prepare(*sys.argv[1:])
