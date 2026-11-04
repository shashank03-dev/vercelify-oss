// Real browser against the owned lab guest gateway. Never log invitation URLs.
const { chromium } = require('../../../cockpit/node_modules/playwright');
const { execFileSync } = require('node:child_process');
const assert = require('node:assert/strict');
function inner(args) {return execFileSync('limactl',['shell','vfybench','--','sudo','docker','exec','vfy-m1-contract-docker',...args],{encoding:'utf8'});}
const facts=JSON.parse(inner(['cat','/var/lib/vfy-guest-contract/facts.json']));
const invitation=facts.browser_invitation;
function hits(){return Number(inner(['docker','exec',facts.preview_container,'cat','/tmp/guest-hits']).trim());}
(async()=>{
 const browser=await chromium.launch({headless:true,args:['--no-proxy-server','--host-resolver-rules=MAP *.guests.example.test 127.0.0.1']});
 try {
  const context=await browser.newContext({ignoreHTTPSErrors:true});const page=await context.newPage();
  const before=hits();await page.goto(invitation.url);await page.waitForFunction(()=>!location.hash);
  assert(!page.url().includes('#'));assert.equal(hits(),before,'scanner navigation must not hit the application');
  await page.getByRole('button',{name:'Open preview'}).click();await page.getByRole('heading',{name:'Guest fixture'}).waitFor();
  const cookies=await context.cookies();const session=cookies.find(c=>c.name==='__Host-vfy_guest');
  assert(session&&session.secure&&session.httpOnly&&session.sameSite==='Lax'&&!session.domain.startsWith('.'));
  assert(session.expires-Date.now()/1000<=901);
  assert(await page.evaluate(()=>isSecureContext));
  assert(await page.evaluate(async()=>{try{await navigator.serviceWorker.register('/sw.js');return false;}catch{return true;}}),'guest workers must be refused');
  const next=hits();await page.goto(invitation.url);await page.waitForFunction(()=>!location.hash);assert.equal(hits(),next);
  await page.getByRole('button',{name:'Open preview'}).click();await page.getByRole('heading',{name:'Guest fixture'}).waitFor();
  const outsider=await browser.newContext({ignoreHTTPSErrors:true});const other=await outsider.newPage();await other.goto(invitation.url);await other.getByRole('button',{name:'Open preview'}).click();
  await other.getByText('This invitation is expired, used or unavailable. Ask its owner for another.').waitFor();
  console.log('PASS: external Chromium scanner GET never reaches app; fragment clears before confirmation; host-only Secure/HttpOnly cookie; worker denied; existing session safely reopens; separate browser cannot reuse single-session invitation');
 } finally {await browser.close();}
})().catch(()=>{console.error('FAIL: guest browser assertion (secret URLs suppressed)');process.exitCode=1;});
