// Offscreen HTML worker for the ⌘X diagram viewer. JSON lines in/out; never opens a browser window.
// Embedded in the nebula binary and written to the runtime dir at viewer start, so nothing here may
// depend on where this file lives: Playwright is looked up at runtime (see findPlaywright).
const { pathToFileURL } = require('node:url');
const { createRequire } = require('node:module');
const readline = require('node:readline');
const path = require('node:path');
const fs = require('node:fs');
let source = process.argv[2];
let browser, page, closing=false;
const emit = value => process.stdout.write(JSON.stringify(value)+'\n');
async function close(){if(closing)return;closing=true;setTimeout(()=>process.exit(0),1000).unref();await browser?.close().catch(()=>{});process.exit(0)}
process.on('SIGTERM',close);process.on('SIGINT',close);
process.stdout.on('error',close);
// Playwright is a runtime concern: NEBULA_PLAYWRIGHT_DIR (a directory holding node_modules), then the
// diagram's checkout (and its web/app, where this repo keeps it), the cwd, NODE_PATH and the global
// node_modules next to this node binary. Any of playwright-core / playwright / @playwright/test will do.
function findPlaywright(){
 const checkout=path.resolve(path.dirname(source),'..','..');
 const roots=[process.env.NEBULA_PLAYWRIGHT_DIR,checkout,path.join(checkout,'web','app'),process.cwd(),
  path.resolve(path.dirname(process.execPath),'..','lib')].filter(Boolean);
 for(const root of roots){
  const from=createRequire(path.join(root,'noop.js'));
  for(const name of ['playwright-core','playwright','@playwright/test']){
   try{const chromium=from(name).chromium;if(chromium)return chromium}catch{}
  }
 }
 return null;
}
// Input is queued by us, not by readline, and from before the browser is up: `close` never waits behind a backlog, and
// a flood of pointer events costs one step instead of one vsync (~16.7 ms) each.
const queue=[];let ended=false,pumping=false,ready=false;
const isMouse=(m,action)=>m&&m.type==='mouse'&&m.action===action;
// Next message, with the run it heads folded in: consecutive moves collapse to the last
// one, adjacent wheels sum. Order against down/up/key/text/resize never changes, so a
// drag is still down → (latest) move → up.
function next(){
 const m=queue.shift();
 if(isMouse(m,'move')){let last=m;while(isMouse(queue[0],'move'))last=queue.shift();return last}
 if(isMouse(m,'wheel'))while(isMouse(queue[0],'wheel')){const w=queue.shift();m.dx=(m.dx||0)+(w.dx||0);m.dy=(m.dy||0)+(w.dy||0);m.x=w.x;m.y=w.y}
 return m;
}
// Local diagram files only; `source` is what a decision is attributed to.
async function open(file){
 if(!fs.statSync(file).isFile())throw Error('HTML file not found');
 source=file;
 await page.goto(pathToFileURL(file).href,{waitUntil:'load',timeout:10000});
}
async function step(m){
 // `reply` marks the error that stands in for the frame a request was owed.
 const owesFrame=m.type==='resize'||m.type==='frame'||m.type==='goto';
 try{
  if(m.type==='resize')await page.setViewportSize({width:m.width,height:m.height});
  if(m.type==='mouse'){
   await page.mouse.move(m.x,m.y);
   if(m.action==='down')await page.mouse.down({button:m.button});
   if(m.action==='up')await page.mouse.up({button:m.button});
   if(m.action==='wheel')await page.mouse.wheel(m.dx||0,m.dy||0);
  }
  if(m.type==='key')await page.keyboard.press(m.key);
  if(m.type==='text')await page.keyboard.insertText(m.text);
  if(m.type==='reload')await page.reload({waitUntil:'load',timeout:10000});
  // Switching diagrams reuses this page: no new browser, no new worker.
  if(m.type==='goto')await open(m.path);
  if(owesFrame){
   const image=await page.screenshot({type:'png',timeout:5000});
   emit({png:image.toString('base64'),width:page.viewportSize().width,height:page.viewportSize().height});
  }
 }catch(e){emit({error:e.message.split('\n')[0].slice(0,240),...(owesFrame?{reply:true}:{})})}
}
async function pump(){
 if(pumping||!ready)return;pumping=true;
 while(queue.length&&!closing)await step(next());
 pumping=false;
 if(ended)await close();
}
const lines=readline.createInterface({input:process.stdin});
lines.on('line',line=>{
 let m;try{m=JSON.parse(line)}catch{return}
 if(m.type==='close')return close();
 queue.push(m);pump();
});
lines.on('close',()=>{
 ended=true;
 // Stdin is gone, so is our owner: a hung await must not keep us (and Chrome) alive.
 setTimeout(()=>process.exit(0),1500).unref();
 pump();
});
const cap=(value,max)=>typeof value==='string'?value.slice(0,max):'';
(async()=>{
 const chromium=findPlaywright();
 if(!chromium)throw Error('Playwright not found — `npm i -g playwright-core`, or point NEBULA_PLAYWRIGHT_DIR at a directory that has it');
 // The user's Chrome when there is one, else Playwright's own Chromium.
 browser=await chromium.launch({channel:'chrome',headless:true}).catch(()=>chromium.launch({headless:true}));
 const context=await browser.newContext({viewport:{width:1200,height:800},colorScheme:'dark',deviceScaleFactor:1,acceptDownloads:false});
 // The page's one way out: window.nebula.decide({id,choice,label,note}) becomes one `decision` line of
 // capped strings, attributed by us (not by the page) to the file being shown. Top frame only.
 await context.exposeBinding('__nebulaDecide',(origin,d)=>{
  if(origin.frame!==origin.page.mainFrame()||!d||typeof d!=='object')return false;
  emit({type:'decision',source,id:cap(d.id,64),choice:cap(d.choice,64),label:cap(d.label,200),note:cap(d.note,2000)});
  return true;
 });
 // Plain assignment: the templates' browser stub is `window.nebula = window.nebula || {…}`, also under "use strict".
 await context.addInitScript(()=>{window.nebula={decide:d=>window.__nebulaDecide(d)}});
 page=await context.newPage();
 // Standalone local documents only. No remote pages or external subresources.
 await context.route('**/*',route=>{const u=new URL(route.request().url());return ['file:','data:','blob:','about:'].includes(u.protocol)?route.continue():route.abort()});
 page.on('popup',p=>p.close());
 page.on('dialog',d=>d.dismiss());
 page.on('pageerror',e=>emit({error:e.message.split('\n')[0].slice(0,240)}));
 // Never use a browser profile.
 await open(source);
 ready=true;pump();
})().catch(async e=>{emit({error:e.message.split('\n')[0].slice(0,240),fatal:true});await close()});
