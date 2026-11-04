const { chromium } = require('../../../cockpit/node_modules/playwright');
const fs=require('node:fs');
(async()=>{const browser=await chromium.launch({headless:true});const rows=[];
try{const page=await browser.newPage();for(const url of ['http://gw.10.77.0.1.sslip.io.localhost:7481/','http://pr-1-gw.user-inspiron-5567.vfy.localhost:7481/','http://pr-1-gw.user-inspiron-5567.vfy/']){
try{const r=await page.goto(url,{waitUntil:'domcontentloaded',timeout:20000});rows.push({url,status:r.status(),body_bytes:(await page.content()).length});}catch(e){rows.push({url,error:String(e).split('\n')[0]});}}
fs.writeFileSync(__dirname+'/mac-browser.json',JSON.stringify(rows,null,2)+'\n');console.log(JSON.stringify(rows));if(rows[0].status!==200||rows[1].status!==200)process.exitCode=1;
}finally{await browser.close();}})().catch(e=>{console.error(e);process.exitCode=1;});
