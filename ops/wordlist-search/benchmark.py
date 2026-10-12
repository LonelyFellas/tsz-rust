#!/usr/bin/env python3
"""Reproducible local-only wordlist suggestion benchmark; never runs on remote DBs.

Provision/migrate an empty tsz_wordlist_search_* database and seed its synthetic
admin with the repository's seed binary before running `seed`. The publication
JSON template must come from a disposable test fixture, never production data.
Authentication for `http` uses BENCH_TOKEN without printing it or storing it in
reports. Results contain synthetic query strings, timings, plans and host facts.
"""
import argparse
import concurrent.futures
import datetime
import http.client
import json
import math
import os
from pathlib import Path
import platform
import subprocess
import time
import urllib.parse

ROOT = Path(__file__).resolve().parents[2]


def database_url():
    value = os.environ['DATABASE_URL']
    parsed = urllib.parse.urlsplit(value)
    if parsed.hostname not in ('localhost', '127.0.0.1', '::1') or not parsed.path.startswith('/tsz_wordlist_search_'):
        raise SystemExit('Requires a local, dedicated tsz_wordlist_search_* database')
    return value


def sql(statement):
    parsed = urllib.parse.urlsplit(database_url())
    env = {**os.environ, 'PGHOST': parsed.hostname, 'PGPORT': str(parsed.port or 5432),
           'PGUSER': parsed.username or os.environ.get('USER', ''), 'PGDATABASE': parsed.path[1:]}
    if parsed.password:
        env['PGPASSWORD'] = urllib.parse.unquote(parsed.password)
    proc = subprocess.run(['psql', '-X', '-qAt', '-v', 'ON_ERROR_STOP=1'], input=statement, text=True, capture_output=True, check=True, env=env)
    return proc.stdout.strip()


def literal(value):
    return "'" + value.replace("'", "''") + "'"


def metadata():
    return {'timestamp': datetime.datetime.now(datetime.timezone.utc).isoformat(),
            'host': platform.platform(), 'cpu_count': os.cpu_count(),
            'postgres': sql('SELECT version();'),
            'counts': json.loads(sql("SELECT json_build_object('entries',(SELECT count(*) FROM lexicon.entries),'surfaces',(SELECT count(*) FROM lexicon.surface_sources),'publications',(SELECT count(*) FROM lexicon.entry_publications),'publication_bytes',(SELECT pg_total_relation_size('lexicon.entry_publications')),'surface_bytes',pg_total_relation_size('lexicon.surface_sources'))")),
            'database': urllib.parse.urlsplit(database_url()).path[1:],
            'worktree': str(ROOT),
            'head': subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
            'dirty': bool(subprocess.check_output(['git','status','--porcelain'],cwd=ROOT,text=True))}


def seed(args):
    if sql('SELECT count(*) FROM lexicon.entries') != '0':
        raise SystemExit('Seed requires an empty lexicon; no existing data is deleted')
    template = json.loads(Path(args.template).read_text())
    body = 'CREATE TEMP TABLE search_bench_template AS SELECT ' + literal(json.dumps(template)) + '::jsonb AS snapshot;\n'
    body += Path(__file__).with_name('seed.sql').read_text()
    started = time.perf_counter()
    sql('BEGIN;\n'+body+'\nCOMMIT;')
    for table in ['lexicon.entries','lexicon.entry_publications','lexicon.surface_sources']:
        sql(f'VACUUM (ANALYZE) {table};')
    result = metadata()
    result['seed_seconds'] = time.perf_counter()-started
    Path(args.output).write_text(json.dumps(result,indent=2))
    print(json.dumps(result,indent=2))


CASES = [('exact','apple'),('prefix','appl'),('short','a'),('typo','aple'),
         ('phrase_typo','look forwadr to'),('miss','zzxxqqnotpresent'),('dense_prefix','pre'),('phrase','look forward'),('literal','app%')]


def explain(args):
    plans = {}
    for kind,q in [('exact','apple'),('exact','a'),('prefix','appl'),('prefix','pre'),('prefix','look forward'),('fuzzy_word','aple'),('fuzzy','look forwadr to'),('fuzzy_word','zzxxqqnotpresent')]:
        statement=(ROOT/f'src/lexicon/search_sql/suggest_{kind}.sql').read_text()
        extra_type=',text' if kind=='prefix' else ''
        upper=q[:-1]+chr(ord(q[-1])+1)
        extra_value=','+literal(upper) if kind=='prefix' else ''
        command=f'PREPARE suggestion(text,uuid[],bigint{extra_type}) AS {statement}; EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) EXECUTE suggestion({literal(q)},ARRAY[]::uuid[],3{extra_value});'
        plans[f'{kind}:{q}']=json.loads(sql(command))
    result={'metadata':metadata(),'plans':plans}
    Path(args.output).write_text(json.dumps(result,indent=2))
    for key,plan in plans.items():print(key,plan[0]['Execution Time'],'ms')


def percentile(values,p):
    ordered=sorted(values)
    return round(ordered[max(0, math.ceil(len(ordered)*p)-1)],3)


def stats(records):
    times=[r['ms'] for r in records]
    return {'requests':len(records),'errors':sum(not r['ok'] for r in records),
            'p50_ms':percentile(times,.5),'p95_ms':percentile(times,.95),'p99_ms':percentile(times,.99),
            'max_ms':round(max(times),3),'max_response_bytes':max(r['bytes'] for r in records)}


def benchmark_http(args):
    parsed=urllib.parse.urlsplit(args.url)
    if parsed.hostname not in ('127.0.0.1','localhost','::1') or parsed.scheme!='http':raise SystemExit('Local HTTP benchmark only')
    token=os.environ['BENCH_TOKEN']
    labels=json.loads(sql("SELECT json_agg(label) FROM (SELECT snapshot#>>'{presentation,label}' AS label FROM lexicon.entry_publications ORDER BY id LIMIT 128) s"))
    jobs=CASES+[(('varied_prefix' if i%2 else 'varied_exact'),label[:5] if i%2 else label) for i,label in enumerate(labels)]
    def worker(worker_id,count,case_list):
        conn=http.client.HTTPConnection(parsed.hostname,parsed.port,timeout=15)
        records=[]
        for n in range(count):
            kind,q=case_list[(worker_id*7+n)%len(case_list)]
            started=time.perf_counter();status=0;data=b'';ok=False;failure=None
            try:
                conn.request('GET',parsed.path+'?'+urllib.parse.urlencode({'q':q,**({'exclude_entry_id':args.exclude_entry_id} if args.exclude_entry_id else {})}),headers={'Authorization':'Bearer '+token})
                response=conn.getresponse();status=response.status;data=response.read()
                payload=json.loads(data)
                items=payload.get('items',[])
                ok=status==200 and set(payload)=={'items'} and len(items)<=3 and len({i['entry_id'] for i in items})==len(items)
                if kind=='exact':ok=ok and bool(items) and items[0]['label']=='apple'
                if kind=='short':ok=ok and len(items)==1 and items[0]['label']=='a'
                if kind in ('miss','literal'):ok=ok and not items
                if kind=='typo':ok=ok and any(i['label']=='apple' for i in items)
                if kind=='phrase_typo':ok=ok and any(i['label']=='look forward to' for i in items)
                if kind in ('prefix','dense_prefix','phrase','varied_prefix','varied_exact'):ok=ok and bool(items)
            except Exception as error:
                failure=type(error).__name__;conn.close();conn=http.client.HTTPConnection(parsed.hostname,parsed.port,timeout=15)
            records.append({'case':kind,'query':q,'ms':(time.perf_counter()-started)*1000,'status':status,'bytes':len(data),'ok':ok,'failure':failure})
        conn.close();return records
    # Warm endpoint auth, pool and prepared statements, with varied query values.
    warmup=worker(0,len(jobs),jobs)
    if any(not r['ok'] for r in warmup):raise SystemExit('Warm-up had errors; check endpoint/auth/fixtures before timing')
    result={'metadata':metadata(),'endpoint':args.url,'pool_connections':10,'exclude_entry_id':args.exclude_entry_id,'phases':[]}
    for concurrency in args.concurrency:
        started=time.perf_counter()
        with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as executor:
            records=[r for batch in executor.map(lambda i:worker(i,args.per_worker,jobs),range(concurrency)) for r in batch]
        elapsed=time.perf_counter()-started
        phase={'concurrency':concurrency,'seconds':round(elapsed,3),'rps':round(len(records)/elapsed,2),**stats(records),
               'by_case':{kind:stats([r for r in records if r['case']==kind]) for kind in sorted({r['case'] for r in records})},
               'failures':[r for r in records if not r['ok']]}
        result['phases'].append(phase)
        Path(args.output).write_text(json.dumps(result,indent=2))
        print(json.dumps({k:v for k,v in phase.items() if k not in ('by_case','failures')},indent=2),flush=True)
    if any(p['errors'] for p in result['phases']):raise SystemExit('Benchmark had errors; inspect report')


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    sub=parser.add_subparsers(dest='command',required=True)
    p=sub.add_parser('seed');p.add_argument('--template',required=True);p.add_argument('--output',required=True);p.set_defaults(run=seed)
    p=sub.add_parser('explain');p.add_argument('--output',required=True);p.set_defaults(run=explain)
    p=sub.add_parser('http');p.add_argument('--url',required=True);p.add_argument('--output',required=True);p.add_argument('--exclude-entry-id');p.add_argument('--concurrency',type=int,nargs='+',default=[1,16,32]);p.add_argument('--per-worker',type=int,default=160);p.set_defaults(run=benchmark_http)
    args=parser.parse_args();args.run(args)
