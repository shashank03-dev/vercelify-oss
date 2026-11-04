import concurrent.futures,subprocess,time,json,datetime,socket
out='docs/validation/2026-11-04-attended/monitor.jsonl'
def probe(item):
 name,url,host=item
 args=['curl','--noproxy','*','-s','-o','/dev/null','-w','%{http_code}','--max-time','2']
 if host:args+=['-H','Host: '+host]
 p=subprocess.run(args+[url],capture_output=True,text=True)
 return name,{'code':p.stdout,'exit':p.returncode}
def tcp():
 try:
  with socket.create_connection(('100.123.113.77',22),timeout=2):return True
 except OSError:return False
with open(out,'a',buffering=1) as f,concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
 for i in range(1800):
  t=time.monotonic(); futures=[pool.submit(probe,x) for x in [('preview','http://pr-1-gw.user-inspiron-5567.vfy.localhost:7481/',''),('production','http://gw.10.77.0.1.sslip.io.localhost:7481/','')]];ssh=pool.submit(tcp)
  r={'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'ssh_tcp':ssh.result(),**dict(x.result() for x in futures)};f.write(json.dumps(r)+'\n')
  if i%15==0:print(json.dumps(r),flush=True)
  time.sleep(max(0,1-(time.monotonic()-t)))
