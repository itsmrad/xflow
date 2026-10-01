#!/usr/bin/python3
# Optional isolated GNOME 50 runtime check. Requires gnome-shell, gjs and Python GI.
import ast,json,os,pathlib,shutil,subprocess,sys,tempfile,time
from gi.repository import GLib
REPO=pathlib.Path(__file__).resolve().parents[3]
if len(sys.argv)==1:
 scratch=REPO/'target/overlay-smoke';scratch.mkdir(parents=True,exist_ok=True)
 root=pathlib.Path(tempfile.mkdtemp(prefix='smoke-',dir=scratch))
 for name in ['run','cfg','data','cache','services']: (root/name).mkdir(mode=0o700)
 runtime=pathlib.Path(tempfile.mkdtemp(prefix='xfo-run-'))
 extensions=root/'data/gnome-shell/extensions'; extensions.mkdir(parents=True)
 (root/'data/gnome-shell/update-check-50').touch()
 shutil.copytree(REPO/'packaging/gnome-extension',extensions/'xflow@xflow.local',ignore=shutil.ignore_patterns('tests'))
 subprocess.run(['glib-compile-schemas',str(extensions/'xflow@xflow.local/schemas')],check=True)
 driver=extensions/'xflow-test@local';driver.mkdir()
 (driver/'metadata.json').write_text(json.dumps({'uuid':'xflow-test@local','name':'Isolated XFlow test driver','description':'Temporary isolated test helper','shell-version':['50']}))
 (driver/'extension.js').write_text('''import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
export default class TestDriver extends Extension {
 enable() {
  global.context.unsafe_mode = true;
  GLib.file_set_contents(GLib.getenv('XFLOW_SMOKE_ROOT')+'/driver-ready','ready');
  this.requests=[];
  this.stub=Gio.DBusExportedObject.wrapJSObject(`<node><interface name="org.xflow.Daemon"><method name="Command"><arg type="s" direction="in"/><arg type="s" direction="out"/></method><signal name="Event"><arg type="s"/></signal></interface></node>`,{
   Command: request => {const r=JSON.parse(request); this.requests.push(r); return JSON.stringify({ok:true,state:r.command==='start'?'listening':r.command==='stop'?'processing':'idle',level:0});}
  });
  this.stub.export(Gio.DBus.session,'/org/xflow/Daemon');
  this.owner=Gio.bus_own_name_on_connection(Gio.DBus.session,'org.xflow.Daemon',Gio.BusNameOwnerFlags.NONE,null,null);
 }
 event(json){this.stub.emit_signal("Event",new GLib.Variant("(s)",[json]));}
 disable(){this.stub?.unexport();if(this.owner)Gio.bus_unown_name(this.owner);global.context.unsafe_mode=false;}
}
''')
 (root/'bus.conf').write_text('<busconfig><type>session</type><listen>unix:tmpdir='+str(runtime)+'</listen><auth>EXTERNAL</auth><servicedir>'+str(root/'services')+'</servicedir><policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>')
 env=os.environ.copy()
 for k in ['DISPLAY','WAYLAND_DISPLAY','DBUS_SESSION_BUS_ADDRESS','DBUS_STARTER_ADDRESS','DBUS_STARTER_BUS_TYPE']:env.pop(k,None)
 env.update(XDG_RUNTIME_DIR=str(runtime),XDG_CONFIG_HOME=str(root/'cfg'),XDG_DATA_HOME=str(root/'data'),XDG_CACHE_HOME=str(root/'cache'),LIBGL_ALWAYS_SOFTWARE='1',GSETTINGS_BACKEND='keyfile',XFLOW_SMOKE_ROOT=str(root))
 print('Artifacts:',root,flush=True)
 raise SystemExit(subprocess.run(['dbus-run-session','--config-file='+str(root/'bus.conf'),'--','/usr/bin/python3',__file__,'--inner'],env=env).returncode)
root=pathlib.Path(os.environ['XFLOW_SMOKE_ROOT'])
def run(args,timeout=8):
 p=subprocess.run(args,text=True,capture_output=True,timeout=timeout)
 if p.returncode: raise RuntimeError(p.stderr.strip() or p.stdout.strip())
 return p.stdout.strip()
def call(dest,path,method,*args):
 encoded=[GLib.Variant('s',arg).print_(False) for arg in args]
 if method.endswith('Screenshot'):encoded=list(args[:2])+encoded[2:]
 return run(['gdbus','call','--session','--dest',dest,'--object-path',path,'--method',method,*encoded])
def evaluate(js):
 js='const xflowTest=global.xflowTest;const xflowDriver=global.xflowDriver;'+js
 result=ast.literal_eval(call('org.gnome.Shell','/org/gnome/Shell','org.gnome.Shell.Eval',js).replace('(true,','(True,',1).replace('(false,','(False,',1))
 if not result[0]: raise RuntimeError('Eval failed: '+str(result))
 return json.loads(result[1]) if result[1] else None
log=(root/'shell.log').open('w')
shell=None
prefs=None
try:
 run(['gsettings','set','org.gnome.shell','enabled-extensions',"['xflow@xflow.local','xflow-test@local']"])
 run(['gsettings','set','org.gnome.shell','disable-user-extensions','false'])
 shell=subprocess.Popen(['gnome-shell','--wayland','--headless','--virtual-monitor','1280x720','--no-x11'],stdout=log,stderr=log)
 for _ in range(100):
  if shell.poll() is not None:raise RuntimeError('Shell exited; see '+str(root/'shell.log'))
  try:
   if evaluate('JSON.stringify(!!Main.extensionManager.lookup("xflow@xflow.local")?.stateObj)')=='true':break
  except Exception as error:
   if _==99: print('Last Eval error:',str(error),flush=True)
  time.sleep(.15)
 else:
  print('Driver loaded:',(root/'driver-ready').exists(),flush=True)
  print('Info:',call('org.gnome.Shell','/org/gnome/Shell','org.gnome.Shell.Extensions.GetExtensionInfo','xflow@xflow.local'),flush=True)
  raise RuntimeError('Extension not ready; see '+str(root/'shell.log'))
 print('Version',call('org.xflow.Shell','/org/xflow/Shell','org.xflow.Shell.Version'),flush=True)
 print('Context',call('org.xflow.Shell','/org/xflow/Shell','org.xflow.Shell.Context'),flush=True)
 evaluate('global.xflowTest=Main.extensionManager.lookup("xflow@xflow.local").stateObj; global.xflowDriver=Main.extensionManager.lookup("xflow-test@local").stateObj; true')
 evaluate('Main.overview.hide();true')
 results=[]
 for state in ['idle','listening','processing','success','error']:
  payload=json.dumps({'state':state,'level':.08,'mode':'dictation','message':'Example error' if state=='error' else None})
  evaluate('xflowDriver.event('+json.dumps(payload)+');true')
  time.sleep(.22)
  result=json.loads(evaluate('JSON.stringify({state:xflowTest._state,visible:xflowTest._pill.visible,sources:xflowTest._sources.size,frame:!!xflowTest._frameSource,escape:!!xflowTest._escapeAction,x:xflowTest._pill.x,y:xflowTest._pill.y,width:xflowTest._pill.width,height:xflowTest._pill.height})'))
  assert result['state']==state,result
  assert result['frame']==(state in ['listening','processing']),result
  assert result['escape']==(state in ['listening','processing']),result
  if state=='idle':assert result['sources']==0,result
  results.append(result)
  call('org.gnome.Shell.Screenshot','/org/gnome/Shell/Screenshot','org.gnome.Shell.Screenshot.Screenshot','false','false',str(root/(state+'.png')))
 evaluate('xflowTest._settings.set_string("theme","dark");xflowTest._update({state:"listening",level:.12,mode:"command"});true')
 time.sleep(.22)
 call('org.gnome.Shell.Screenshot','/org/gnome/Shell/Screenshot','org.gnome.Shell.Screenshot.Screenshot','false','false',str(root/'command-dark.png'))
 # The isolated Shell has no focused window: verify copy-only instead of keys.
 print('Inject copy-only',call('org.xflow.Shell','/org/xflow/Shell','org.xflow.Shell.Inject','isolated test',json.dumps({'method':'paste','target':{'window_id':'unknown','app_id':None}})),flush=True)
 print('Selection',call('org.xflow.Shell','/org/xflow/Shell','org.xflow.Shell.Selection'),flush=True)
 evaluate('xflowTest._command("start","command",123);xflowTest._command("stop");true')
 time.sleep(.25)
 requests=json.loads(evaluate('JSON.stringify(xflowDriver.requests)'))
 assert [r['command'] for r in requests][-2:]==['start','stop'],requests
 assert requests[-2]['mode']=='command' and requests[-2]['t0_us']==123,requests
 evaluate('xflowTest._settings.set_int("ptt-threshold-ms",100);xflowTest._update({state:"idle"});xflowTest._key(125,true);xflowTest._key(56,true);xflowTest._key(57,true);true')
 time.sleep(.22)
 assert evaluate('JSON.stringify(xflowTest._hotkey.holding)')=='true','Held shortcut released too early'
 evaluate('xflowTest._key(57,false);xflowTest._key(56,false);xflowTest._key(125,false);true')
 time.sleep(.15)
 assert evaluate('JSON.stringify(xflowDriver.requests.slice(-2).map(r=>r.command))')=='["start","stop"]','Push-to-talk did not stop on release'
 # Modifier-first release and tap-to-toggle exercise the real compositor path.
 evaluate('xflowTest._update({state:"idle"});xflowTest._key(125,true);xflowTest._key(56,true);xflowTest._key(57,true);true')
 time.sleep(.16)
 evaluate('xflowTest._key(56,false);true')
 time.sleep(.06)
 assert evaluate('JSON.stringify(xflowTest._state)')=='"processing"','Modifier-first release did not stop'
 evaluate('xflowTest._key(57,false);xflowTest._key(125,false);true')
 evaluate('xflowTest._update({state:"idle"});xflowTest._key(125,true);xflowTest._key(56,true);xflowTest._key(57,true);true')
 time.sleep(.035)
 evaluate('xflowTest._key(57,false);xflowTest._key(56,false);xflowTest._key(125,false);true')
 time.sleep(.06)
 assert evaluate('JSON.stringify(xflowTest._state)')=='"listening"','Quick tap did not latch'
 evaluate('xflowTest._key(125,true);xflowTest._key(56,true);xflowTest._key(57,true);true')
 time.sleep(.05)
 evaluate('xflowTest._key(57,false);xflowTest._key(56,false);xflowTest._key(125,false);true')
 time.sleep(.06)
 assert evaluate('JSON.stringify(xflowTest._state)')=='"processing"','Next press did not end hands-free'
 evaluate('xflowTest._desktop.set_boolean("enable-animations",false);xflowTest._update({state:"processing"});true')
 time.sleep(.05)
 assert evaluate('JSON.stringify(xflowTest._sources.size)')=='0'
 prefsLog=(root/'prefs.log').open('w')
 prefsEnv=os.environ.copy();prefsEnv['WAYLAND_DISPLAY']='wayland-0';prefsEnv['GSK_RENDERER']='cairo'
 prefs=subprocess.Popen(['gjs','-m','/usr/share/gnome-shell/org.gnome.Shell.Extensions'],env=prefsEnv,stdout=prefsLog,stderr=prefsLog)
 for _ in range(40):
  try:
   run(['gdbus','call','--session','--dest','org.gnome.Shell.Extensions','--object-path','/org/gnome/Shell/Extensions','--method','org.gnome.Shell.Extensions.OpenExtensionPrefs',GLib.Variant('s','xflow@xflow.local').print_(False),"''",'{}'])
   break
  except Exception:time.sleep(.1)
 else:raise RuntimeError('Preferences service not ready')
 for _ in range(50):
  if evaluate('JSON.stringify(global.get_window_actors().length)')!='0':break
  time.sleep(.1)
 evaluate('global.get_window_actors()[0]?.meta_window.activate(global.get_current_time());true')
 time.sleep(1.2)
 assert 'JS ERROR' not in (root/'prefs.log').read_text(),(root/'prefs.log').read_text()
 windows=json.loads(evaluate('JSON.stringify(global.get_window_actors().map(a=>({title:a.meta_window.get_title(),id:a.meta_window.get_stable_sequence(),visible:a.visible,mapped:a.mapped,rect:a.meta_window.get_frame_rect(),hidden:a.meta_window.is_hidden()})))'))
 print('Preferences windows:',windows,flush=True)
 assert windows and windows[0]['mapped'],'Preferences did not render a window'
 call('org.gnome.Shell.Screenshot','/org/gnome/Shell/Screenshot','org.gnome.Shell.Screenshot.Screenshot','false','false',str(root/'prefs.png'))
 evaluate('Main.extensionManager.disableExtension("xflow@xflow.local");true')
 time.sleep(.1)
 assert evaluate('JSON.stringify({enabled:xflowTest._enabled,sources:xflowTest._sources.size,grabs:xflowTest._grabs.size})')=='{"enabled":false,"sources":0,"grabs":0}'
 (root/'results.json').write_text(json.dumps({'states':results,'requests':requests,'disable':'all sources and shortcuts removed','preferences':'mapped with software renderer','ptt':'held trigger release and modifier-first release, tap latch and next-press stop verified'},indent=2))
 print('PASS isolated Shell smoke',flush=True)
except Exception as e:
 print('FAIL',e,flush=True)
 print((root/'shell.log').read_text()[-10000:],flush=True)
 raise SystemExit(1)
finally:
 if prefs is not None:
  prefs.terminate()
  try:prefs.wait(timeout=5)
  except subprocess.TimeoutExpired:prefs.kill();prefs.wait()
 if shell is not None:
  shell.terminate()
  try:shell.wait(timeout=5)
  except subprocess.TimeoutExpired:shell.kill();shell.wait()
 log.close()
