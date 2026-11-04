import subprocess,pathlib
r=pathlib.Path(__file__).parent
subprocess.run(['python3',str(r/'object.py'),'corrupt'],check=True)
try:
 code=pathlib.Path('target/r2-qualification/recover.py').read_text().split("run('live-R2-separate-ARM-recovery'")[0]+"\nrun('R2-ciphertext-corruption',artifact,'failed')\n"
 p=subprocess.run(['limactl','shell','vfybench','sudo','docker','exec','-i','vfy-final-recovery-docker','python3','-'],input=code,capture_output=True,text=True,timeout=300)
 pathlib.Path('docs/validation/2026-11-04-r2-qualification/corruption.txt').write_text(p.stdout+p.stderr);assert p.returncode==0,p.stderr[:600]
 print(p.stdout)
finally:subprocess.run(['python3',str(r/'object.py'),'repair'],check=True)
