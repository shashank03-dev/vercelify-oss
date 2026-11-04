from pathlib import Path
import concurrent.futures,time,json
s=Path('target/guest-public-qualification/qualify.py').read_text().split("inv=issue();x=cloud('probe',inv)")[0].replace('cloud-access.json','cloud-access-stream.json');exec(s)
inv=issue();cookie=redeem(inv)
with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
 future=pool.submit(cloud,'watch-stream',inv,cookie);time.sleep(4);at=time.monotonic()
 inner("h['api']('DELETE','/platform/guest-links/'+"+repr(inv['grant']['id'])+")")
 result=future.result();elapsed=time.monotonic()-at
assert result.get('first_chunk') and result.get('closed') and not result.get('timeout'),result
assert cloud('probe',inv,cookie)['root']['status']==401
row={'case':'external-in-progress-HTTP-stream-revocation','passed':True,'close_observed_after_revoke_call_seconds':round(elapsed,3),'stream_total_ms':result['ms']};(out/'stream-revocation.json').write_text(json.dumps(row,indent=2)+'\n');print(json.dumps(row),flush=True)
