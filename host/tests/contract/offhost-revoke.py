#!/usr/bin/env python3
"""In-target race fixture. Credentials stay in this disposable target's files."""
import http.client
import json
import os
from pathlib import Path
import sys
import time

WORK=Path('/tmp/vfy-offhost-contract')
ADMIN='offhost-contract-only'
def call(method,path,body=None,secret=ADMIN):
    conn=http.client.HTTPConnection('127.0.0.1',17425,timeout=15)
    headers={'Authorization':'Bearer '+secret,'Content-Type':'application/json'}
    conn.request(method,path,json.dumps(body).encode() if body is not None else None,headers)
    response=conn.getresponse(); data=response.read();status=response.status;conn.close()
    assert 200<=status<300,(path,status,data)
    return json.loads(data)

mode=sys.argv[1]
if mode=='create':
    app,platform,planfile=sys.argv[2:]
    token=call('POST','/v1/tokens',{'name':'offhost-race','role':'deployer'})
    job=call('POST','/v1/platform/apps/'+app+'/build-jobs',{'commit_sha':'3'*40,'platform':platform,'plan':json.loads(Path(planfile).read_text())},token['secret'])
    with os.fdopen(os.open(WORK/'device.json',os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o600),'w') as f:
        json.dump({'token':token,'job':job},f)
    print(json.dumps(job))
else:
    archive,digest=sys.argv[2:]
    fixture=json.loads((WORK/'device.json').read_text())
    job=fixture['job']['job']; token=fixture['token']; id=job['deployment_id']
    raw=Path(archive).read_bytes()
    conn=http.client.HTTPConnection('127.0.0.1',17425,timeout=30)
    conn.putrequest('PUT',f'/v1/platform/build-jobs/{id}/image?generation=1&digest={digest}')
    conn.putheader('Authorization','Bearer '+token['secret'])
    conn.putheader('Content-Type','application/x-tar')
    conn.putheader('Content-Length',str(len(raw)));conn.endheaders();conn.send(raw[:1024])
    for _ in range(30):
        if call('GET','/v1/platform/build-jobs/'+id)['phase']=='uploading':break
        time.sleep(.1)
    else:raise AssertionError('sender did not enter upload phase')
    call('DELETE','/v1/tokens/'+token['token']['id'])
    conn.send(raw[1024:]);response=conn.getresponse();response.read()
    assert response.status==409,response.status
    d=call('GET','/v1/platform/deployments/'+id)
    assert d['status']=='failed',d['status']
    print('PASS: revoking the device during a complete valid upload fences publication')
