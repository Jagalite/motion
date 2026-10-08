"""Install one matching Demuxe package without modifying the source checkout."""
import argparse, hashlib, json, pathlib, tarfile, shutil

parser=argparse.ArgumentParser()
parser.add_argument('archive',type=pathlib.Path)
parser.add_argument('--output',type=pathlib.Path,default=pathlib.Path('web/vendor/demuxe'))
args=parser.parse_args()
if args.output.exists(): raise SystemExit('Output already exists; use a new versioned destination')
with tarfile.open(args.archive,'r:gz') as archive:
    members=archive.getmembers()
    for m in members:
        p=pathlib.PurePosixPath(m.name)
        if not p.parts or p.parts[0]!='package' or '..' in p.parts or p.is_absolute() or not (m.isfile() or m.isdir()): raise SystemExit('Unsafe package member')
    if sum(m.size for m in members)>2*1024**3: raise SystemExit('Package too large')
    package=json.load(archive.extractfile('package/package.json'))
    if package.get('name')!='demuxe':raise SystemExit('Not a Demuxe package')
    args.output.mkdir(parents=True)
    for m in members:
        target=args.output.joinpath(*pathlib.PurePosixPath(m.name).parts[1:])
        if m.isdir():target.mkdir(parents=True,exist_ok=True)
        else:
            target.parent.mkdir(parents=True,exist_ok=True)
            with archive.extractfile(m) as source,target.open('wb') as dest:shutil.copyfileobj(source,dest)
receipt={'name':package['name'],'version':package['version'],'archive_sha256':hashlib.file_digest(args.archive.open('rb'),'sha256').hexdigest(),'files':{str(p.relative_to(args.output)):hashlib.file_digest(p.open('rb'),'sha256').hexdigest() for p in sorted(args.output.rglob('*')) if p.is_file()}}
# The core's browser providers are implemented in the package itself. Optional
# Wasm/codec providers require Demuxe's official deployment/bundling tool.
# This explicit browser-only deployment matches deploy-providers.py with no providers.
build_flags=(args.output/'web/generated/internal/provider-build.js').read_text()
if 'providerDeploymentEnabled = true' in build_flags or 'providerDeploymentEnabled=true' in build_flags:
    providers=[{'id':identifier,'implementationIdentity':'demuxe-browser-v1','technology':'browser-native','delivery':['browser','application-bundle'],'applicationBuild':'demuxe-'+package['version'],'offers':[{'capability':capability,'version':1,'profile':profile}]} for identifier,capability,profile in [('browser-original','media.present.original','selected-source'),('browser-prepared','media.present.prepared','selected-streams'),('web-audio-gain','audio.gain','scalar')]]
    encoded=lambda v:(json.dumps(v,sort_keys=True,indent=2)+'\n').encode()
    deployment={'schema':1,'providerContractVersion':1,'revision':'sha256:'+hashlib.sha256(encoded({'providers':providers,'assets':[]})).hexdigest(),'providers':providers,'assets':[]}
    manifest=encoded(deployment)
    (args.output/'demuxe-providers.json').write_bytes(manifest)
    receipt['deployment']='browser-only'
    receipt['files']['demuxe-providers.json']=hashlib.sha256(manifest).hexdigest()
else:
    receipt['deployment']='upstream-bundled'
(args.output/'playscale-package.json').write_text(json.dumps(receipt,indent=2)+'\n')
print(json.dumps({k:v for k,v in receipt.items() if k!='files'}))
