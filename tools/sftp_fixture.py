"""Loopback-only SSH/SFTP fixture for integration tests; never a production server.

Install: python -m pip install --target artifacts/python paramiko
Run: python tools/sftp_fixture.py --ready artifacts/fixture.json
The test account is arcnix / arcnix-test. Files stay under an isolated temp root.
"""
from __future__ import annotations

import argparse
import errno
import ipaddress
import json
import os
from pathlib import Path
import socket
import stat
import sys
import tempfile
import threading
import time
import shlex
import hashlib
import base64
import re
import zlib
import io,zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "artifacts" / "python"))
import paramiko
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey


class Authentication(paramiko.ServerInterface):
    tmux_sessions = {}
    nginx_installed = False
    services = {"nginx.service": "active", "worker.service": "inactive", "broken.service": "failed"}
    hosted_tasks = {}
    hosted_tasks_lock = threading.Lock()
    database_policies = {}
    configuration_records = {}
    task_metadata = {}
    cleanup_records = {}
    recovery_records = {}
    audit_cleanups = {}
    task_stops = {}

    def observation_options(self):
        source = self.fixture_root/'task-observation.json'
        return json.loads(source.read_text(encoding='utf-8')) if source.exists() else {}

    def trace_task_query(self, identifier, phase):
        if (self.fixture_root/'trace-task-queries').exists():
            with self.hosted_tasks_lock:
                with (self.fixture_root/'task-queries.jsonl').open('a',encoding='utf-8') as output:
                    output.write(json.dumps(dict(id=identifier,phase=phase,at=time.time_ns()))+'\n')

    def stop_output(self,identifier):
        frozen=self.task_stops[identifier];selection=frozen['plan']['selection'];account=selection['account'];compact=lambda value:json.dumps(value,ensure_ascii=False,separators=(',',':')).encode()
        description=dict(version=1,count=len(selection['ids']),ids_sha256=hashlib.sha256(compact(selection['ids'])).hexdigest(),selection_sha256=hashlib.sha256(compact([1,account['uid'],account['home'],selection['root'],selection['ids']])).hexdigest())
        report=dict(version=1,id=identifier,revision=frozen['revision'],description=description,phase='completed',rows=[dict(id=name,state='stopped',service='inactive',reason='') for name in selection['ids']])
        if (self.fixture_root/'stops-result-wrong').exists():report['rows'][0]['id']='unreviewed-task'
        if (self.fixture_root/'stops-result-missing').exists():return 'Control job exited without final proof\n'
        return 'ARCNIX_TASK_STOP_V1\t'+base64.b64encode(compact(report)).decode()+'\n'

    def records_for_browser(self):
        seed = self.fixture_root/'records-fixture.json'
        records = json.loads(seed.read_text(encoding='utf-8')) if seed.exists() else []
        with self.hosted_tasks_lock:
            for identifier in set(self.task_metadata) | set(self.hosted_tasks):
                job = self.hosted_tasks.get(identifier)
                finished = bool(job and self.task_finished(job))
                metadata = self.task_metadata.get(identifier)
                log=self.drill_output(identifier,job) if finished and job.get('drill') else self.installation_output(identifier,job) if finished and job.get('installation') else 'Saved fixture log · 中文\n'
                if finished and job.get('stop_control'):log=self.stop_output(identifier)
                records.append(dict(id=identifier, modified=metadata['created'] if metadata else int(time.time()), state=('failed' if job['code'] else 'inactive') if finished else 'active' if job else 'not-found', code=job['code'] if finished else None, metadata=metadata, log=log))
                options=self.observation_options()
                if options:
                    records[-1]['state']=options.get('after',options.get('before',records[-1]['state']))
                    records[-1]['code']=options.get('saved',records[-1]['code']);records[-1]['observation']=options;records[-1]['log']=options.get('output',records[-1]['log'])

        return sorted(records, key=lambda entry:(entry['modified'],entry['id']), reverse=True)

    @staticmethod
    def exit_bytes(identifier,code,options):
        if 'raw' in options:return options['raw']
        if code is None:return None
        if options.get('legacy'):return str(code)+'\n'
        return 'ARCNIX_TASK_RESULT_V1\t'+options.get('receipt_task',identifier)+'\t'+options.get('receipt_invocation','1'*32)+'\t'+str(code)+'\n'

    @staticmethod
    def record_line(entry):
        metadata=base64.b64encode(json.dumps(entry['metadata'],ensure_ascii=False).encode()).decode() if entry.get('metadata') is not None else '-'
        size=entry.get('bytes',len(entry.get('log','').encode()));options=entry.get('observation',{})
        raw=Authentication.exit_bytes(entry['id'],entry.get('code'),options);saved=base64.b64encode(raw.encode()).decode() if raw is not None else '-'
        before=options.get('before',entry['state']);after=options.get('after',before);invocation=options.get('invocation','1'*32)
        return '\t'.join(['R',entry['id'],str(entry['modified']),'-' if size is None else str(size),before,options.get('before_invocation',invocation),after,invocation,saved,'3',metadata])+'\n'

    def configuration_browser_entries(self):
        seed=self.fixture_root/'configuration-records-fixture.json'
        rows=json.loads(seed.read_text(encoding='utf-8')) if seed.exists() else []
        with self.hosted_tasks_lock:
            for identifier,request in self.configuration_records.items():
                job=self.hosted_tasks.get(identifier);finished=bool(job and self.task_finished(job))
                rows.append(dict(id=identifier,modified=self.task_metadata.get(identifier,{}).get('created',int(time.time())),phase=job.get('configuration','prepared') if finished else 'publishing' if job else 'prepared',path=request['spec']['path'],kind=request['spec']['kind'],restore_from=request['restore_from'],service=('failed' if job['code'] else 'inactive') if finished else 'active' if job else 'not-found',issue=None,request=request,diff='--- current\n+++ recorded original\n-fixture current private configuration\n+fixture previous version\n'))
        return sorted(rows,key=lambda row:(row['modified'],row['id']),reverse=True)

    def configuration_browser_reply(self,channel,script):
        args=shlex.split(script.split('# arcnix:configuration-records:',1)[1].split('\n',1)[1]);action,argument=args[-2:]
        entries=self.configuration_browser_entries()
        root='/var/lib/arcnix/configurations'
        if (self.fixture_root/'configuration-records-root-changed').exists():root='/changed-configurations'
        payload=dict(version=1,root=root,sampled=int(time.time()))
        def record(entry):
            return {name:entry.get(name) for name in ['id','modified','phase','path','kind','restore_from','service','issue']}
        if action=='list':
            offset=int(argument);page=entries[offset:offset+100]
            payload.update(offset=offset,more=len(entries)>offset+100,record_count=len(entries),allocated_bytes=len(entries)*24576,capacity_complete=not(self.fixture_root/'configuration-records-incomplete-capacity').exists(),records=[record(entry) for entry in page])
        else:
            entry=next((entry for entry in entries if entry['id']==argument),None)
            if not entry or entry.get('issue'):
                channel.sendall_stderr(b'Cannot read private configuration records\n');channel.send_exit_status(2);channel.close();return
            restorable=not entry['restore_from'] and entry['service'] in ['inactive','failed','not-found'] and not entry.get('detail_issue')
            payload.update(record=record(entry),request=entry['request'],original_sha256='b'*64,current_sha256=hashlib.sha256(b'').hexdigest() if entry.get('current_missing',False) else 'e'*64,current_missing=entry.get('current_missing',False),restorable=restorable,issue=entry.get('detail_issue'),diff=entry.get('diff','--- current\n+++ recorded original\n-current configuration\n+private fixture original\n'),events=[dict(phase=entry['phase'],time=1700000000)],logs=entry.get('logs',{'health.log':'private log · arcnix-test · fixture-admin-secret\n'}))
            if (self.fixture_root/'configuration-records-wrong-id').exists():payload['record']['id']='wrong-record'
        output=json.dumps(payload,ensure_ascii=False)
        if (self.fixture_root/'configuration-records-truncated').exists():output=output[:-1]
        channel.sendall(output.encode());channel.send_exit_status(0);channel.close()

    def record_storage_reply(self,channel,script):
        args=shlex.split(script.split('# arcnix:record-storage:',1)[1].split('\n',1)[1]);query=json.loads(zlib.decompress(base64.b64decode(args[-1])))
        scope=query['scope'];policy=query['policy'];account=query['account']
        root='/home/arcnix/.local/state/arcnix/tasks' if scope=='task' else '/var/lib/arcnix/configurations'
        if (self.fixture_root/'storage-root-changed').exists():root='/changed-record-store'
        seed=self.fixture_root/'record-storage-fixture.json'
        stores=json.loads(seed.read_text(encoding='utf-8')) if seed.exists() else {}
        entries=sorted(stores.get(scope,[]),key=lambda row:(row['modified'],row['id']),reverse=True)
        retained={entry['id'] for entry in entries[:policy['keep_latest']]}
        complete=not(self.fixture_root/'storage-references-incomplete').exists()
        def record(entry,objects=False):
            reasons=list(entry.get('reasons',[]));state=entry.get('service','not-found')
            if state in ['active','activating','reloading','deactivating']:reasons.append('Active or stopping server tasks must be retained')
            if state in ['unavailable','unknown']:reasons.append('Server task state cannot be verified')
            if entry['id'] in retained:reasons.append('Record is protected by the keep-latest policy')
            if not complete:reasons.append('Configuration references could not be fully verified')
            if int(time.time())-entry['modified']<policy['min_age_days']*86400:reasons.append('Record is newer than the chosen minimum age')
            allocated=entry.get('allocated_bytes',16384 if scope=='task' else 24576);logical=entry.get('logical_bytes',256 if scope=='task' else 512)
            row=dict(id=entry['id'],title=entry.get('title',entry['id']),modified=entry['modified'],logical_bytes=logical,allocated_bytes=allocated,capacity_complete=entry.get('capacity_complete',True),service=state,eligible=not reasons,reasons=list(dict.fromkeys(reasons)),identity='a'*64,objects=[],objects_complete=entry.get('objects_complete',True))
            row['eligible']=row['eligible'] and row['capacity_complete'] and row['objects_complete']
            if objects:
                names=['output','result','record.json'] if scope=='task' else ['original','candidate','runner.py','request.json','manifest.json']
                parts=[logical-130,2,128] if scope=='task' else [logical-448,64,128,128,128]
                for index,name in enumerate(names):
                    row['objects'].append(dict(path=root+'/'+entry['id']+'/'+name,kind='file',logical_bytes=max(0,parts[index]),allocated_bytes=max(0,allocated-len(names)*4096) if index==0 else 4096,identity='b'*64))
                row['objects'].append(dict(path=root+'/'+entry['id'],kind='directory',logical_bytes=0,allocated_bytes=4096,identity='a'*64))
            return row
        response=dict(version=1,scope=scope,account=account,policy=policy,root=root,root_identity='d'*64,sampled=int(time.time()),references_complete=complete,references_token='c'*64,reference_issues=[] if complete else ['Invalid configuration record: protected-source'],preview_only=True)
        if query['action']=='list':
            offset=query['offset'];page=entries[offset:offset+100]
            response.update(offset=offset,more=len(entries)>offset+100,record_count=len(entries),logical_bytes=sum(e.get('logical_bytes',256 if scope=='task' else 512) for e in entries),allocated_bytes=sum(e.get('allocated_bytes',16384 if scope=='task' else 24576) for e in entries),capacity_complete=all(e.get('capacity_complete',True) for e in entries),records=[record(entry) for entry in page])
        else:
            rows=[record(next(entry for entry in entries if entry['id']==name),True) for name in query['ids']]
            candidates=[row for row in rows if row['eligible']]
            response.update(records=rows,selected_count=len(rows),eligible_count=len(candidates),reclaimable_bytes=sum(row['allocated_bytes'] for row in candidates))
            if (self.fixture_root/'storage-wrong-preview-id').exists() and rows:
                record=rows[0];old=record['id'];record['id']='wrong-record'
                for obj in record['objects']:obj['path']=obj['path'].replace('/'+old,'/wrong-record')
            if (self.fixture_root/'storage-escaped-object').exists() and rows:rows[0]['objects'][0]['path']='/etc/shadow'
        output=json.dumps(response,ensure_ascii=False)
        if (self.fixture_root/'storage-truncated').exists():output=output[:-1]
        channel.sendall(output.encode());channel.send_exit_status(0);channel.close()

    def installation_snapshot_reply(self,channel,text):
        names=shlex.split(next(line for line in text.splitlines() if line.startswith('for package in ')).removeprefix('for package in ').removesuffix('; do'))
        rows=[];actions=[];preserve=' --no-upgrade ' in text
        for name in names:
            installed=self.nginx_installed or name.startswith('curl')
            rows.append('ARCNIX_PACKAGE_V1\t'+name+'\t'+(name+'\t1.24.0\tii \tamd64' if installed else '-\t-\tmissing\t-')+'\n')
            if not installed or not preserve:
                old=' [1.24.0]' if installed else '';new='1.25.0' if installed else '1.24.0'
                if (self.fixture_root/'installer-downgrade').exists():old=' [2.0]'
                actions.extend([f'Inst {name}{old} ({new} Ubuntu [amd64])\n',f'Conf {name} ({new} Ubuntu [amd64])\n'])
        if (self.fixture_root/'installer-dependency-upgrade').exists():actions+=['Inst dependency [1.0] (2.0 Ubuntu [amd64])\n','Conf dependency (2.0 Ubuntu [amd64])\n']
        if (self.fixture_root/'installer-plan-remove').exists():actions+=['Remv curl [8.5]\n']
        if (self.fixture_root/'installer-plan-malformed').exists():actions+=['Inst broken (1.0 Ubuntu)\n']
        status=hashlib.sha256(('installed' if self.nginx_installed else 'missing').encode()).hexdigest()
        output='ARCNIX_APT_STATUS_V1\t'+status+'\n'+''.join(rows)+'ARCNIX_APT_SIMULATION_V1\nReading package lists...\n'+''.join(actions)
        if (self.fixture_root/'installer-transaction-truncated').exists():output=output.split('ARCNIX_APT_SIMULATION_V1')[0]
        if (self.fixture_root/'installer-before-truncated').exists():output='ARCNIX_APT_STATUS_V1\t'+status+'\nARCNIX_APT_SIMULATION_V1\n'+''.join(actions)
        channel.sendall(output.encode());channel.send_exit_status(0)
        channel.close()

    def installation_definition(self,script):
        runner=shlex.split(script)[-1]
        tokens=shlex.split(runner)
        payload=next(tokens[n+2] for n in range(len(tokens)-2) if tokens[n:n+2]==['/bin/sh','-c'] and tokens[n+2].startswith('# arcnix:installers:v1\n'))
        fields=dict(line.split('=',1) for line in payload.splitlines()[1:12])
        definition=json.loads(base64.b64decode(shlex.split(fields['definition'])[0]))
        revision=shlex.split(fields['revision'])[0]
        probes=dict(packages=[],python3=['python'],nginx=['nginx-version','nginx-config','nginx-service'],restic=['restic'],git=['git'],tools=['tools'],postgresql_client=['pg-dump','pg-restore','psql'],mysql_client=['mysql','mysql-dump'])
        keys=['environment']+(['plan'] if definition['version']==2 else [])+['install']+['package:'+name for name in definition['packages']]+probes[definition['kind']]
        return dict(definition=definition,revision=revision,keys=keys,plan=shlex.split(fields.get('expected_plan',"''"))[0],mode=next((m for m in ['environment-change','state-change','plan-change','protocol-v2','unconfirmed-plan','apt-fail','check-fail','service-fail','report-incomplete','report-wrong-id'] if (self.fixture_root/('installer-'+m)).exists()),'passed'))

    def drill_definition(self,script):
        runner=shlex.split(script)[-1];tokens=shlex.split(runner)
        payload=next(tokens[n+2] for n in range(len(tokens)-2) if tokens[n:n+2]==['/bin/sh','-c'] and tokens[n+2].startswith('# arcnix:drills:v1\n'))
        request=json.loads(base64.b64decode(shlex.split(payload.split('\n',1)[1])[-1]))
        request['mode']=next((m for m in ['check-fail','restore-fail','incomplete','wrong-task','wrong-server','interrupted'] if (self.fixture_root/('drill-'+m)).exists()),'passed')
        return request

    @staticmethod
    def drill_output(identifier,record):
        request=record['drill'];description=request['description'];mode=request['mode']
        keys=['snapshot','restore']+['check:'+str(n) for n in range(len(description['checks']))]
        steps=[]
        for n,key in enumerate(keys):
            state='failed' if (mode=='check-fail' and n==2) or (mode=='restore-fail' and n==1) else 'skipped' if mode=='restore-fail' and n>1 else 'unknown' if mode=='interrupted' and n>0 else 'passed'
            detail='Fixture stage '+state
            if key.startswith('check:'):
                check=description['checks'][n-2]
                detail=('Rows: '+str(check['minimum_rows']+2) if check['kind']=='table' else 'Bytes: '+str(check['minimum_bytes']+10)) if state=='passed' else 'Acceptance threshold not met'
            steps.append(dict(key=key,state=state,detail=detail,elapsed_ms=150))
        report=dict(version=1,task='another-task' if mode=='wrong-task' else identifier,revision=request['revision'],
            server='another-server' if mode=='wrong-server' else request['server'],description=description,
            started=1700000000,finished=1700000002,elapsed_ms=2000,tools=['Python 3.12','restic 0.19.1'],
            exit_code=record['code'],interrupted=mode=='interrupted',steps=steps)
        if description['destination']['kind']=='postgres':report['tools']+=['pg_restore 16','psql 16']
        encoded=base64.b64encode(json.dumps(report,separators=(',',':'),ensure_ascii=False).encode()).decode()
        if mode=='incomplete':encoded=encoded[:-12]
        return 'Recovery drill fixture finished\nARCNIX_DRILL_REPORT_V1\t'+encoded+'\n'

    @staticmethod
    def installation_output(identifier,record):
        installation=record['installation'];definition=installation['definition'];revision=installation['revision'];keys=installation['keys'];mode=installation['mode']
        source='other-task' if mode=='report-wrong-id' else identifier
        output='Server-owned fixture job finished · 中文\n'
        output+=f'ARCNIX_INSTALL_START_V1\t{source}\t{revision}\t1\t'+base64.b64encode(json.dumps(definition,separators=(',',':')).encode()).decode()+f'\t{len(keys)}\n'
        for key in keys:
            if mode=='unconfirmed-plan' and key=='plan':continue
            if mode=='environment-change':state='failed' if key=='environment' else 'skipped'
            elif mode in ['state-change','plan-change','protocol-v2'] and key in ['plan','install']:state='skipped' if mode=='state-change' and key=='install' else 'failed'
            else:state='failed' if (mode=='apt-fail' and key=='install') or (mode=='check-fail' and key=='nginx-config') or (mode=='service-fail' and key=='nginx-service') else 'passed'
            detail='nginx: configuration test failed' if key=='nginx-config' and state=='failed' else 'inactive' if key=='nginx-service' and state=='failed' else 'Ubuntu 24.04; amd64' if key=='environment' else 'nginx 1.24.0; ii ' if key.startswith('package:') else 'Reviewed APT transaction '+state+'; SHA-256: '+installation['plan'] if key=='plan' else 'Fixture check '+state
            output+=f'ARCNIX_INSTALL_STEP_V1\t{key}\t{state}\t'+base64.b64encode(detail.encode()).decode()+'\n'
        if mode!='report-incomplete':output+=f'ARCNIX_INSTALL_END_V1\t{source}\t{revision}\t{len(keys)}\t'+('failed' if record['code'] else 'passed')+f"\t{record['code']}\n"
        return output

    def task_finished(self, record):
        finished = time.monotonic() >= record['finish'] or (record.get('held') and (self.fixture_root/'release-held-operation').is_file())
        if finished and record['code'] == 0 and not record.get('applied'):
            if record.get('application_script'):
                from server_apps_fixture import apply_task
                apply_task(self,record)
            if record.get('stop_control'):
                plan=self.task_stops[record['stop_control']]['plan']
                for identifier in plan['selection']['ids']:
                    if identifier in self.hosted_tasks:self.hosted_tasks[identifier].update(finish=0,code=143,held=False)
                seed=self.fixture_root/'records-fixture.json'
                if seed.exists():
                    rows=json.loads(seed.read_text(encoding='utf-8'))
                    for row in rows:
                        if row['id'] in plan['selection']['ids']:row.update(state='inactive',code=143)
                    seed.write_text(json.dumps(rows),encoding='utf-8')
            if record.get('install_nginx'):
                type(self).nginx_installed = True
            if record.get('service'):
                unit, action = record['service']
                if action in ['enable','disable']:
                    path=self.fixture_root/'services-startup.json';values=json.loads(path.read_text()) if path.exists() else {};values[unit]='enabled' if action=='enable' else 'disabled';path.write_text(json.dumps(values))
                else:self.services[unit] = 'inactive' if action == 'stop' else 'active'
            if record.get('cleanup'):
                plan=self.cleanup_records[record['cleanup']]
                seed=self.fixture_root/'record-storage-fixture.json'
                stores=json.loads(seed.read_text(encoding='utf-8'));names={row['id'] for row in plan['records']}
                stores[plan['scope']]=[row for row in stores.get(plan['scope'],[]) if row['id'] not in names]
                seed.write_text(json.dumps(stores),encoding='utf-8')
            if record.get('recovery'):
                plan=self.recovery_records[record['recovery']]
                if plan['action']=='release':
                    seed=self.fixture_root/'cleanup-reports-fixture.json'
                    entries=json.loads(seed.read_text(encoding='utf-8'))
                    for entry in entries:
                        if entry['id']==plan['source']['record']['id']:entry.update(partial=False,released=True)
                    seed.write_text(json.dumps(entries),encoding='utf-8')
            if record.get('audit_cleanup'):
                identifier=record['audit_cleanup'];plan=self.audit_cleanups[identifier]['preview']
                seed=self.fixture_root/'audit-storage-fixture.json';stores=json.loads(seed.read_text());ids=set(plan['selected'])
                stores['cleanup']=[row for row in stores.get('cleanup',[]) if row['id'] not in ids]
                result=next(row for row in stores.setdefault('maintenance',[]) if row['id']==identifier);result.update(modified=int(time.time()),phase='completed')
                seed.write_text(json.dumps(stores))
            record['applied'] = True
        return finished

    def audit_storage_reply(self,channel,script):
        parts=shlex.split(script.split('# arcnix:audit-storage:',1)[1].split('\n',1)[1]);query=json.loads(zlib.decompress(base64.b64decode(parts[-1])))
        if (self.fixture_root/'audit-storage-slow').exists():time.sleep(2)
        if (self.fixture_root/'audit-storage-denied').exists():
            channel.sendall_stderr(b'Cannot inspect audit storage: fixture-admin-secret denied\n');channel.send_exit_status(2);channel.close();return
        seed=self.fixture_root/'audit-storage-fixture.json';stores=json.loads(seed.read_text()) if seed.exists() else {}
        for job in list(self.hosted_tasks.values()):self.task_finished(job)
        if seed.exists():stores=json.loads(seed.read_text())
        scope=query['scope'];policy=query['policy'];rows=stores.get(scope,[]);rows=sorted(rows,key=lambda r:(r['modified'],r['id']),reverse=True)
        root='/var/lib/arcnix/'+dict(cleanup='record-cleanups',recovery='record-recoveries',maintenance='audit-cleanups')[scope];now=1794732331;references_complete=not(self.fixture_root/'audit-storage-incomplete').exists()
        selected=query['ids'];preview=query['action']=='preview';offset=query['offset'];retained={r['id'] for r in rows[:policy['keep_latest']]};result=[]
        for original in ([next(r for r in rows if r['id']==name) for name in selected] if preview else rows[offset:offset+100]):
            row=dict(original);row.setdefault('phase','completed');row.setdefault('action',None);row.setdefault('source',None);row.setdefault('service','inactive');row.setdefault('allocated_bytes',16384);row.setdefault('logical_bytes',300);row.setdefault('capacity_complete',True);row['reasons']=list(row.get('reasons',[]))
            if row['id'] in retained:row['reasons'].append('retained')
            if row['modified']>now-policy['min_age_days']*86400:row['reasons'].append('recent')
            if not references_complete:row['reasons'].append('references')
            row['candidate']=preview and not row['reasons'];row['objects_complete']=preview and scope=='cleanup';row['objects']=[]
            if row['objects_complete']:
                base=root+'/'+row['id'];row['objects']=[dict(path=base,kind='directory',identity='a'*64,allocated_bytes=4096,logical_bytes=0)]+[dict(path=base+'/'+name,kind='file',identity='b'*64,allocated_bytes=4096,logical_bytes=100) for name in ['request.json','report.json','runner.py']]
            if not row['candidate'] and not row['reasons']:row['reasons'].append('preview')
            row['reasons']=list(dict.fromkeys(row['reasons']));result.append(row)
        response=dict(version=1,scope=scope,root=root,root_identity='a'*64,references_token='b'*64,sampled=now,policy=policy,preview_only=preview,offset=offset,more=not preview and len(rows)>offset+100,record_count=len(rows),allocated_bytes=sum(r.get('allocated_bytes',16384) for r in rows),logical_bytes=sum(r.get('logical_bytes',300) for r in rows),capacity_complete=True,references_complete=references_complete,selected=selected,candidate_count=sum(r['candidate'] for r in result),candidate_allocated_bytes=sum(r['allocated_bytes'] for r in result if r['candidate']),records=result)
        if (self.fixture_root/'audit-storage-wrong-scope').exists():response['root']='/etc'
        if (self.fixture_root/'audit-storage-wrong-selection').exists():response['selected']=['different-audit']
        channel.sendall(json.dumps(response,ensure_ascii=False).encode());channel.send_exit_status(0);channel.close()

    def export_download_reply(self,channel,script):
        parts=shlex.split(script.split('# arcnix:export-download:',1)[1].split('\n',1)[1]);action,argument=parts[-2:]
        if (self.fixture_root/'export-download-inspect-slow').exists() and action=='inspect':time.sleep(2)
        if (self.fixture_root/'export-download-denied').exists():
            channel.sendall_stderr(b'Cannot download export: fixture-admin-secret denied\n');channel.send_exit_status(2);channel.close();return
        raw=zlib.decompress(base64.b64decode(argument)) if action=='stream' else None
        selected=json.loads(raw) if raw else None;identifier=selected['id'] if selected else argument
        rows=json.loads((self.fixture_root/'export-download-fixture.json').read_text());row=next(row for row in rows if row['id']==identifier)
        payload=base64.b64decode(row.get('data',base64.b64encode(b'fixture residual\x00\xff').decode()));source=row['source'];directory='record-1'
        metadata=json.dumps(dict(version=1,uid=1001,gid=1001,mode=384,mtime_ns=1,ctime_ns=1,xattrs=[])).encode()
        manifest=json.dumps(dict(version=1,source=source,files=[dict(record=directory,name='result',size=len(payload),sha256=hashlib.sha256(payload).hexdigest(),metadata_name='metadata-result.json',metadata_sha256=hashlib.sha256(metadata).hexdigest())])).encode()
        content={'request.json':json.dumps(dict(version=1,source=source)).encode(),'report.json':json.dumps(dict(version=1,id=identifier,source=source,phase='completed')).encode(),'manifest.json':manifest,f'files/{directory}/result':payload,f'files/{directory}/metadata-result.json':metadata}
        files=[dict(path=path,size=len(data),sha256=hashlib.sha256(data).hexdigest(),identity=dict({'request.json':'b','report.json':'c'}).get(path,'d')*64,kind='audit' if '/' not in path else 'metadata' if path.endswith('metadata-result.json') else 'data') for path,data in sorted(content.items())]
        request=dict(version=1,id=identifier,source=source,root='/var/lib/arcnix/record-recoveries',record_identity='a'*64,request_identity='b'*64,report_identity='c'*64,manifest_identity='d'*64,manifest_sha256=hashlib.sha256(manifest).hexdigest(),inventory_identity=hashlib.sha256(json.dumps(files,sort_keys=True).encode()).hexdigest(),file_count=1,logical_bytes=len(payload),raw_bytes=sum(map(len,content.values())),entry_count=7)
        if action=='inspect':
            response=dict(version=1,sampled=int(time.time()),request=request,directories=[directory],files=files)
            if (self.fixture_root/'export-download-wrong-id').exists():response['request']['id']='another-export'
            if (self.fixture_root/'export-download-wrong-source').exists():response['request']['source']='another-source'
            if (self.fixture_root/'export-download-wrong-path').exists():response['files'][-1]['path']='../../outside'
            if (self.fixture_root/'export-download-missing-metadata').exists():response['files']=[f for f in files if f['kind']!='metadata']
            channel.sendall(json.dumps(response).encode());channel.send_exit_status(0);channel.close();return
        if selected!=request or (self.fixture_root/'export-download-stale').exists():
            channel.sendall_stderr(b'Export changed; inspect it again before downloading\n');channel.send_exit_status(2);channel.close();return
        counter=self.fixture_root/'export-download-streams';counter.write_text(str(int(counter.read_text())+1 if counter.exists() else 1))
        buffer=io.BytesIO()
        with zipfile.ZipFile(buffer,'w',compression=zipfile.ZIP_STORED,allowZip64=True) as archive:
            archive.writestr('files/',b'');archive.writestr(f'files/{directory}/',b'')
            for path,data in sorted(content.items()):archive.writestr(path,data)
        body=buffer.getvalue();receipt=dict(version=1,id=identifier,request_sha256=hashlib.sha256(raw).hexdigest(),manifest_sha256=request['manifest_sha256'],archive_bytes=len(body),archive_sha256=hashlib.sha256(body).hexdigest(),entry_count=7)
        if (self.fixture_root/'export-download-bad-checksum').exists():receipt['archive_sha256']='0'*64
        if (self.fixture_root/'export-download-bad-request').exists():receipt['request_sha256']='0'*64
        if (self.fixture_root/'export-download-truncated').exists():body=body[:len(body)//2]
        if (self.fixture_root/'export-download-bad-magic').exists():body=b'not a ZIP'+body
        if (self.fixture_root/'export-download-oversized').exists():body+=bytes(request['raw_bytes']+request['entry_count']*1024+1024*1024+1-len(body))
        if (self.fixture_root/'export-download-slow').exists():
            for offset in range(0,len(body),8192):channel.sendall(body[offset:offset+8192]);time.sleep(.025)
        else:channel.sendall(body)
        if not (self.fixture_root/'export-download-no-receipt').exists():channel.sendall_stderr(('ARCNIX_EXPORT_ZIP_V1\t'+json.dumps(receipt)+'\n').encode())
        if (self.fixture_root/'export-download-duplicate-receipt').exists():channel.sendall_stderr(('ARCNIX_EXPORT_ZIP_V1\t'+json.dumps(receipt)+'\n').encode())
        channel.send_exit_status(3 if (self.fixture_root/'export-download-final-error').exists() else 0);channel.close()

    def audit_details_reply(self,channel,script):
        name=shlex.split(script.split('# arcnix:audit-details:read\n',1)[1])[-1]
        if (self.fixture_root/'audit-details-slow').exists():time.sleep(2)
        if (self.fixture_root/'audit-details-denied').exists():
            channel.sendall_stderr(b'Cannot inspect audit cleanup: fixture-admin-secret denied\n');channel.send_exit_status(2);channel.close();return
        store=json.loads((self.fixture_root/'audit-storage-fixture.json').read_text());row=next(r for r in store.get('maintenance',[]) if r['id']==name)
        root='/var/lib/arcnix/audit-cleanups';absent=dict(state='absent',identity=None,logical_bytes=None,allocated_bytes=None)
        completed=row.get('phase')=='completed';prepared=row.get('phase')=='prepared';partial=not completed and not prepared
        files=[]
        for file in ['request.json','report.json','runner.py']:
            remaining=dict(state='matching',identity='c'*64,logical_bytes=128,allocated_bytes=4096)
            files.append(dict(name=file,removal='recorded' if completed or partial and file=='report.json' else 'pending' if partial and file=='request.json' else 'not_recorded',original=remaining if prepared else absent,quarantine=remaining if partial and file=='runner.py' else absent))
        response=dict(version=1,id=name,root=root,source_root='/var/lib/arcnix/record-cleanups',report_path=f'{root}/{name}/report.json',quarantine_path=f'{root}/{name}/quarantine',sampled=int(time.time()),phase=row.get('phase','interrupted'),service_before=row.get('service','inactive'),service=row.get('service','inactive'),stable=True,selected_count=1,request_identity='a'*64,report_identity='b'*64,error=row.get('error'),issues=[],records=[dict(id=row.get('source','source-0001'),phase='removed' if completed else None if prepared else 'quarantined',original_present=prepared,quarantine_present=partial,quarantine_proven=partial,objects=files)])
        if (self.fixture_root/'audit-details-changed').exists():response.update(stable=False,issues=['changed'])
        if (self.fixture_root/'audit-details-wrong-id').exists():response['id']='another-audit'
        if (self.fixture_root/'audit-details-wrong-path').exists():response['quarantine_path']='/etc'
        if (self.fixture_root/'audit-details-omitted-file').exists():response['records'][0]['objects'].pop()
        channel.sendall(json.dumps(response,ensure_ascii=False).encode());channel.send_exit_status(0);channel.close()

    def cleanup_browser_reply(self,channel,script):
        parts=shlex.split(script.split('# arcnix:cleanup-records:',1)[1].split('\n',1)[1]);action,argument=parts[-2:]
        if (self.fixture_root/'cleanup-reports-slow').exists():time.sleep(.8)
        if (self.fixture_root/'cleanup-reports-denied').exists():
            channel.sendall_stderr(b'Cleanup reports unavailable\n');channel.send_exit_status(2);channel.close();return
        seed=self.fixture_root/'cleanup-reports-fixture.json'
        entries=json.loads(seed.read_text(encoding='utf-8')) if seed.exists() else []
        with self.hosted_tasks_lock:
            for identifier,plan in self.cleanup_records.items():
                job=self.hosted_tasks.get(identifier);finished=bool(job and self.task_finished(job))
                entries.append(dict(id=identifier,modified=self.task_metadata.get(identifier,{}).get('created',int(time.time())),phase='completed' if finished else 'running' if job else 'prepared',scope=plan['scope'],selected_count=len(plan['records']),processed_count=len(plan['records']) if finished else 0,removed_files=sum(len(r['objects'])-1 for r in plan['records']) if finished else 0,pending_files=0,service='inactive' if finished else 'active' if job else 'not-found',issue=None))
        entries.sort(key=lambda r:(r['modified'],r['id']),reverse=True)
        root='/var/lib/arcnix/record-cleanups';sampled=int(time.time())
        def summary(entry):return {name:entry.get(name) for name in ['id','modified','phase','scope','selected_count','processed_count','removed_files','pending_files','service','issue']}
        if action=='list':
            offset=int(argument);page=entries[offset:offset+100]
            response=dict(version=1,root=root,sampled=sampled,offset=offset,more=len(entries)>offset+100,record_count=len(entries),allocated_bytes=len(entries)*24576,capacity_complete=not(self.fixture_root/'cleanup-reports-incomplete').exists(),records=[summary(e) for e in page])
        else:
            entry=next(e for e in entries if e['id']==argument)
            plan=self.cleanup_records.get(argument)
            scope=entry.get('scope') or 'task';account=plan['account'] if plan else dict(home='/home/arcnix',uid=1000)
            record_root=plan['root'] if plan else '/home/arcnix/.local/state/arcnix/tasks' if scope=='task' else '/var/lib/arcnix/configurations'
            policy=plan['policy'] if plan else dict(min_age_days=7,keep_latest=10 if scope=='task' else 3)
            source=plan['records'] if plan else [dict(id=argument+'-source',objects=[dict(kind='file',path=record_root+'/'+argument+'-source/'+name) for name in (['output','result','record.json'] if scope=='task' else ['candidate','runner.py','request.json','manifest.json','original'])])]
            partial=entry.get('partial',False);completed=entry['phase']=='completed';released=entry.get('released',False);rows=[]
            def observation(state):return dict(state=state,identity='a'*64 if state in ['matching','changed'] else None,logical_bytes=2 if state in ['matching','changed'] else None,allocated_bytes=4096 if state in ['matching','changed'] else None)
            for record in source:
                objects=[]
                for n,obj in enumerate(o for o in record['objects'] if o['kind']=='file'):
                    objects.append(dict(name=obj['path'].rsplit('/',1)[1],saved_removed=completed,saved_pending=entry.get('pending_files',0)>0 and n==0,normal=observation('absent' if completed or partial or released else 'matching'),quarantine=observation('absent' if completed or not partial or n==0 else 'matching')))
                rows.append(dict(id=record['id'],saved_phase='quarantined' if partial or released else 'removed' if completed else None,normal_present=not(completed or partial or released),quarantine_present=partial,directory_proven=partial and not entry.get('legacy',False),objects=objects))
            recoveries=[]
            for identifier,request in self.recovery_records.items():
                if request['source']['record']['id']!=argument:continue
                job=self.hosted_tasks.get(identifier);finished=bool(job and self.task_finished(job))
                action=request['action'];export_id=identifier if action=='export' else request['export_id']
                count=sum(o['quarantine']['state'] in ['matching','changed'] for r in request['source']['records'] for o in r['objects'])
                recoveries.append(dict(id=identifier,source=argument,action=action,phase='completed' if finished and job['code']==0 else 'failed' if finished else 'exporting' if job else 'prepared',service='failed' if finished and job['code'] else 'inactive' if finished else 'active' if job else 'not-found',destination='/var/lib/arcnix/record-recoveries/'+export_id+'/files',file_count=count,logical_bytes=count*2,manifest_identity='d'*64 if action=='export' and finished and job['code']==0 else None,manifest_sha256='e'*64 if action=='export' and finished and job['code']==0 else None,issue=None))
            downloaded=self.fixture_root/'export-download-fixture.json'
            if downloaded.exists():
                for copy in json.loads(downloaded.read_text()):
                    if copy['source']==argument:recoveries.append(dict(id=copy['id'],source=argument,action='export',phase='completed',service='inactive',destination='/var/lib/arcnix/record-recoveries/'+copy['id']+'/files',file_count=1,logical_bytes=len(base64.b64decode(copy['data'])),manifest_identity='d'*64,manifest_sha256='e'*64,issue=None))
            response=dict(version=1,root=root,sampled=sampled,record=summary(entry),scope=scope,account=account,policy=policy,record_root=record_root,report_path=root+'/'+argument+'/report.json',request_identity='b'*64,report_identity='c'*64,quarantine_path=record_root+'/.arcnix-cleanup-'+argument,quarantine_present=partial,quarantine_identity='f'*64 if partial else None,quarantine_complete=not entry.get('legacy',False),issues=['Quarantined directory identity cannot be proven'] if entry.get('legacy') else [],error='private arcnix-test fixture-admin-secret detail' if partial else None,records=rows,recoveries=recoveries)
            if (self.fixture_root/'cleanup-reports-wrong-id').exists():response['record']['id']='wrong-report'
            if (self.fixture_root/'cleanup-reports-escaped-path').exists():response['quarantine_path']='/etc'
        if (self.fixture_root/'cleanup-reports-changed-root').exists():response['root']='/changed-cleanup-records'
        output=json.dumps(response,ensure_ascii=False)
        if (self.fixture_root/'cleanup-reports-truncated').exists():output=output[:-1]
        channel.sendall(output.encode());channel.send_exit_status(0);channel.close()

    def operation_busy(self):
        return any(record.get('exclusive') and not self.task_finished(record) for record in self.hosted_tasks.values())
    def __init__(self, client_key, transport, audit_auth):
        super().__init__()
        self.client_key = client_key
        self.transport = transport
        self.audit_auth = audit_auth
        self.sftp_enabled = True
        self.metrics_sample = 0
        self.windows_service_state = "Stopped"
        self.windows_time_zone = "UTC"
        self.username = ""
        self.terminal_size = (100, 30)

    def check_auth_password(self, username, password):
        self.username = username
        self.audit_auth(username, "password")
        self.sftp_enabled = username not in ("arcnix-nosftp", "arcnix-windows")
        if username == "arcnix-word":
            return paramiko.AUTH_SUCCESSFUL if password == "passed" else paramiko.AUTH_FAILED
        return paramiko.AUTH_SUCCESSFUL if username in ("arcnix", "arcnix-nosftp", "arcnix-no-terminal", "arcnix-nosystemd", "arcnix-windows") and password == "arcnix-test" else paramiko.AUTH_FAILED

    def get_allowed_auths(self, username):
        return "password,publickey"

    def check_auth_publickey(self, username, key):
        self.username = username
        self.audit_auth(username, "publickey")
        self.sftp_enabled = username != "arcnix-nosftp"
        return paramiko.AUTH_SUCCESSFUL if username in ("arcnix", "arcnix-nosftp", "arcnix-no-terminal", "arcnix-nosystemd") and key in self.client_key else paramiko.AUTH_FAILED

    def check_channel_pty_request(self, channel, term, width, height, pixelwidth, pixelheight, modes):
        self.terminal_size = (width, height)
        return self.username != "arcnix-no-terminal" and term == b"xterm-256color"

    def check_channel_window_change_request(self, channel, width, height, pixelwidth, pixelheight):
        self.terminal_size = (width, height)
        return True

    def check_channel_shell_request(self, channel):
        return self.interactive(channel, "/fixture")

    def interactive(self, channel, directory, session_id=None):
        # A deterministic SSH PTY protocol peer, never a host OS shell.
        def interactive():
            nonlocal directory
            try:
                session=self.tmux_sessions.setdefault((self.username,session_id),dict(directory=directory,history=[])) if session_id else None
                if session:directory=session['directory']
                channel.sendall(b"\x1b[?2004h\x1b[32mArcnix terminal fixture\x1b[0m\r\n$ ")
                if session:
                    channel.sendall(b''.join(session['history']))
                line = bytearray()
                while True:
                    data = channel.recv(8192)
                    if not data:
                        return
                    data = data.replace(b"\x1b[200~", b"").replace(b"\x1b[201~", b"")
                    for value in data:
                        if value == 3:
                            line.clear()
                            channel.sendall(b"^C\r\n$ ")
                        elif value in (8, 127):
                            if line:
                                line.pop()
                                channel.sendall(b"\b \b")
                        elif value in (10, 13):
                            command = bytes(line).decode("utf-8", errors="replace")
                            line.clear()
                            channel.sendall(b"\r\n")
                            if command == "exit":
                                channel.send_exit_status(0)
                                channel.close()
                                return
                            if command == "pwd":
                                channel.sendall(directory.encode() + b"\r\n")
                            elif command == "size":
                                channel.sendall((str(self.terminal_size[0]) + "x" + str(self.terminal_size[1]) + "\r\n").encode())
                            elif command == "ansi":
                                channel.sendall(b"\x1b[31mRED\x1b[0m\r\n")
                            elif command == "osc52":
                                channel.sendall(b"\x1b]52;c;YmFkLWNsaXBib2FyZA==\x07")
                            elif command == "unicode":
                                encoded = "中文终端\r\n".encode()
                                channel.sendall(encoded[:2])
                                channel.sendall(encoded[2:])
                            elif command == "burst":
                                for index in range(5000):
                                    channel.sendall((str(index) + " " + "terminal-output " * 8 + "\r\n").encode())
                                channel.sendall(b"BURST COMPLETE\r\n")
                            elif command.startswith("echo "):
                                channel.sendall(command[5:].encode() + b"\r\n")
                                if session:
                                    session['history'].append(command[5:].encode()+b'\r\n')
                                    session['history']=session['history'][-100:]
                            elif command.startswith('cd ') and session:
                                target=command[3:].strip()
                                if target.startswith('/'):
                                    directory=target;session['directory']=directory
                            elif command.startswith("checksum "):
                                channel.sendall(hashlib.sha256(command[9:].encode()).hexdigest().encode() + b"\r\n")
                            elif command:
                                channel.sendall(b"Fixture command received\r\n")
                            channel.sendall(b"$ ")
                        else:
                            if len(line) < 262144:
                                line.append(value)
                                channel.sendall(bytes([value]))
            except (OSError, EOFError, paramiko.SSHException):
                pass
        threading.Thread(target=interactive, daemon=True).start()
        return True

    def check_channel_subsystem_request(self, channel, name):
        if name == "sftp" and not self.sftp_enabled:
            return False
        return super().check_channel_subsystem_request(channel, name)

    def check_channel_direct_tcpip_request(self, channel_id, origin, destination):
        host, port = destination
        if self.browser_port:
            with (self.fixture_root / 'browser-connect-attempts.jsonl').open('a', encoding='utf-8') as trace:
                trace.write(json.dumps(dict(host=host, port=port)) + '\n')
        if self.browser_tls_port and host == 'tls.browser-fixture.test' and port == 443:
            self.browser_channels[channel_id] = (("127.0.0.1", self.browser_tls_port), True)
            return paramiko.OPEN_SUCCEEDED
        if self.browser_https_address and host == 'example.com' and port == 443:
            destination = (self.browser_https_address, port)
        # Model an HTTP-only server on its standard port without binding port 80 on the test host.
        if self.browser_port and host in {"http-only.browser-fixture.test", "192.0.2.87"}:
            if port != 80:
                return paramiko.OPEN_FAILED_CONNECT_FAILED
            destination = ("127.0.0.1", self.browser_port)
            host, port = destination
        asset = host in {"assets.browser-fixture.test", "scripts.browser-fixture.test"}
        local = bool(self.browser_port and port == self.browser_port
                     and (asset or host in {"browser-fixture.test", "127.0.0.1", "localhost", "::1"}))
        repaired = self.browser_forwarding_repaired.get(self.username) is True
        if not repaired and (asset or self.browser_forwarding_denied or (not local and not (self.browser_web and port in (80, 443)))):
            return paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED
        if local and (self.fixture_root / 'browser-destination-unreachable').exists():
            return paramiko.OPEN_FAILED_CONNECT_FAILED
        self.browser_channels[channel_id] = (destination, local)
        return paramiko.OPEN_SUCCEEDED

    def check_channel_request(self, kind, channel_id):
        return paramiko.OPEN_SUCCEEDED if kind == "session" else paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

    def snapshot_files_reply(self, channel, script):
        request=json.loads(shlex.split(script.split('# arcnix:backups:files\n',1)[1])[-1])
        with (self.fixture_root/'snapshot-file-queries.jsonl').open('a',encoding='utf-8') as log:log.write(json.dumps(request)+'\n')
        if (self.fixture_root/'snapshot-files-delay').exists():time.sleep(1)
        if (self.fixture_root/'snapshot-files-fail').exists():
            channel.sendall_stderr(b'fixture snapshot read failed fixture-admin-secret arcnix-test\n');channel.send_exit_status(2);channel.close();return
        snapshot=dict(id=request['snapshot'],time='2026-11-14T08:00:00Z',hostname='fixture 中文',paths=['/srv/site'],tags=[request['policy_tag'],request['server_tag']])
        if request['action']=='selection':
            if (self.fixture_root/'snapshot-selection-fail').exists():
                channel.sendall_stderr(b'A selected path is absent from the snapshot; browse and review again\n');channel.send_exit_status(2);channel.close();return
            payload=dict(version=1,snapshot=snapshot,paths=request['paths'])
        else:
            directory=request['directory'];offset=request['offset']
            if directory=='/srv/site':items=[('/srv/site/assets','dir'),('/srv/site/many','dir'),('/srv/site/config[prod]*?.txt','file'),('/srv/site/link','symlink')]
            elif directory=='/srv/site/many':items=[('/srv/site/many/entry-%03d'%n,'file') for n in range(105)]
            elif directory=='/srv/site/assets':items=[('/srv/site/assets/report.txt','file'),('/srv/site/assets/中文.txt','file')]
            elif directory=='/':items=[('/srv','dir')]
            elif directory=='/srv':items=[('/srv/site','dir')]
            else:
                channel.sendall_stderr(b'Snapshot directory is absent\n');channel.send_exit_status(2);channel.close();return
            entries=[dict(path=path,kind=kind,size=12 if kind=='file' else None,mode=420,modified='2026-11-14T08:00:00Z') for path,kind in items[offset:offset+100]]
            payload=dict(version=1,snapshot=snapshot,directory=directory,offset=offset,total=len(items),sampled=1700000000,entries=entries)
            if (self.fixture_root/'snapshot-files-cross-path').exists():payload['entries'][0]['path']='/outside/child'
        if (self.fixture_root/'snapshot-files-wrong-id').exists():payload['snapshot']['id']='b'*64
        raw=json.dumps(payload,ensure_ascii=False).encode()
        if (self.fixture_root/'snapshot-files-truncated').exists():raw=raw[:-3]
        for offset in range(0,len(raw),4096):channel.sendall(raw[offset:offset+4096])
        channel.send_exit_status(0);channel.close()

    def resource_ports_reply(self, channel, script, administrator):
        """Model only fixture-owned identities; never signal a host process."""
        fixture = self.fixture_root / 'resource-ports-fixture.json'
        options = json.loads(fixture.read_text(encoding='utf-8')) if fixture.exists() else {}
        terminated = self.fixture_root / 'resource-ports-terminated.json'
        ended = json.loads(terminated.read_text(encoding='utf-8')) if terminated.exists() else []
        web = dict(pid=200, start_ticks=12345, uid=1000, fd=3, command='fixture-web',
                   executable='/usr/bin/fixture-web', unit='fixture-web.service')
        dns = dict(pid=201, start_ticks=23456, uid=0, fd=5, command='fixture-dns',
                   executable='/usr/sbin/fixture-dns', unit='fixture-dns.service')
        rows = [
            dict(protocol='tcp', family=4, address='127.0.0.1', port=8080, uid=1000, inode=42,
                 holders_limited=False, holders=[web]),
            dict(protocol='tcp', family=6, address='::', port=8080, uid=1000, inode=43,
                 holders_limited=False, holders=[dict(web, fd=4)]),
            dict(protocol='udp', family=4, address='0.0.0.0', port=5353, uid=0, inode=44,
                 holders_limited=False, holders=[dns]),
            dict(protocol='udp', family=6, address='::', port=5353, uid=0, inode=45,
                 holders_limited=False, holders=[]),
        ]
        snapshot = dict(version=1, sampled=1794773000, uid=0 if administrator else 1000,
                        boot_id='11111111-1111-4111-8111-111111111111', namespace=4026531840,
                        processes=10, descriptors=23, denied=0 if administrator else 2,
                        changed=0, unreadable=0, limits=[], matched=4, rows=rows)
        snapshot.update(options.get('snapshot', {}))
        snapshot['rows'] = [row for row in snapshot['rows']
                            if not row['holders'] or any(h['pid'] not in ended for h in row['holders'])]
        snapshot['rows'].sort(key=lambda row: (row['protocol'], row['port'], row['family'], row['address'], row['inode']))
        if 'matched' not in options.get('snapshot', {}):
            snapshot['matched'] = len(snapshot['rows'])
        terminating = '# arcnix:resource-ports:terminate' in script
        target = json.loads(shlex.split(script)[-1]) if terminating else None
        query = dict(action='terminate' if terminating else 'read', administrator=administrator)
        if target is not None:
            query['target'] = target
        with (self.fixture_root / 'resource-ports-queries.jsonl').open('a', encoding='utf-8') as audit:
            audit.write(json.dumps(query) + '\n')
        if options.get('disconnect_transport'):
            # Close only this fixture's accepted SSH connection. The listening
            # peer stays available so the desktop can reconnect normally.
            channel.get_transport().close()
            return
        until = time.monotonic() + options.get('terminate_delay' if terminating else 'delay', .15)
        while time.monotonic() < until:
            if channel.closed:
                return
            time.sleep(.02)
        if terminating:
            holder, listener = target['holder'], target['listener']
            valid = (target['boot_id'] == snapshot['boot_id'] and target['namespace'] == snapshot['namespace']
                     and any(all(row[key] == listener[key] for key in ('protocol', 'family', 'address', 'port', 'uid', 'inode'))
                             and holder in row['holders'] for row in snapshot['rows']))
            error = options.get('terminate_error')
            if not valid:
                error = 'Process identity or listening socket changed; refresh ports before retrying'
            elif holder['uid'] != 1000 and not administrator:
                error = 'Permission denied ending another user process'
            if error:
                channel.sendall_stderr(error.encode())
                channel.send_exit_status(options.get('terminate_exit', 1))
            else:
                ended.append(holder['pid'])
                terminated.write_text(json.dumps(sorted(set(ended))), encoding='utf-8')
                result = dict(version=1, pid=holder['pid'], start_ticks=holder['start_ticks'], signal=9)
                channel.sendall(json.dumps(result).encode())
                channel.send_exit_status(0)
        else:
            if options.get('stderr'):
                channel.sendall_stderr(options['stderr'].encode())
            channel.sendall(options.get('raw', json.dumps(snapshot, ensure_ascii=False)).encode())
            channel.send_exit_status(options.get('exit', 0))
        channel.close()

    def check_channel_exec_request(self, channel, command):
        if command.startswith(b'# arcnix:terminal:resume\n'):
            try:
                parts=shlex.split(command.decode().split('\n',1)[1])
                assert parts[:7]==['exec','tmux','-L','arcnix','new-session','-A','-s']
                name=parts[7];assert re.fullmatch(r'terminal-[a-fA-F0-9-]{1,80}',name)
                options = parts.index(';', 8)
                assert parts[options:] == [
                    ';', 'set-option', 'mouse', 'on',
                    ';', 'set-window-option', '-q', 'pane-scrollbars', 'on',
                    ';', 'set-window-option', '-q', 'pane-scrollbars-position', 'right',
                ]
                assert options == 8 or (options == 10 and parts[8] == '-c')
                directory=parts[9] if options == 10 else '/fixture'
                with (self.fixture_root/'terminal-resume-requests.jsonl').open('a') as audit:audit.write(json.dumps(dict(session=name,directory=directory))+'\n')
                return self.interactive(channel,directory,session_id=name)
            except (AssertionError,ValueError,IndexError):return False
        if command.startswith(b"# arcnix:terminal:here\n"):
            # Interpret only the fixed client bootstrap grammar. This fixture
            # never passes remote text or a selected path to the host shell.
            try:
                text = command.decode("utf-8")
                parts = shlex.split(text.split("\n", 1)[1])
                assert len(parts) == 7
                assert parts[:2] == ["cd", "--"]
                assert parts[3:] == ["&&", "exec", "${SHELL:-/bin/sh}", "-i"]
                directory = parts[2]
                assert directory.startswith("/") and len(directory.encode()) <= 16384
                assert not any(ord(character) < 32 or ord(character) == 127 for character in directory)
                destination = Files(self, root=self.fixture_root).path(directory)
                with (self.fixture_root / "terminal-here-requests.jsonl").open("a", encoding="utf-8") as audit:
                    audit.write(json.dumps(dict(directory=directory, script=text), ensure_ascii=False) + "\n")
                if not destination.is_dir():
                    def missing():
                        # Let Paramiko acknowledge the exec request before the
                        # short-lived bootstrap publishes its failing exit.
                        time.sleep(.02)
                        channel.sendall(b"cd: fixture directory does not exist\r\n")
                        channel.send_exit_status(1)
                        channel.close()
                    threading.Thread(target=missing, daemon=True).start()
                    return True
                return self.interactive(channel, directory)
            except (AssertionError, UnicodeError, ValueError, OSError):
                return False
        def reply():
            # Deliberately does not execute arbitrary commands on the developer's machine.
            text = command.decode()
            if self.username == "arcnix-windows":
                from server_apps_fixture import windows_inventory_reply
                windows_inventory_reply(self, channel, text)
                return
            if text.startswith(('# arcnix:browser:repair ', '# arcnix:sync:', '# arcnix:apps:', '# arcnix:desktop:', '# arcnix:docker:', '# arcnix:disks:', '# arcnix:compose:', '# arcnix:software:', '# arcnix:network:', '# arcnix:processes:', '# arcnix:redis:')):
                from server_apps_fixture import reply as application_reply
                if application_reply(self, channel, text):
                    return
            if text.strip() == "fixture-stream":
                channel.sendall(b"first stdout\n")
                time.sleep(.15)
                channel.sendall_stderr(b"first stderr\n")
                time.sleep(.15)
                channel.sendall(b"last stdout\n")
                channel.send_exit_status(9)
            elif text.strip() == "fixture-stream-large":
                for _ in range(100):
                    channel.sendall(b"o" * 8192)
                    channel.sendall_stderr(b"e" * 8192)
                channel.sendall(b"stdout-end\n")
                channel.sendall_stderr(b"stderr-end\n")
                channel.send_exit_status(0)
            elif text.strip() == "fixture-stream-timeout":
                time.sleep(2)
                if channel.closed:
                    return
                channel.send_exit_status(0)
            elif text.strip() == "fixture-stream-input":
                data = bytearray()
                while True:
                    chunk = channel.recv(1024)
                    if not chunk:
                        break
                    data.extend(chunk)
                channel.sendall(b"stdin accepted\n" if data == b"fixture-private-input\n" else b"stdin rejected\n")
                channel.send_exit_status(0 if data == b"fixture-private-input\n" else 1)
            elif text == '# arcnix:record-recovery:stage\nfixture-upload-stream':
                # Backpressure in both directions: output exceeds the SSH
                # window before this fixed fixture starts consuming its input.
                for _ in range(384):
                    channel.sendall(b'o' * 8192)
                    channel.sendall_stderr(b'e' * 8192)
                data = bytearray()
                while True:
                    chunk = channel.recv(65536)
                    if not chunk:
                        break
                    data.extend(chunk)
                channel.sendall(f'\n{len(data)}:{hashlib.sha256(data).hexdigest()}\n'.encode())
                channel.send_exit_status(0)
            elif text == '# arcnix:record-recovery:stage\nfixture-upload-early-exit':
                channel.sendall(b'fixture exited before reading input\n')
                channel.send_exit_status(0)
            elif text.startswith("# arcnix:transfer:replace"):
                # Emulate only the fixed replacement adapter within the fixture
                # sandbox. Never pass commands to a shell on the test machine.
                try:
                    parts = shlex.split(text.splitlines()[-1])
                    assert parts[-5:-2] == ["mv", "-fT", "--"]
                    files = Files(self, root=self.fixture_root)
                    source, target = files.path(parts[-2]), files.path(parts[-1])
                    assert source.is_file() and target.is_file() and not source.is_symlink() and not target.is_symlink()
                    if target.name.startswith("fixture-replace-denied-"):
                        raise PermissionError("Fixture replacement denied")
                    os.replace(source, target)
                    Files.moved(source, target)
                    channel.send_exit_status(0)
                except (OSError, AssertionError, ValueError) as error:
                    channel.sendall_stderr((str(error) + "\n").encode())
                    channel.send_exit_status(1)
            elif text.startswith("# arcnix:configurations:stage"):
                identifier=re.search(r'/config-staging/([a-zA-Z0-9-]+)',text).group(1)
                (self.fixture_root/'home/arcnix/.local/state/arcnix/config-staging'/identifier).mkdir(parents=True)
                channel.send_exit_status(0)
            elif text.startswith('# arcnix:record-cleanup:stage'):
                args=shlex.split(text.split('\n',1)[1]);staging=args[-1]
                assert staging.startswith('/home/arcnix/.local/state/arcnix/cleanup-staging/')
                incoming=self.fixture_root/staging.lstrip('/');incoming.mkdir(parents=True)
                data=bytearray()
                while True:
                    chunk=channel.recv(65536)
                    if not chunk:break
                    data.extend(chunk)
                assert len(data)<=2*1024*1024
                (incoming/'request.json').write_bytes(data)
                channel.send_exit_status(0)
            elif text.startswith(('# arcnix:record-recovery:stage','# arcnix:audit-cleanup:stage','# arcnix:task-stop:stage')):
                staging=shlex.split(text.split('\n',1)[1])[-1]
                assert staging.startswith(('/home/arcnix/.local/state/arcnix/recovery-staging/','/home/arcnix/.local/state/arcnix/audit-cleanup-staging/','/home/arcnix/.local/state/arcnix/task-stop-staging/'))
                incoming=self.fixture_root/staging.lstrip('/');incoming.mkdir(parents=True)
                data=bytearray()
                while True:
                    chunk=channel.recv(65536)
                    if not chunk:break
                    data.extend(chunk)
                assert len(data)<=4*1024*1024
                frame=json.loads(data)
                for key,name in [('source','source.py'),('request','request.json')]:(incoming/name).write_bytes(base64.b64decode(frame[key],validate=True))
                channel.send_exit_status(0)
            elif text.startswith(('# arcnix:record-recovery:discard-stage','# arcnix:audit-cleanup:discard-stage','# arcnix:task-stop:discard-stage')):
                staging=shlex.split(text.split('\n',1)[1])[-1];assert staging.startswith(('/home/arcnix/.local/state/arcnix/recovery-staging/','/home/arcnix/.local/state/arcnix/audit-cleanup-staging/','/home/arcnix/.local/state/arcnix/task-stop-staging/'))
                incoming=self.fixture_root/staging.lstrip('/')
                for name in ['source.py','request.json']:(incoming/name).unlink()
                incoming.rmdir();channel.send_exit_status(0)
            elif text.startswith('# arcnix:record-cleanup:discard-stage'):
                staging=shlex.split(text.split('\n',1)[1])[-1]
                assert staging.startswith('/home/arcnix/.local/state/arcnix/cleanup-staging/')
                incoming=self.fixture_root/staging.lstrip('/')
                (incoming/'request.json').unlink();incoming.rmdir();channel.send_exit_status(0)
            elif text.startswith('# arcnix:trash:environment'):
                if (self.fixture_root / 'fixture-sftp-only').exists():
                    channel.send_exit_status(1)
                    channel.close()
                    return
                channel.sendall(f"/\n/.local/share\n{self.fixture_root.stat().st_uid}\n{time.strftime('%Y-%m-%dT%H:%M:%S')}\n".encode())
                channel.send_exit_status(0)
            elif text.startswith('# arcnix:task-records:home'):
                channel.sendall(b'/home/arcnix');channel.send_exit_status(0)
            elif text.startswith('# arcnix:record-storage:context'):
                uid=1002 if (self.fixture_root/'storage-account-changed').exists() else 1000
                with (self.fixture_root/'storage-context-audit.jsonl').open('a',encoding='utf-8') as audit:audit.write(json.dumps(dict(uid=uid,time=time.monotonic()))+'\n')
                channel.sendall(f'{uid}\n/home/arcnix'.encode());channel.send_exit_status(0)
            elif text.startswith('# arcnix:task-records:'):
                args=shlex.split(text.split('\n',1)[1]);home,action,offset,identifier,expected_root=args[-5:];offset=int(offset)
                root='/home/arcnix/.local/state/arcnix/tasks'
                if (self.fixture_root/'records-root-changed').exists():root='/home/arcnix/changed-tasks'
                root64=base64.b64encode(root.encode()).decode();rows=self.records_for_browser()
                if action=='list':
                    page=rows[offset:offset+100]
                    output=f'ARCNIX_RECORDS_V1\t{int(time.time())}\t{offset}\t{root64}\n'+''.join(self.record_line(row) for row in page)
                    if not (self.fixture_root/'records-truncated').exists():output+=f'ARCNIX_RECORDS_END_V1\t{len(page)}\t{int(len(rows)>offset+100)}\n'
                else:
                    record=next((row for row in rows if row['id']==identifier),None)
                    if record is None:channel.send_exit_status(1);channel.close();return
                    output=f'ARCNIX_RECORD_DETAIL_V1\t{int(time.time())}\t-\t{root64}\n'+self.record_line(record)+'ARCNIX_RECORD_LOG_V1\n'+(record.get('log','') if record.get('bytes',0) is not None else '')
                channel.sendall(output.encode());channel.send_exit_status(0)
            elif text.startswith("# arcnix:tasks:operation-preflight"):
                with self.hosted_tasks_lock:
                    busy = self.operation_busy()
                if busy:
                    channel.sendall_stderr(b"Another Arcnix server operation is running. Query its task before submitting another operation.\n")
                channel.send_exit_status(75 if busy else 0)
            elif text.startswith("# arcnix:tasks:capabilities"):
                if self.username == "arcnix-nosystemd":
                    channel.send_exit_status(125)
                else:
                    channel.sendall(b"/home/arcnix")
                    channel.send_exit_status(0)
            elif text.strip() == "id -u":
                channel.sendall(b"1000\n")
                channel.send_exit_status(0)
            elif text.startswith("# arcnix:tasks:initialize"):
                if '# arcnix:tasks:metadata' in text:
                    line=text.split('# arcnix:tasks:metadata\n',1)[1].strip()
                    args=shlex.split(line)
                    metadata=json.loads(args[2]+',"created":'+str(int(time.time()))+'}')
                    with self.hosted_tasks_lock:self.task_metadata[metadata['id']]=metadata
                channel.send_exit_status(0)
            elif text.startswith("# arcnix:tasks:query"):
                match = re.search(r"/tasks/([a-zA-Z0-9-]+)(?:/|[\s\"'])", text)
                self.trace_task_query(match.group(1) if match else '', 'start')
                delay=self.fixture_root/'task-query-delay'
                if delay.exists():time.sleep(min(3,max(0,float(delay.read_text()))))
                record = self.hosted_tasks.get(match.group(1) if match else "")
                if record:
                    finished = self.task_finished(record)
                    output = "Server-owned fixture job finished · 中文\n" if finished else "Server-owned fixture job started\n"
                    if record.get("backup"):
                        output=json.dumps(dict(message_type="summary" if finished else "status",percent_done=1.0 if finished else .5,bytes_done=1024 if finished else 512,total_bytes=1024,files_done=2 if finished else 1,total_files=2,snapshot_id="a"*64))+"\n"
                    if record.get('configuration'):
                        phase=record['configuration'] if finished else 'publishing'
                        output='ARCNIX_CONFIG_V1\t'+phase+'\t'+match.group(1)+'\n'
                    if record.get('cleanup'):
                        output='ARCNIX_CLEANUP_V1\t'+('completed' if finished else 'frozen')+'\t'+match.group(1)+'\n'
                    if record.get('recovery'):
                        output='ARCNIX_RECOVERY_V1\t'+('completed' if finished else 'exporting')+'\t'+match.group(1)+'\n'
                    if record.get('installation') and finished:
                        output=self.installation_output(match.group(1),record)
                    if record.get('drill') and finished:
                        output=self.drill_output(match.group(1),record)
                    if record.get('blocked'):
                        output='Another Arcnix server operation is running. Query its task before submitting another operation.\n'
                    state = 'inactive' if finished and record['code']==0 else 'failed' if finished else 'active'
                    if finished and record.get('stop_control'):output=self.stop_output(match.group(1))
                    options=self.observation_options()
                    before=options.get('before',state);after=options.get('after',before)
                    code=options.get('saved',record['code'] if finished else None)
                    raw=self.exit_bytes(match.group(1),code,options)
                    saved=base64.b64encode(raw.encode()).decode() if raw is not None else '-'
                    invocation=options.get('invocation','1'*32)
                    header='\t'.join(['ARCNIX_TASK_QUERY_V2','other-task' if options.get('wrong_id') else match.group(1),before,options.get('before_invocation',invocation),after,invocation,saved])
                    if options.get('weak_protocol'):header=str(code)
                    if options.get('truncate'):header=header.rsplit('\t',1)[0]
                    if 'output' in options:output=options['output']
                    self.trace_task_query(match.group(1), 'end')
                    channel.sendall((header + "\nARCNIX_TASK_OUTPUT\n" + output).encode())
                    channel.send_exit_status(0)
                else:
                    self.trace_task_query(match.group(1) if match else '', 'end')
                    channel.sendall_stderr(b"No such server task\n")
                    channel.send_exit_status(1)
            elif text.startswith("# arcnix:operations:identity"):
                channel.sendall(b"ARCNIX_UID\t1000\nARCNIX_SUDO\t1\n")
                channel.send_exit_status(0)
            elif text.startswith("# arcnix:services:list"):
                if self.username == "arcnix-nosystemd":
                    channel.sendall_stderr(b"Service management requires a running systemd system manager.\n")
                    channel.send_exit_status(125)
                else:
                    for record in list(self.hosted_tasks.values()):
                        self.task_finished(record)
                    for unit, state in self.services.items():
                        channel.sendall(f"{unit} loaded {state} {'running' if state == 'active' else 'dead'} Fixture {unit}\n".encode())
                    if 'ARCNIX_UNIT_FILES' in text:
                        states=self.fixture_root/'services-startup.json'
                        values=json.loads(states.read_text()) if states.exists() else {}
                        channel.sendall(b'ARCNIX_UNIT_FILES\n')
                        for unit in self.services:
                            channel.sendall(f"{unit} {values.get(unit,'enabled' if unit=='nginx.service' else 'disabled')} enabled\n".encode())
                        channel.sendall(b'not-loaded.service disabled disabled\n')
                    channel.send_exit_status(0)
            elif text.startswith("# arcnix:services:inspect"):
                unit = shlex.split(text.splitlines()[-1])[-2]
                state = self.services.get(unit, "not-found")
                channel.sendall(f"Id={unit}\nLoadState={'loaded' if state != 'not-found' else state}\nActiveState={state}\nMainPID=123\n".encode())
                channel.send_exit_status(0)
            elif text.startswith("# arcnix:operations:execute"):
                # Interpret only fixed test cases; never invoke a host OS shell.
                args = shlex.split(text.split("\n", 1)[1])
                script = args[-1]
                data = bytearray()
                while True:
                    chunk = channel.recv(1024)
                    if not chunk: break
                    data.extend(chunk)
                if "sudo" in args and "-S" in args:
                    if data != b"fixture-admin-secret\n":
                        channel.sendall_stderr(b"sudo: incorrect password\n")
                        channel.send_exit_status(1);channel.close();return
                if '# arcnix:configuration-compare:read' in script:
                    request=json.loads(base64.b64decode(shlex.split(script.split('\n',1)[1])[-1]));assert request['version']==1
                    with (self.fixture_root/'configuration-compare-queries.jsonl').open('a') as audit:audit.write(json.dumps(dict(path=request['path'],administrator='sudo' in args))+'\n')
                    fixture=self.fixture_root/'configuration-compare-fixture.json';values=json.loads(fixture.read_text(encoding='utf-8')) if fixture.exists() else {}
                    if (self.fixture_root/'configuration-compare-slow').exists():time.sleep(2)
                    else:time.sleep(.2)
                    if values.get('invalid_json'):channel.sendall(b'invalid JSON');channel.send_exit_status(0);channel.close();return
                    if values.get('oversized'):channel.sendall(b'x'*(256*1024+1));channel.send_exit_status(0);channel.close();return
                    issue=values.get('issue') or ('missing' if values.get('path',request['path'])!=request['path'] else None)
                    body=values.get('content','worker_processes 2;\n').encode();snapshot=None if issue else dict(canonical=values.get('canonical',request['path']),bytes=len(body),sha256=values.get('sha256',hashlib.sha256(body).hexdigest()),content=base64.b64encode(body).decode(),uid=values.get('uid',0),gid=values.get('gid',0),mode=values.get('mode',420),symlink=values.get('symlink',False))
                    payload=dict(version=1,path=values.get('wrong_path',request['path']),sampled=1794759400,snapshot=snapshot,issue=issue)
                    if values.get('warning'):channel.sendall_stderr(values['warning'].encode())
                    channel.sendall(json.dumps(payload).encode());channel.send_exit_status(0);channel.close();return
                if '# arcnix:resource-ports:read' in script or '# arcnix:resource-ports:terminate' in script:
                    self.resource_ports_reply(channel, script, 'sudo' in args)
                    return
                if '# arcnix:installers:transaction' in script:
                    self.installation_snapshot_reply(channel,script);return
                if '# arcnix:configuration-records:' in script:
                    self.configuration_browser_reply(channel,script);return
                if '# arcnix:backups:files' in script and '# arcnix:tasks:launch' not in script:
                    self.snapshot_files_reply(channel,script);return
                if '# arcnix:record-storage:' in script:
                    self.record_storage_reply(channel,script);return
                if '# arcnix:cleanup-records:' in script:
                    self.cleanup_browser_reply(channel,script);return
                if '# arcnix:audit-storage:' in script:
                    self.audit_storage_reply(channel,script);return
                if '# arcnix:audit-details:read' in script:
                    self.audit_details_reply(channel,script);return
                if '# arcnix:export-download:' in script:
                    self.export_download_reply(channel,script);return
                if '# arcnix:task-stop:inspect' in script:
                    if (self.fixture_root/'stops-inspect-slow').exists():time.sleep(2)
                    if (self.fixture_root/'stops-inspect-denied').exists():
                        channel.sendall_stderr(b'Cannot inspect tasks: fixture-admin-secret denied');channel.send_exit_status(2);channel.close();return
                    parts=shlex.split(script.split('# arcnix:task-stop:inspect\n',1)[1]);selection=json.loads(zlib.decompress(base64.b64decode(parts[-1])))
                    rows=[]
                    for identifier in selection['ids']:
                        ready='foreign' not in identifier
                        row=dict(id=identifier,state='ready' if ready else 'refused',reason='' if ready else 'foreign_unit',service='active',uid=0 if ready else None,invocation='1'*32 if ready else None,unit_hash='a'*64 if ready else None,directory_identity='b'*64 if ready else None,output_identity='c'*64 if ready else None,metadata_identity=None,cgroup='/system.slice/arcnix-'+identifier+'.service' if ready else None,cgroup_identity='d'*64 if ready else None,pid=123 if ready else 0,process_start=456 if ready else 0)
                        rows.append(row)
                    payload=dict(version=1,sampled=int(time.time()),selection=selection,root_identity='e'*64,rows=rows)
                    if (self.fixture_root/'stops-inspect-wrong').exists():payload['selection']['account']['uid']=1234
                    with (self.fixture_root/'stops-inspections.jsonl').open('a') as audit:audit.write(json.dumps(payload)+'\n')
                    channel.sendall(json.dumps(payload).encode());channel.send_exit_status(0);channel.close();return
                if '# arcnix:task-stop:prepare' in script:
                    parts=shlex.split(script.split('# arcnix:task-stop:prepare\n',1)[1]);staging,identifier,source_hash,request_hash,uid=parts[-5:]
                    incoming=self.fixture_root/staging.lstrip('/');raw=(incoming/'request.json').read_bytes();source=(incoming/'source.py').read_bytes()
                    assert hashlib.sha256(source).hexdigest()==source_hash and hashlib.sha256(raw).hexdigest()==request_hash
                    request=json.loads(raw);assert request['selection']['account']['uid']==int(uid)
                    self.task_stops[identifier]=dict(plan=request,revision=source_hash[:16])
                    if (self.fixture_root/'stops-prepare-delay').exists():time.sleep(2)
                    payload=dict(version=1,id=identifier,request_sha256=request_hash,runner_sha256='f'*64,candidates=len(request['rows']))
                    if (self.fixture_root/'stops-prepare-wrong').exists():payload['request_sha256']='0'*64
                    channel.sendall(json.dumps(payload).encode());channel.send_exit_status(0);channel.close();return
                if '# arcnix:record-recovery:prepare' in script:
                    parts=shlex.split(script.split('# arcnix:record-recovery:prepare\n',1)[1]);staging,identifier,source_hash,request_hash,uid=parts[-5:]
                    incoming=self.fixture_root/staging.lstrip('/')
                    assert hashlib.sha256((incoming/'source.py').read_bytes()).hexdigest()==source_hash and hashlib.sha256((incoming/'request.json').read_bytes()).hexdigest()==request_hash
                    request=json.loads((incoming/'request.json').read_bytes());assert request['source']['quarantine_complete'] and request['source']['quarantine_identity']
                    if (self.fixture_root/'recovery-preflight-denied').exists():
                        channel.sendall_stderr(b'Residual evidence changed; inspect and review again\n');channel.send_exit_status(2);channel.close();return
                    self.recovery_records[identifier]=request
                    count=sum(o['quarantine']['state'] in ['matching','changed'] for r in request['source']['records'] for o in r['objects'])
                    export_id=identifier if request['action']=='export' else request['export_id']
                    payload=dict(version=1,id=identifier,request_sha256='a'*64,runner_sha256='b'*64,destination='/var/lib/arcnix/record-recoveries/'+export_id+'/files',file_count=count)
                    channel.sendall(json.dumps(payload).encode());channel.send_exit_status(0);channel.close();return
                if '# arcnix:audit-cleanup:prepare' in script:
                    parts=shlex.split(script.split('# arcnix:audit-cleanup:prepare\n',1)[1]);staging,identifier,source_hash,request_hash,uid=parts[-5:]
                    incoming=self.fixture_root/staging.lstrip('/');raw=(incoming/'request.json').read_bytes()
                    assert hashlib.sha256((incoming/'source.py').read_bytes()).hexdigest()==source_hash and hashlib.sha256(raw).hexdigest()==request_hash
                    if (self.fixture_root/'audit-cleanup-preflight-denied').exists():
                        channel.sendall_stderr(b'Audit objects changed; inspect and review again\n');channel.send_exit_status(2);channel.close();return
                    request=json.loads(raw);self.audit_cleanups[identifier]=request
                    seed=self.fixture_root/'audit-storage-fixture.json';stores=json.loads(seed.read_text());stores.setdefault('maintenance',[]).append(dict(id=identifier,modified=int(time.time()),phase='prepared',reasons=['maintenance']));seed.write_text(json.dumps(stores))
                    payload=dict(version=1,id=identifier,request_sha256=request_hash,runner_sha256='c'*64,candidates=len(request['preview']['records']))
                    if (self.fixture_root/'audit-cleanup-wrong-request').exists():payload['request_sha256']='0'*64
                    channel.sendall(json.dumps(payload).encode());channel.send_exit_status(0);channel.close();return
                if '# arcnix:record-cleanup:prepare' in script:
                    parts=shlex.split(script.split('# arcnix:record-cleanup:prepare\n',1)[1]);identifier,staging,expected,uid=parts[-4:]
                    raw=(self.fixture_root/staging.lstrip('/')/'request.json').read_bytes()
                    assert hashlib.sha256(raw).hexdigest()==expected
                    plan=json.loads(raw)
                    if (self.fixture_root/'cleanup-preflight-denied').exists():
                        channel.sendall_stderr(b'Cleanup objects changed; inspect storage again\n');channel.send_exit_status(2);channel.close();return
                    assert plan['account']['uid']==int(uid) and plan['records'] and all(row['eligible'] for row in plan['records'])
                    self.cleanup_records[identifier]=plan
                    payload=dict(version=1,id='wrong-cleanup-id' if (self.fixture_root/'cleanup-wrong-id').exists() else identifier,request_sha256='e'*64,runner_sha256='f'*64,candidates=len(plan['records']))
                    channel.sendall(json.dumps(payload).encode());channel.send_exit_status(0);channel.close();return
                if '# arcnix:configurations:preview' in script:
                    identifier=re.search(r'/config-staging/([a-zA-Z0-9-]+)',script).group(1)
                    incoming=self.fixture_root/'home/arcnix/.local/state/arcnix/config-staging'/identifier
                    request=json.loads((incoming/'request.json').read_bytes())
                    if request['restore_from'] and (self.fixture_root/'configuration-strict-restore').exists():
                        prior=self.configuration_records[request['restore_from']]
                        assert prior['spec']==request['spec'] and prior['restore_from'] is None,'Restore must use this server original candidate settings'
                    data=(incoming/'candidate').read_bytes() if not request['restore_from'] else b'fixture previous version\n'
                    assert request['restore_from'] or hashlib.sha256(data).hexdigest()==request['spec']['sha256']
                    with self.hosted_tasks_lock:self.configuration_records[identifier]=request
                    preview=dict(old_sha256='b'*64,new_sha256=hashlib.sha256(data).hexdigest(),record='/var/lib/arcnix/configurations/'+identifier,path=request['spec']['path'],mode='0o640',owner='0:0',diff='--- original\n+++ candidate\n-before\n+'+data.decode())
                    channel.sendall(json.dumps(preview,ensure_ascii=False).encode());channel.send_exit_status(0);channel.close();return
                if "sudo" in args and "-S" in args and "snapshots --json" not in script:
                    channel.sendall(b"echo fixture-admin-")
                    channel.sendall(b"secret\n")
                if "# arcnix:tasks:launch" in script:
                    if (self.fixture_root/'task-launch-delay').exists():time.sleep(2)
                    unit = re.search(r"--unit='arcnix-([a-zA-Z0-9-]+)'", script)
                    assert unit, "Expected a generated systemd unit"
                    task_id = unit.group(1)
                    assert task_id not in self.hosted_tasks, "A server task must not be submitted twice"
                    if (self.fixture_root/'trace-task-queries').exists():
                        with (self.fixture_root/'task-launches.jsonl').open('a',encoding='utf-8') as audit:
                            audit.write(json.dumps(dict(id=task_id,at=time.time_ns()))+'\n')
                    policy=re.search(r"arcnix-policy:([a-f0-9-]+)",script)
                    if policy:
                        if "arcnix-database:postgresql" in script:self.database_policies[policy.group(1)]="postgresql"
                        elif "arcnix-database:mysql" in script:self.database_policies[policy.group(1)]="mysql"
                    with self.hosted_tasks_lock:
                        exclusive = '# arcnix:tasks:exclusive' in script
                        blocked = exclusive and self.operation_busy()
                        held = 'fixture-hold-operation' in script
                        service = re.search(r'systemctl --no-pager --no-ask-password (start|stop|restart|enable|disable) -- .*?(nginx\.service|worker\.service|broken\.service)', script)
                        self.hosted_tasks[task_id] = dict(finish=time.monotonic()+(0 if blocked else 120 if held else 4), code=75 if blocked else 7 if "fixture-command-fail" in script else 0, exclusive=exclusive, held=held, blocked=blocked, service=(service.group(2), service.group(1)) if service else None, install_nginx='# arcnix:packages:install' in script and 'nginx' in script, backup="# arcnix:backups:execute" in script)
                        if any(marker in script for marker in ['# arcnix:compose:','# arcnix:software:mutate','# arcnix:startup:install']):
                            self.hosted_tasks[task_id].update(application_script=script,application_user=self.username)
                        if '# arcnix:installers:v1' in script and not blocked:
                            installation=self.installation_definition(script);self.hosted_tasks[task_id]['installation']=installation
                            if installation['mode'] in ['environment-change','state-change','plan-change','protocol-v2','apt-fail','check-fail','service-fail']:self.hosted_tasks[task_id]['code']=1
                        if '# arcnix:drills:v1' in script and not blocked:
                            request=self.drill_definition(script);self.hosted_tasks[task_id]['drill']=request
                            if request['mode'] in ['check-fail','restore-fail','interrupted']:self.hosted_tasks[task_id]['code']=1
                    if task_id in self.task_stops:
                        assert not exclusive,'A stop controller must not wait for its target operation lock'
                        self.hosted_tasks[task_id]['stop_control']=task_id
                    if task_id in self.configuration_records:
                        request=self.configuration_records[task_id]
                        if 'fixture-submit-fail' in request['spec']['path']:
                            del self.hosted_tasks[task_id]
                            channel.send_exit_status(1);channel.close();return
                        phase='restored' if request['restore_from'] else 'published'
                        if 'fixture-health-fail' in request['spec']['path']:phase='rolled_back'
                        self.hosted_tasks[task_id]['configuration']=phase
                        if phase=='rolled_back':self.hosted_tasks[task_id]['code']=1
                    if task_id in self.cleanup_records:self.hosted_tasks[task_id]['cleanup']=task_id
                    if task_id in self.audit_cleanups:
                        self.hosted_tasks[task_id]['audit_cleanup']=task_id
                        if (self.fixture_root/'audit-cleanup-runtime-fail').exists():self.hosted_tasks[task_id]['code']=9
                    if task_id in self.recovery_records:
                        self.hosted_tasks[task_id]['recovery']=task_id
                        if (self.fixture_root/'recovery-fail-runtime').exists():self.hosted_tasks[task_id]['code']=9
                    channel.send_exit_status(0)
                    channel.close()
                    return
                if "# arcnix:backups:" in script:
                    if "snapshots --json" in script:
                        tags = re.search(r"arcnix-policy:([^,']+),arcnix-server:([^']+)", script)
                        assert tags, "Expected policy and source tags"
                        with (self.fixture_root/'backup-snapshot-queries.jsonl').open('a',encoding='utf-8') as audit:
                            audit.write(json.dumps(dict(policy=tags.group(1),origin=tags.group(2)))+'\n')
                        if (self.fixture_root/'snapshots-delay').exists():time.sleep(1)
                        if (self.fixture_root/'snapshots-fail').exists():
                            channel.sendall_stderr(b'fixture snapshot read failed arcnix-test fixture-admin-secret\n');channel.send_exit_status(2);channel.close();return
                        ids = re.findall(r"'([a-f0-9]{64})'", script)
                        snapshot_id = ids[-1] if ids else "a"*64
                        snapshots = [dict(id=snapshot_id,time="2026-11-14T08:00:00Z",hostname="fixture 中文",paths=["/srv/site"],tags=["arcnix-policy:"+tags.group(1),"arcnix-server:"+tags.group(2)])]
                        engine=self.database_policies.get(tags.group(1))
                        engines=self.fixture_root/'database-policies-fixture.json'
                        if not engine and engines.exists():engine=json.loads(engines.read_text()).get(tags.group(1))
                        if engine:
                            snapshots[0]['tags'].append('arcnix-database:'+engine)
                            snapshots[0]['paths']=['/staging/arcnix-db-'+tags.group(1)]
                        if (self.fixture_root/'snapshots-wrong-source').exists():
                            snapshots[0]['tags']=[t for t in snapshots[0]['tags'] if not t.startswith('arcnix-server:')]+['arcnix-server:another-source']
                        payload=json.dumps(snapshots,ensure_ascii=False).encode()
                        for offset in range(0,len(payload),7): channel.sendall(payload[offset:offset+7])
                    elif "forget --dry-run" in script:
                        channel.sendall(b"Retention preview: remove old matching snapshots; keep latest 7.\n")
                    channel.send_exit_status(0)
                    channel.close()
                    return
                channel.sendall(b"Operation started\n")
                time.sleep(.3)
                if "systemctl" in script:
                    parts = shlex.split(script)
                    unit = parts[-1]
                    verb = parts[-3]
                    if unit not in self.services or verb not in ("start", "stop", "restart"):
                        channel.sendall_stderr(b"Unsupported fixture service action\n")
                        channel.send_exit_status(1)
                    else:
                        self.services[unit] = "inactive" if verb == "stop" else "active"
                        channel.sendall(f"Service {unit} {self.services[unit]}\n".encode())
                        channel.send_exit_status(0)
                elif script.strip() == "fixture-command-fail":
                    channel.sendall_stderr(b"Fixture command failed\n")
                    channel.send_exit_status(7)
                else:
                    channel.sendall("Fixture command completed · 中文\n".encode())
                    channel.send_exit_status(0)
            elif text == "uname -s":
                platform = self.fixture_root / 'host-uname.txt'
                channel.sendall(platform.read_bytes() if platform.exists() else b"Linux\n")
                channel.send_exit_status(0)
            elif text.startswith("# arcnix:metrics:darwin"):
                channel.sendall((self.fixture_root / 'darwin-metrics.txt').read_bytes())
                channel.send_exit_status(0)
            elif text.startswith(("# arcnix:gpu:linux", "# arcnix:gpu:macos")):
                counters = self.fixture_root / 'gpu-counters.txt'
                if counters.exists():
                    channel.sendall(counters.read_bytes())
                channel.send_exit_status(0)
            elif text.startswith("# arcnix:metrics:sample"):
                self.metrics_sample += 1
                count = self.metrics_sample
                options_path=self.fixture_root/'metrics-fixture.json'
                options=json.loads(options_path.read_text(encoding='utf-8')) if options_path.exists() else {}
                if options.get('trace'):
                    with (self.fixture_root/'metrics-queries.jsonl').open('a',encoding='utf-8') as output:output.write(json.dumps(dict(count=count,at=time.monotonic()))+'\n')
                if options.get('delay'):time.sleep(float(options['delay']))
                if options.get('error'):
                    channel.sendall_stderr(b'Fixture resource read failed\n');channel.send_exit_status(1);channel.close();return
                if options.get('reset'):count=1
                payload = f"""ARCNIX_METRICS_V1
ARCNIX_CPU_BEGIN
cpu  {100 + count * 60} 20 30 {400 + count * 140} 10 5 5 0 20 0
cpu0 50 10 15 200 5 2 3 0 10 0
cpu1 50 10 15 200 5 3 2 0 10 0
ARCNIX_CPU_END
ARCNIX_MEMORY_BEGIN
MemTotal: 1048576 kB
MemAvailable: 524288 kB
SwapTotal: 262144 kB
SwapFree: 196608 kB
ARCNIX_MEMORY_END
ARCNIX_LOAD_BEGIN
0.25 0.50 0.75 1/100 321
ARCNIX_LOAD_END
ARCNIX_UPTIME_BEGIN
{1000 + count * 3}.00 1800.00
ARCNIX_UPTIME_END
ARCNIX_NETWORK_BEGIN
Inter-| Receive | Transmit
 face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed
lo: 10 1 0 0 0 0 0 0 10 1 0 0 0 0 0 0
eth0: {10000 + count * 3072} 10 0 0 0 0 0 0 {20000 + count * 6144} 20 0 0 0 0 0 0
ARCNIX_NETWORK_END
ARCNIX_DISK_BEGIN
Filesystem 1024-blocks Used Available Capacity Mounted on
/dev/root 1048576 262144 786432 25% /
/dev/data 2097152 1048576 1048576 50% /srv/my data
ARCNIX_DISK_END
ARCNIX_METRICS_END
"""
                if options.get('missing_memory'):payload=payload.replace('MemTotal: 1048576 kB','MemTotal: unavailable')
                if options.get('missing_network'):payload='\n'.join(line for line in payload.splitlines() if not line.startswith('eth0:'))+'\n'
                channel.sendall(payload.encode())
                channel.send_exit_status(0)
            elif text.startswith("# arcnix:logs:discover"):
                with (self.fixture_root / 'system-log-queries.jsonl').open('a') as audit:
                    audit.write(json.dumps(dict(action='discover')) + '\n')
                channel.sendall(b"ARCNIX_LOGS_V1\njournal\nsyslog\nauth\n")
                channel.send_exit_status(0)
            elif text.startswith("# arcnix:logs:read"):
                with (self.fixture_root / 'system-log-queries.jsonl').open('a') as audit:
                    audit.write(json.dumps(dict(action='read')) + '\n')
                if "denied.service" in text:
                    channel.sendall_stderr(b"Permission denied reading system journal\n")
                    channel.send_exit_status(1)
                elif "empty.service" in text:
                    channel.send_exit_status(0)
                elif "colors.service" in text:
                    # journalctl exports a message with control characters as a byte array.
                    record = dict(__REALTIME_TIMESTAMP="1794499320000000", _SYSTEMD_UNIT="colors.service", PRIORITY="6", MESSAGE=list(b"\x1b[32mFixture colored line\x1b[0m done"))
                    channel.sendall((json.dumps(record) + "\n").encode())
                    channel.send_exit_status(0)
                elif "journalctl" in text:
                    records = [dict(__REALTIME_TIMESTAMP="1794499200000000", _SYSTEMD_UNIT="ssh.service", PRIORITY="6", MESSAGE="Accepted publickey for arcnix"),
                               dict(__REALTIME_TIMESTAMP="1794499260000000", _SYSTEMD_UNIT="nginx.service", PRIORITY="4", MESSAGE="Fixture service warning\nSecond line preserved")]
                    if "ssh.service" in text:
                        records = records[:1]
                    if "--priority=3" in text or "-p 3" in text:
                        records = []
                    channel.sendall(("\n".join(json.dumps(record) for record in records) + "\n").encode())
                    channel.send_exit_status(0)
                else:
                    channel.sendall(b"Nov 12 08:00:00 fixture sshd[321]: Accepted publickey for arcnix\nNov 12 08:01:00 fixture kernel: Fixture file log entry\n")
                    channel.send_exit_status(0)
            elif text.startswith("# arcnix:installers:transaction"):
                self.installation_snapshot_reply(channel,text);return
            elif text.startswith("# arcnix:installers:versions"):
                if (self.fixture_root/'installer-downgrade').exists():channel.sendall_stderr(b'APT downgrade was refused\n');channel.send_exit_status(1)
                else:channel.sendall(b'ARCNIX_APT_VERSIONS_OK\n');channel.send_exit_status(0)
            elif text.startswith("# arcnix:installers:before"):
                names=shlex.split(next(line for line in text.splitlines() if line.startswith('for package in ')).removeprefix('for package in ').removesuffix('; do'))
                rows=[]
                for name in names:
                    installed=self.nginx_installed or name.startswith('curl')
                    rows.append('ARCNIX_PACKAGE_V1\t'+name+'\t'+(name+'\t1.24.0\tii \tamd64' if installed else '-\t-\tmissing\t-')+'\n')
                output=''.join(rows)
                if (self.fixture_root/'installer-before-truncated').exists():output=''
                channel.sendall(output.encode());channel.send_exit_status(0)
            elif text.startswith("# arcnix:packages:"):
                marker = text.splitlines()[0]
                if marker.endswith(":detect"):
                    os_id='alpine' if (self.fixture_root/'installer-unsupported').exists() else 'fedora' if (self.fixture_root/'software-manager').exists() else 'ubuntu'
                    manager=(self.fixture_root/'software-manager').read_text().strip() if (self.fixture_root/'software-manager').exists() else 'apt'
                    pretty='Fedora test' if os_id=='fedora' else 'Alpine test' if os_id=='alpine' else 'Ubuntu 24.04 LTS'
                    sudo = int(not (self.fixture_root/'software-no-sudo').exists())
                    channel.sendall(f'ARCNIX_OS_BEGIN\nPRETTY_NAME="{pretty}"\nID={os_id}\nVERSION_ID="24.04"\nARCNIX_OS_END\nARCNIX_MANAGER\t{manager}\nARCNIX_UID\t1000\nARCNIX_SUDO\t{sudo}\nARCNIX_ARCH\tamd64\n'.encode())
                elif marker.endswith(":search"):
                    channel.sendall(b"nginx - web server\ncurl - transfer tool\n")
                elif marker.endswith(":installed"):
                    channel.sendall(b"curl\t8.5\tii \ttransfer tool\n")
                    if self.nginx_installed:
                        channel.sendall(b"nginx\t1.24\tii \tweb server\n")
                elif marker.endswith(":verify"):
                    channel.sendall(b"Package: nginx\nVersion: 1.24\nArchitecture: amd64\n\nPackage: curl\nVersion: 8.5\nArchitecture: amd64\n")
                elif marker.endswith(":preview"):
                    channel.sendall(b"Reading package lists...\nInst nginx (1.24 Ubuntu [amd64])\nConf nginx (1.24 Ubuntu [amd64])\n")
                elif marker.endswith(":install") or marker.endswith(":update"):
                    channel.sendall(b"Reading package lists...\n")
                    time.sleep(.35)
                    channel.sendall(b"Processing package operation...\n")
                    time.sleep(.35)
                    channel.sendall(b"Package operation completed (isolated fixture).\n")
                    if marker.endswith(":install"):
                        type(self).nginx_installed = True
                else:
                    channel.sendall_stderr(b"Unsupported fixture package marker\n")
                    channel.send_exit_status(1)
                    channel.close()
                    return
                channel.send_exit_status(0)
            elif text.strip() == "fixture-output":
                channel.sendall(b"fixture stdout\n")
                channel.sendall_stderr(b"fixture stderr\n")
                channel.send_exit_status(7)
            else:
                channel.sendall(b"Arcnix isolated SSH fixture\n")
                channel.send_exit_status(0)
            channel.close()
        threading.Thread(target=reply, daemon=True).start()
        return True


class DelayedHandle(paramiko.SFTPHandle):
    """Optional latency for visible transfer progress in loopback UI tests."""
    delay = 0
    range_trace = None
    upload_gate = None
    def stat(self):
        stream = getattr(self, 'readfile', None) or getattr(self, 'writefile', None)
        if stream is None:
            return paramiko.SFTP_FAILURE
        return paramiko.SFTPAttributes.from_stat(os.fstat(stream.fileno()))
    def read(self, offset, length):
        if self.delay: time.sleep(self.delay)
        data = super().read(offset, length)
        if self.range_trace and isinstance(data, bytes):
            with self.range_trace.open('a') as trace:
                trace.write(f'{offset} {len(data)}\n')
        return data
    def write(self, offset, data):
        if self.upload_gate and self.upload_gate.exists():
            self.upload_gate.with_suffix('.started').touch()
            deadline = time.monotonic() + 20
            while self.upload_gate.exists() and time.monotonic() < deadline:
                time.sleep(.01)
            if self.upload_gate.with_suffix('.drop').exists():
                self.transport.close()
                return paramiko.SFTP_FAILURE
        if self.delay: time.sleep(self.delay)
        return super().write(offset, data)
    def close(self):
        super().close()
        if self.range_trace:
            with self.range_trace.with_suffix('.closed').open('a', encoding='utf-8') as trace:
                trace.write(self.media_path + '\n')


class Files(paramiko.SFTPServerInterface):
    # Windows cannot represent Linux mode bits; retain them for protocol tests.
    modes = {}
    aliases = {}

    @classmethod
    def attributes(cls, path):
        attributes = paramiko.SFTPAttributes.from_stat(path.lstat())
        if str(path) in cls.aliases:
            attributes.st_mode = stat.S_IFLNK | 0o777
        if str(path) in cls.modes:
            attributes.st_mode = (attributes.st_mode & ~0o7777) | cls.modes[str(path)]
        return attributes

    @classmethod
    def moved(cls, source, target):
        mode = cls.modes.pop(str(source), None)
        cls.modes.pop(str(target), None)
        if mode is not None:
            cls.modes[str(target)] = mode

    def __init__(self, server, *args, root, **kwargs):
        super().__init__(server, *args, **kwargs)
        self.root = Path(root).resolve()
        self.transport = server.transport

    def raw_path(self, path):
        target = (self.root / path.lstrip("/")).resolve()
        if not target.is_relative_to(self.root):
            raise PermissionError(errno.EACCES, "Outside test root")
        return target

    def path(self, path):
        target = self.raw_path(path)
        for _ in range(32):
            alias = self.aliases.get(str(target))
            if alias is None:
                return target
            target = self.raw_path(alias)
        raise OSError(errno.ELOOP, "Too many links")

    def canonicalize(self, path):
        if path == "/__fixture_non_utf8_canonical__":
            return b"/invalid-\xff"
        if path == "/__fixture_disconnect__":
            self.transport.close()
            return "/"
        try:
            relative = self.path(path).relative_to(self.root).as_posix()
            return "/" if relative == "." else "/" + relative
        except OSError:
            return "/"

    @staticmethod
    def error(exc):
        return paramiko.SFTPServer.convert_errno(exc.errno or errno.EIO)

    def list_folder(self, path):
        try:
            if "fixture-slow-" in path: time.sleep(.08)
            if path == "/__fixture_delay__":
                time.sleep(1.5)
                path = "/"
            rows = []
            # Real SFTP servers return these raw directory entries too.
            for name in (".", ".."):
                item = self.attributes(self.path(path))
                item.filename = name
                rows.append(item)
            if path.startswith("/__fixture_non_utf8_names__-"):
                for name in (b"bad-\xff", b"bad-\xfe"):
                    item = self.attributes(self.path(path))
                    item.filename = name
                    rows.append(item)
            for child in self.path(path).iterdir():
                try:
                    item = self.attributes(child)
                except FileNotFoundError:
                    # Parallel transfers may publish/remove a staging file
                    # between enumeration and lstat; the directory still exists.
                    continue
                item.filename = child.name
                rows.append(item)
            return rows
        except OSError as exc:
            return self.error(exc)

    def stat(self, path):
        try:
            return self.attributes(self.path(path))
        except OSError as exc:
            return self.error(exc)

    def lstat(self, path):
        try:
            if "fixture-slow-" in path: time.sleep(.08)
            return self.attributes(self.raw_path(path))
        except OSError as exc:
            return self.error(exc)

    def symlink(self, target_path, path):
        try:
            self.raw_path(target_path)
            link = self.raw_path(path)
            link.touch(exist_ok=False)
            self.aliases[str(link)] = target_path
            return paramiko.SFTP_OK
        except OSError as exc:
            return self.error(exc)

    def readlink(self, path):
        try:
            return self.aliases.get(str(self.raw_path(path)), paramiko.SFTP_NO_SUCH_FILE)
        except OSError as exc:
            return self.error(exc)

    def open(self, path, flags, attr):
        try:
            fd = os.open(self.path(path), flags | getattr(os, "O_BINARY", 0), attr.st_mode or 0o600)
            if flags & os.O_CREAT:
                self.modes[str(self.path(path))] = (attr.st_mode or 0o600) & 0o7777
            mode = "r+b" if flags & os.O_RDWR else ("wb" if flags & os.O_WRONLY else "rb")
            stream = os.fdopen(fd, mode)
            handle = DelayedHandle(flags)
            if flags & os.O_WRONLY and Path(path).name.startswith('.arcnix-') and path.endswith('.part'):
                handle.upload_gate = self.root.parent / 'pause-office-upload'
                handle.transport = self.transport
            trace = self.root.parent / 'video-reads.txt'
            if path.lower().endswith(('.mp4', '.mkv')) and trace.exists():
                handle.range_trace = trace
                handle.media_path = path
            if not flags & os.O_WRONLY:
                handle.readfile = stream
            if flags & (os.O_WRONLY | os.O_RDWR):
                handle.writefile = stream
            return handle
        except OSError as exc:
            return self.error(exc)

    def remove(self, path):
        if "fixture-delete-denied-" in path:
            return paramiko.SFTP_PERMISSION_DENIED
        try:
            if "fixture-slow-" in path: time.sleep(.08)
            target = self.raw_path(path)
            target.unlink()
            self.modes.pop(str(target), None)
            self.aliases.pop(str(target), None)
            return paramiko.SFTP_OK
        except OSError as exc:
            return self.error(exc)

    def mkdir(self, path, attr):
        try:
            self.path(path).mkdir(mode=attr.st_mode or 0o755)
            self.modes[str(self.path(path))] = (attr.st_mode or 0o755) & 0o7777
            return paramiko.SFTP_OK
        except OSError as exc:
            return self.error(exc)

    def rmdir(self, path):
        if "fixture-delete-denied-" in path:
            return paramiko.SFTP_PERMISSION_DENIED
        try:
            if "fixture-slow-" in path: time.sleep(.08)
            self.path(path).rmdir()
            return paramiko.SFTP_OK
        except OSError as exc:
            return self.error(exc)

    def rename(self, oldpath, newpath):
        if oldpath == '/fixture-trash-denied.txt':
            return paramiko.SFTP_PERMISSION_DENIED
        try:
            target = self.raw_path(newpath)
            if target.exists():
                return paramiko.SFTP_FAILURE
            source = self.raw_path(oldpath)
            source.rename(target)
            self.moved(source, target)
            alias = self.aliases.pop(str(source), None)
            if alias is not None:
                self.aliases[str(target)] = alias
            return paramiko.SFTP_OK
        except OSError as exc:
            return self.error(exc)

    def posix_rename(self, oldpath, newpath):
        try:
            source = self.path(oldpath)
            target = self.path(newpath)
            if source.is_dir() or target.is_dir() or target.is_symlink():
                return paramiko.SFTP_FAILURE
            os.replace(source, target)
            self.moved(source, target)
            return paramiko.SFTP_OK
        except OSError as exc:
            return self.error(exc)

    def chattr(self, path, attributes):
        if "fixture-mode-denied-" in path:
            return paramiko.SFTP_PERMISSION_DENIED
        try:
            target = self.path(path)
            target.stat()
            if attributes.st_mode is not None:
                self.modes[str(target)] = attributes.st_mode & 0o7777
            return paramiko.SFTP_OK
        except OSError as exc:
            return self.error(exc)


_DNS_PROXY_ADDRESSES = ipaddress.ip_network('198.18.0.0/15')


def connect_browser_destination(host, port, local):
    if local:
        return socket.create_connection(
            ('127.0.0.1' if host in ('browser-fixture.test', 'assets.browser-fixture.test', 'scripts.browser-fixture.test') else host, port), timeout=5)

    try:
        ipaddress.ip_address(host)
        from_dns = False
    except ValueError:
        from_dns = True
    failure = OSError('Public web preview requires a public IP address')
    for _, _, _, _, address in socket.getaddrinfo(host, port, type=socket.SOCK_STREAM):
        ip = ipaddress.ip_address(address[0])
        # Some local network proxies return a synthetic DNS address in this
        # range and route it to the public hostname. Reject direct literals.
        dns_proxy_address = from_dns and ip in _DNS_PROXY_ADDRESSES
        if not dns_proxy_address and (not ip.is_global or ip.is_multicast):
            continue
        try:
            return socket.create_connection((str(ip), port), timeout=5)
        except OSError as error:
            failure = error
    raise failure


def forward_browser_channel(channel, destination, local, root):
    host, port = destination
    try:
        with connect_browser_destination(host, port, local) as upstream:
            with (root / "browser-forwarding.jsonl").open("a", encoding="utf-8") as trace:
                trace.write(json.dumps({"host": host, "port": port}) + "\n")
            upstream.settimeout(None)

            def upload():
                try:
                    while data := channel.recv(16384):
                        upstream.sendall(data)
                    upstream.shutdown(socket.SHUT_WR)
                except (EOFError, OSError):
                    pass

            threading.Thread(target=upload, daemon=True).start()
            while data := upstream.recv(16384):
                channel.sendall(data)
            channel.shutdown_write()
    except (EOFError, OSError):
        pass
    finally:
        channel.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--browser-port", type=int, default=0)
    parser.add_argument("--browser-tls-port", type=int, default=0)
    parser.add_argument("--browser-https-address", default='', help="Test-only public address for example.com; TLS still verifies example.com")
    parser.add_argument("--browser-web", action="store_true",
                        help="Allow forwarding to public HTTP/HTTPS destinations in the loopback preview")
    parser.add_argument("--ready", type=Path, required=True)
    parser.add_argument("--io-delay", type=float, default=0, help="Test-only delay per SFTP read/write (0–0.1 seconds)")
    args = parser.parse_args()
    if args.browser_https_address and not ipaddress.ip_address(args.browser_https_address).is_global:
        parser.error('--browser-https-address must be a public IP address')
    if not 0 <= args.io_delay <= 0.1: parser.error("io-delay must be between 0 and 0.1")
    DelayedHandle.delay = args.io_delay
    host_key = paramiko.RSAKey.generate(2048)
    transports = []
    with tempfile.TemporaryDirectory(prefix="arcnix-sftp-fixture-") as directory:
        root = Path(directory) / "files"
        root.mkdir()
        client_key = paramiko.RSAKey.generate(2048)
        client_key_path = Path(directory) / "client-key.pem"
        client_key.write_private_key_file(str(client_key_path))
        ed25519 = Ed25519PrivateKey.generate()
        ed25519_path = Path(directory) / "client-ed25519"
        ed25519_path.write_bytes(ed25519.private_bytes(
            serialization.Encoding.PEM, serialization.PrivateFormat.OpenSSH,
            serialization.NoEncryption()))
        encrypted_path = Path(directory) / "client-ed25519-encrypted"
        encrypted_path.write_bytes(ed25519.private_bytes(
            serialization.Encoding.PEM, serialization.PrivateFormat.OpenSSH,
            serialization.BestAvailableEncryption(b"arcnix-test-key-passphrase")))
        untrusted_path = Path(directory) / "untrusted-ed25519"
        untrusted_path.write_bytes(Ed25519PrivateKey.generate().private_bytes(
            serialization.Encoding.PEM, serialization.PrivateFormat.OpenSSH,
            serialization.NoEncryption()))
        accepted_keys = (client_key, paramiko.Ed25519Key.from_private_key_file(str(ed25519_path)))
        # Publish two independent host keys so tests reproduce a backend upgrade
        # changing its default algorithm without changing the server's RSA key.
        host_ed25519_path = Path(directory) / "host-ed25519"
        host_ed25519_path.write_bytes(Ed25519PrivateKey.generate().private_bytes(
            serialization.Encoding.PEM, serialization.PrivateFormat.OpenSSH,
            serialization.NoEncryption()))
        host_ed25519 = paramiko.Ed25519Key.from_private_key_file(str(host_ed25519_path))
        auth_audit = Path(directory) / "auth-audit.jsonl"
        auth_audit.touch()
        audit_lock = threading.Lock()
        def audit_auth(username, method):
            with audit_lock, auth_audit.open("a", encoding="utf-8") as output:
                output.write(json.dumps({"username": username, "method": method}) + "\n")
        rsa_fingerprint = "SHA256:" + base64.b64encode(hashlib.sha256(host_key.asbytes()).digest()).decode().rstrip("=")
        (root / "welcome.txt").write_text("Arcnix SFTP fixture\n", encoding="utf-8")
        (root / "中文 空格.txt").write_text("远程文件测试\n", encoding="utf-8")
        (root / "folder").mkdir()
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", args.port))
            listener.listen(16)
            listener.settimeout(1)
            args.ready.parent.mkdir(parents=True, exist_ok=True)
            args.ready.write_text(json.dumps({"address": "127.0.0.1:" + str(listener.getsockname()[1]), "root": str(root), "client_key": str(client_key_path), "client_key_ed25519": str(ed25519_path), "client_key_ed25519_encrypted": str(encrypted_path), "client_key_untrusted": str(untrusted_path), "host_key_rsa_fingerprint": rsa_fingerprint, "auth_audit": str(auth_audit)}), encoding="utf-8")
            print("Fixture ready on " + str(listener.getsockname()), flush=True)
            try:
                while True:
                    try:
                        peer, _ = listener.accept()
                    except socket.timeout:
                        continue
                    def serve(peer=peer):
                        transport = paramiko.Transport(peer)
                        # Paramiko has no system DH moduli on Windows; avoid its
                        # group-exchange fallback and negotiate fixed modern KEX.
                        transport.get_security_options().kex = (
                            "ecdh-sha2-nistp256", "ecdh-sha2-nistp384",
                            "diffie-hellman-group14-sha256",
                        )
                        transports.append(transport)
                        transport.add_server_key(host_key)
                        transport.add_server_key(host_ed25519)
                        transport.set_subsystem_handler("sftp", paramiko.SFTPServer, Files, root=root)
                        try:
                            authentication = Authentication(accepted_keys, transport, audit_auth)
                            authentication.fixture_root = root
                            authentication.browser_port = args.browser_port
                            authentication.browser_tls_port = args.browser_tls_port
                            authentication.browser_https_address = args.browser_https_address
                            authentication.browser_web = args.browser_web
                            authentication.browser_forwarding_denied = (root / 'browser-forwarding-denied').exists()
                            repaired = root / 'browser-forwarding-repaired.json'
                            authentication.browser_forwarding_repaired = json.loads(repaired.read_text()) if repaired.exists() else {}
                            authentication.browser_channels = {}
                            transport.start_server(server=authentication)
                            while transport.is_active():
                                if not (args.browser_port or args.browser_web):
                                    time.sleep(0.1)
                                    continue
                                channel = transport.accept(timeout=0.1)
                                if channel is not None:
                                    destination = authentication.browser_channels.pop(channel.get_id(), None)
                                    if destination is not None:
                                        target, local = destination
                                        threading.Thread(target=forward_browser_channel,
                                                         args=(channel, target, local, root),
                                                         daemon=True).start()
                        except (EOFError, paramiko.SSHException):
                            pass
                        finally:
                            transport.close()
                    threading.Thread(target=serve, daemon=True).start()
            except KeyboardInterrupt:
                pass
            finally:
                for transport in transports:
                    transport.close()
                args.ready.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
