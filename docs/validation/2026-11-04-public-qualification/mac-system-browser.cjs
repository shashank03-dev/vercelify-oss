const { chromium }=require('../../../cockpit/node_modules/playwright');
const fs=require('node:fs');
(async()=>{const browser=await chromium.launch({headless:true});const results=[];
try{const page=await browser.newPage();for(const url of ['http://pr-1-gw.user-inspiron-5567.vfy/','http://gw.10.77.0.1.sslip.io/']){const response=await page.goto(url,{waitUntil:'domcontentloaded',timeout:30000});if(response.status()!==200)throw new Error('Unexpected status '+response.status());results.push({url,status:response.status(),body_bytes:(await page.content()).length});}fs.writeFileSync(process.argv[2],JSON.stringify(results,null,2)+'\n');}finally{await browser.close();}})().catch(e=>{console.error(e.message);process.exitCode=1;});
