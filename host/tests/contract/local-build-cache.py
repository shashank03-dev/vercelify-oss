#!/usr/bin/env python3
"""Opt-in Docker contract: fresh build variables plus persistent package caches."""
import json
import os
from pathlib import Path
import subprocess
import tempfile

pack=os.environ['VFY_PACK_BIN']
with tempfile.TemporaryDirectory(prefix='vfy-local-cache-') as directory:
    source=Path(directory)
    (source/'package.json').write_text(json.dumps({'name':'cache-fixture','version':'1.0.0','scripts':{'start':'node server.js'}}))
    (source/'server.js').write_text('require("http").createServer((q,s)=>s.end("ok")).listen(3000)')
    config={'pack':'node','install_command':'if test -f /root/.npm/vfy-probe; then cp /root/.npm/vfy-probe /app/cache-was-present; fi; printf kept > /root/.npm/vfy-probe','build_command':'printf "%s" "$PUBLIC_VALUE" > /app/build-value'}
    for generation,value in [(1,'one'),(2,'two')]:
        request={'build':config,'variables':[{'key':'PUBLIC_VALUE','value':value,'secret':False}],'app_name':'nonce-fixture','deployment_id':f'dpl_nonce{generation}','platform':'linux/arm64'}
        built=subprocess.run([pack,'build',str(source)],input=json.dumps(request),capture_output=True,text=True,timeout=120)
        assert built.returncode==0,built.stderr
        image=json.loads(built.stdout)['image']
        actual=subprocess.check_output(['docker','run','--rm','--entrypoint','cat',image,'/app/build-value'],text=True)
        assert actual==value,(actual,value)
        if generation==2:
            cached=subprocess.run(['docker','run','--rm','--entrypoint','cat',image,'/app/cache-was-present'],capture_output=True,text=True)
            assert cached.returncode==0 and cached.stdout=='kept','fresh build lost the package cache'
    print('PASS: generated builds observe new variable values and retain package caches')
