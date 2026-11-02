#!/usr/bin/env python3
"""Read-only local-agent discovery and proxy probes; never prints credentials/body."""
import http.client
import json
from pathlib import Path
import urllib.parse
import urllib.request

token = Path('/etc/vercelify/agent.token').read_text().strip()
request = urllib.request.Request('http://127.0.0.1:7420/v1/platform/apps', headers={'Authorization': 'Bearer ' + token})
apps = json.load(urllib.request.urlopen(request, timeout=10))
for app in apps:
    if not app.get('current_deployment_id') or not app.get('url'):
        continue
    host = urllib.parse.urlsplit(app['url']).netloc
    connection = http.client.HTTPConnection('127.0.0.1', 80, timeout=10)
    connection.request('GET', '/', headers={'Host': host})
    response = connection.getresponse()
    print(json.dumps({'app': app['name'], 'host': host, 'status': response.status, 'bytes': len(response.read())}))
    assert 200 <= response.status < 400, app['name']
    connection.close()
