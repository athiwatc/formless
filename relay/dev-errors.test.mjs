import test from 'node:test';
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {request} from 'node:http';
import {connect} from 'node:net';
import {fileURLToPath} from 'node:url';
import {httpBoundary,upgradeBoundary} from './dev-errors.mjs';

class Response {
  headersSent=false;writableEnded=false;destroyed=false;writes=0;
  writeHead(status,headers){this.status=status;this.headers=headers;this.headersSent=true;this.writes++;}
  end(body){this.body=body;this.writableEnded=true;}
  destroy(){this.destroyed=true;}
}
const fail=async()=>{await Promise.resolve();throw Error('private fixture detail');};

test('HTTP boundary awaits async failure and emits a generic 500 without request details',async()=>{
  const res=new Response(),reported=[];
  await httpBoundary({},res,fail,(...args)=>reported.push(args));
  assert.equal(res.status,500);assert.equal(res.headers.connection,'close');
  assert.equal(JSON.parse(res.body).error.type,'server_error');
  assert.ok(!res.body.includes('private fixture detail'));
  assert.equal(reported[0][0],'HTTP');assert.ok(reported[0][1] instanceof Error);
});

test('HTTP streaming failure closes the response instead of writing another header/body',async()=>{
  const res=new Response();
  await httpBoundary({},res,async()=>{res.writeHead(200,{});await fail();},()=>{});
  assert.equal(res.status,200);assert.equal(res.writes,1);assert.equal(res.body,undefined);
  assert.equal(res.destroyed,true);
});

test('HTTP boundary leaves completed and already destroyed responses alone',async()=>{
  for(const field of ['writableEnded','destroyed']){
    const res=new Response();res[field]=true;
    await httpBoundary({},res,fail,()=>{});
    assert.equal(res.writes,0);assert.equal(res.body,undefined);
  }
});

test('failure while sending a 500 destroys the response without another rejection',async()=>{
  const res=new Response();res.writeHead=()=>{throw Error('fixture write failed');};
  await httpBoundary({},res,fail,()=>{});
  assert.equal(res.destroyed,true);
});

test('Upgrade boundary awaits rejection and destroys only the failed socket',async()=>{
  const socket={destroyed:false,destroy(){this.destroyed=true;}},other={destroyed:false},reported=[];
  await upgradeBoundary({},socket,fail,(...args)=>reported.push(args));
  assert.equal(socket.destroyed,true);assert.equal(other.destroyed,false);
  assert.equal(reported[0][0],'Upgrade');assert.ok(reported[0][1] instanceof Error);
});

function http(port,path,{method='GET',body}={}){
  return new Promise((resolve,reject)=>{
    const req=request({hostname:'127.0.0.1',port,path,method,headers:body===undefined?{}:{'content-type':'application/json','content-length':Buffer.byteLength(body)}},res=>{
      const chunks=[];let size=0;
      res.on('data',chunk=>{size+=chunk.length;if(size>256*1024){req.destroy(Error('fixture response cap'));return;}chunks.push(chunk);});
      res.on('error',reject);res.on('aborted',()=>reject(Error('fixture response aborted')));
      res.on('end',()=>resolve({status:res.statusCode,headers:res.headers,body:Buffer.concat(chunks).toString('utf8')}));
    });
    req.setTimeout(3000,()=>req.destroy(Error('owned HTTP request deadline')));
    req.on('error',reject);req.end(body);
  });
}

async function malformedUpgrade(port){
  await new Promise((resolve,reject)=>{
    const socket=connect(port,'127.0.0.1');
    const timer=setTimeout(()=>{socket.destroy();reject(Error('owned Upgrade closure deadline'));},3000);
    socket.on('error',error=>{if(error.code!=='ECONNRESET')reject(error);});
    socket.once('close',()=>{clearTimeout(timer);resolve();});
    socket.once('connect',()=>socket.write('GET http://[ HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: AAAAAAAAAAAAAAAAAAAAAA==\r\nSec-WebSocket-Version: 13\r\n\r\n'));
    socket.on('data',()=>{});
  });
}

async function closeChild(child){
  if(!child.pid||child.exitCode!==null||child.signalCode!==null)return;
  const exited=once(child,'exit');
  const wait=async(ms)=>{let timer;try{return await Promise.race([exited.then(()=>true),new Promise(resolve=>{timer=setTimeout(()=>resolve(false),ms);})]);}finally{clearTimeout(timer);}};
  child.kill('SIGTERM');
  if(await wait(3000))return;
  child.kill('SIGKILL');
  if(!await wait(3000))throw Error('owned relay was not reaped after forced teardown');
  throw Error('owned relay exceeded graceful teardown deadline (forced exit reaped)');
}

test('owned dev relay survives repeated malformed paths, mock bodies and Upgrade targets',{timeout:20000},async t=>{
  const child=spawn(process.execPath,[fileURLToPath(new URL('./dev.mjs',import.meta.url)),'--port','0','--mock-provider'],{stdio:['ignore','pipe','pipe']});
  let closing;const cleanup=()=>closing??=closeChild(child);
  t.after(cleanup);t.signal.addEventListener('abort',()=>{void cleanup().catch(()=>{});},{once:true});
  let out='',errors='';
  child.stderr.on('data',bytes=>{errors=(errors+bytes).slice(-16384);});
  try{
    const port=await new Promise((resolve,reject)=>{
      const timer=setTimeout(()=>reject(Error('owned relay listener deadline')),4000);
      child.stdout.on('data',bytes=>{
        out=(out+bytes).slice(-8192);
        const match=out.match(/relay on http:\/\/127\.0\.0\.1:(\d+)/);
        if(match){clearTimeout(timer);resolve(Number(match[1]));}
      });
      child.once('error',error=>{clearTimeout(timer);reject(error);});
      child.once('exit',(code,signal)=>{clearTimeout(timer);reject(Error(`owned relay exited ${code}/${signal}: ${errors}`));});
    });
    const logged=scope=>new Promise((resolve,reject)=>{
      const check=()=>{if(errors.includes(`relay: ${scope} request failed`)){clearTimeout(timer);child.stderr.off('data',check);resolve();}};
      const timer=setTimeout(()=>{child.stderr.off('data',check);reject(Error(`owned ${scope} error-log deadline`));},3000);
      child.stderr.on('data',check);check();
    });
    const healthy=async()=>{
      const health=await http(port,'/health');assert.equal(health.status,200,health.body);
      assert.equal(child.exitCode,null,errors);assert.equal(child.signalCode,null,errors);
    };
    await healthy();
    for(let round=0;round<3;round++){
      for(const path of ['/%E0%A4%A','/%zz','/%FF']){
        const bad=await http(port,path);assert.equal(bad.status,400,bad.body);await healthy();
      }
      for(const body of ['{bad','{','not-json']){
        const bad=await http(port,'/mock/v1/chat/completions',{method:'POST',body});
        assert.equal(bad.status,400);assert.deepEqual(JSON.parse(bad.body).error,{message:'Invalid JSON request body',type:'invalid_request_error',param:null,code:'invalid_json'});
        await healthy();
      }
    }
    for(const body of ['null','[]','"private fixture detail"']){
      const bad=await http(port,'/mock/v1/chat/completions',{method:'POST',body});
      assert.equal(bad.status,400);assert.equal(JSON.parse(bad.body).error.code,'invalid_request');await healthy();
    }
    // Valid JSON with an unexpected mock-script shape rejects asynchronously.
    // This exercises the actual outer HTTP boundary, not just JSON parsing.
    const failed=await http(port,'/mock/v1/chat/completions',{method:'POST',body:'{"stream":true,"messages":{"private":"fixture-body-must-not-log"}}'});
    assert.equal(failed.status,500);assert.equal(JSON.parse(failed.body).error.code,'internal_error');
    assert.ok(!failed.body.includes('fixture-body-must-not-log'));await logged('HTTP');await healthy();
    await malformedUpgrade(port);await logged('Upgrade');await healthy();
    const valid=await http(port,'/mock/v1/chat/completions',{method:'POST',body:'{"stream":false,"messages":[]}'});
    assert.equal(valid.status,200);assert.ok(JSON.parse(valid.body).choices[0].message.content);
    const models=await http(port,'/mock/v1/models');assert.equal(models.status,200);
    assert.equal(JSON.parse(models.body).data[0].id,'mock-1');
    const asset=await http(port,'/');assert.equal(asset.status,200);assert.ok(asset.headers['content-security-policy']);
    await healthy();assert.ok(!errors.includes('fixture-body-must-not-log'),errors);
    assert.ok(!errors.includes('UnhandledPromiseRejection'),errors);
  }finally{await cleanup();}
});
