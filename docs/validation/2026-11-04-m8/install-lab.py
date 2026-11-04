"""Install M8 on the owned lab VM; preserve agent/SQLite rollback state.

Run from repository root after qualification. Never used on the physical laptop.
"""
import subprocess

remote = r'''
import datetime, hashlib, json, os, shutil, sqlite3, subprocess, time, urllib.request
from pathlib import Path
candidate=Path('/Users/shashank/Desktop/VERCELIFY/target/aarch64-unknown-linux-musl/release/vercelify-agent')
binary=Path('/usr/local/bin/vercelify-agent')
database=Path('/var/lib/vercelify/platform.db')
backup=Path('/var/lib/vercelify/validation-backups')/('m8-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'))
backup.mkdir(parents=True,mode=0o700)
os.chmod(backup.parent,0o700)
shutil.copy2(binary,backup/'vercelify-agent')
def service(action): subprocess.run(['systemctl',action,'vercelify-agent'],check=True)
service('stop')
try:
    with sqlite3.connect(database) as src, sqlite3.connect(backup/'platform.db') as dst: src.backup(dst)
    os.chmod(backup/'platform.db',0o600)
    temporary=binary.with_name('vercelify-agent.validation-new')
    shutil.copyfile(candidate,temporary)
    os.chmod(temporary,0o755)
    os.replace(temporary,binary)
    guard=Path('/usr/local/lib/vercelify/power-guard.sh')
    helper=guard.parent/'lib/battery-policy.sh'
    for path,source in [(guard,'host/power-guard.sh'),(helper,'host/lib/battery-policy.sh')]:
        path.parent.mkdir(parents=True,exist_ok=True)
        if path.exists(): shutil.copy2(path,backup/path.name)
        shutil.copyfile(Path('/Users/shashank/Desktop/VERCELIFY')/source,path)
        os.chmod(path,0o755 if path==guard else 0o644)
    # This lab was installed without the power-guard service; do not enable it.
    service('start')
    for attempt in range(60):
        try:
            health=json.load(urllib.request.urlopen('http://127.0.0.1:7420/v1/health',timeout=2))
            with sqlite3.connect(database) as db: version=db.execute('PRAGMA user_version').fetchone()[0]
            if 'preview-guest-links' in health.get('capabilities',[]) and version==13:
                print(json.dumps({'ready':True,'schema':version,'backup':str(backup),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest()}))
                break
        except (OSError,ValueError,sqlite3.Error): pass
        time.sleep(1)
    else: raise RuntimeError('M8 candidate did not become ready')
except BaseException:
    service('stop')
    shutil.copy2(backup/'vercelify-agent',binary)
    if (backup/'platform.db').exists():
        for suffix in ['-wal','-shm']: database.with_name(database.name+suffix).unlink(missing_ok=True)
        shutil.copy2(backup/'platform.db',database)
    service('start')
    raise
'''
subprocess.run(['limactl','shell','vfybench','--','sudo','python3','-'],input=remote,text=True,check=True)
