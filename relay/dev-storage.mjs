// Local-only real SQLite adapter for the same bounded Worker ledger. Node's
// built-in SQLite avoids a mock SQL parser and external package dependencies.
import { DatabaseSync } from 'node:sqlite';
export class DevStorage {
  constructor(onAlarm=()=>{},database=':memory:'){
    this.database=new DatabaseSync(database);this.data=new Map();this.onAlarm=onAlarm;this.timer=null;this.alarmAt=null;
    this.sql={exec:(query,...bindings)=>this.database.prepare(query).all(...bindings)};
  }
  async list({prefix,limit=Infinity}){return new Map([...this.data].filter(([key])=>key.startsWith(prefix)).sort(([a],[b])=>a<b?-1:a>b?1:0).slice(0,limit));}
  async put(key,value){this.data.set(key,value);}
  async delete(key){this.data.delete(key);}
  getAlarm(){return this.alarmAt;}
  async setAlarm(at){this.alarmAt=at;if(this.timer)clearTimeout(this.timer);this.timer=setTimeout(()=>{this.timer=null;this.alarmAt=null;Promise.resolve(this.onAlarm()).catch(()=>console.error('relay: alarm failed'));},Math.max(0,at-Date.now()));this.timer.unref();}
  async deleteAlarm(){if(this.timer)clearTimeout(this.timer);this.timer=null;this.alarmAt=null;}
  close(){if(this.timer)clearTimeout(this.timer);this.database.close();}
}
