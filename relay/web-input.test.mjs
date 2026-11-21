import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
const source=readFileSync(new URL('./web/app.js',import.meta.url),'utf8');
function fn(name){const start=source.indexOf('  function '+name+'(');assert.ok(start>=0,name);return source.slice(start,source.indexOf('\n  }',start)+4);}
function element(){const classes=new Set();return {value:'',attrs:{},style:{},classList:{add:v=>classes.add(v),remove:v=>classes.delete(v),toggle(v,on){on?classes.add(v):classes.delete(v);},contains:v=>classes.has(v)},setAttribute(k,v){this.attrs[k]=String(v);},getAttribute(k){return this.attrs[k]??null;},replaceChildren(){},select(){},focus(){}};}
function fixture(){const nodes=new Map(),saved=new Map(),root=new Set(),calls=[];const c=vm.createContext({S:{enterSends:null,view:'settings'},coarsePointer:{matches:true},prefs:{set:(k,v)=>saved.set(k,v)},$:id=>{if(!nodes.has(id))nodes.set(id,element());return nodes.get(id);},document:{documentElement:{toggleAttribute:(k,v)=>v?root.add(k):root.delete(k)}},renderComposer(){},calls,focusBefore:{},rememberFocus(){},restoreFocus(){},openDialog(){},activeDialog:()=>null});vm.runInContext(['inputTouchMode','syncInputTouchMode','composerEnterSends','updateEnterSendsControl','setEnterSends','configureAskInput','ask','showNextAsk','closeAsk','showAskError'].map(fn).join('\n')+'\n'+source.slice(source.indexOf('  let askResolve'),source.indexOf('  /** Promise-based prompt')),c);return{c,saved,root};}

test('Enter preference defaults track primary pointer until an explicit device choice is saved',()=>{
 const{c,saved,root}=fixture();assert.equal(c.composerEnterSends(),false);c.syncInputTouchMode();assert.ok(root.has('data-input-touch'));
 c.coarsePointer.matches=false;c.syncInputTouchMode();assert.equal(c.composerEnterSends(),true);assert.ok(!root.has('data-input-touch'));
 c.setEnterSends(false);assert.equal(saved.get('enterSends'),false);assert.equal(c.composerEnterSends(),false);
 const control=c.$('enter-sends');assert.equal(control.checked,false);c.setEnterSends(true);assert.equal(control.checked,true);assert.equal(c.$('enter-sends'),control);
 c.coarsePointer.matches=true;assert.equal(c.composerEnterSends(),true);c.setEnterSends(null);assert.equal(saved.get('enterSends'),null);assert.equal(c.composerEnterSends(),false);
 for(const bad of ['true',{},0]){c.S.enterSends=bad;assert.equal(c.composerEnterSends(),false);}
});

test('prose, machine, URL and legacy secret prompts reset all input metadata',()=>{
 const{c}=fixture(),input=element();
 for(const[kind,expected]of[['prose',['text','true','sentences','on','text','on']],['machine',['text','false','none','off','text','off']],['url',['text','false','none','off','url','url']],['secret',['password','false','none','off','text','off']],['prose',['text','true','sentences','on','text','on']]]){
  c.configureAskInput(input,{inputKind:kind});assert.deepEqual([input.type,...['spellcheck','autocapitalize','autocorrect','inputmode','autocomplete'].map(k=>input.getAttribute(k))],expected);
 }
 c.configureAskInput(input,{secret:true,inputKind:'prose'});assert.equal(input.type,'password');assert.equal(input.getAttribute('autocomplete'),'off');
 c.configureAskInput(input,{});assert.equal(input.getAttribute('spellcheck'),'true');
});

test('close captures exact prompt result and clears hidden DOM before the next prompt',async()=>{
 const{c}=fixture();const first=c.ask({title:'Secret',input:true,secret:true});c.$('ask-input').value=' Exact secret value ';
 const next=c.ask({title:'Name',input:true,inputKind:'prose'});c.closeAsk(c.$('ask-input').value);
 assert.equal(await first,' Exact secret value ');assert.equal(c.$('ask-input').value,'');assert.equal(c.$('ask-input').getAttribute('autocorrect'),'on');
 c.$('ask-input').value='Untouched prose';c.closeAsk(c.$('ask-input').value);assert.equal(await next,'Untouched prose');assert.equal(c.$('ask-input').value,'');
 const confirmation=c.ask({title:'Confirm'});assert.equal(c.$('ask-input').value,'');c.closeAsk(true);assert.equal(await confirmation,true);
});

test('Enter dispatch keeps native newline, queue, IME and repeat semantics independent of pointer preference',()=>{
 const{c}=fixture();let handler,palettes=0;const sends=[];c.$('input').addEventListener=(_,f)=>handler=f;c.paletteKey=()=>{palettes++;return true;};c.mentionKey=()=>false;c.submit=queue=>sends.push(queue);c.showHelp=()=>{};
 const start=source.indexOf('  $("input").addEventListener("keydown",');vm.runInContext(source.slice(start,source.indexOf('\n  });',start)+6),c);
 const event=extra=>({key:'Enter',prevented:false,preventDefault(){this.prevented=true;},...extra});
 for(const enabled of[false,true]){
  c.S.enterSends=enabled;let e=event({shiftKey:true});handler(e);assert.equal(e.prevented,false);assert.equal(palettes,0);
  for(const modifier of['ctrlKey','metaKey']){e=event({[modifier]:true});handler(e);assert.equal(e.prevented,true);assert.equal(sends.at(-1),true);assert.equal(palettes,0);}
  const before=sends.length;for(const ime of[{isComposing:true},{keyCode:229}]){e=event(ime);handler(e);assert.equal(e.prevented,false);}assert.equal(sends.length,before);
  e=event({repeat:true});handler(e);assert.equal(e.prevented,enabled);assert.equal(sends.length,before);
 }
});

test('all shared text call sites explicitly classify prose versus machine/URL/secret',()=>{
 const lines=source.split('\n').filter(line=>/\binput\s*:\s*true/.test(line));
 for(const line of lines){assert.match(line,/ask\(\{/);assert.match(line,/inputKind:/,line);}
 for(const title of['Rename session','Suggested name','Add a memory','Edit memory'])assert.ok(lines.some(line=>line.includes('"'+title+'"')&&line.includes('inputKind: "prose"')));
 assert.ok(lines.some(line=>line.includes('Import Tools from a URL')&&line.includes('inputKind: "url"')));
 assert.ok(lines.some(line=>line.includes('Type delete to confirm')&&line.includes('inputKind: "machine"')));
});
