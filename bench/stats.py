import json,collections,sys
ev=[json.loads(l) for l in open(sys.argv[1]) if l.strip()]
r=[e for e in ev if e.get('event')=='result'][0]['result']
steps=[e['step_update'] for e in ev if 'step_update' in e and e['step_update'].get('state')=='DONE']
turns=[s for s in steps if s.get('usage')]
u=collections.Counter()
for s in turns:
    for k,v in s['usage'].items(): u[k]+=v
tools=collections.Counter(s.get('tool_name') for s in steps if s['step_type']=='tool')
print(r['conversation_id'], r['status'], 'wall', round(r['duration_seconds']), 's')
print('turns', len(turns), 'first-turn input', turns[0]['usage']['input_tokens'], dict(u))
print('cost-weighted', round(u['output_tokens']*5+u['input_tokens']+u['cache_read_tokens']*0.1))
print('tool calls', sum(v for k,v in tools.items() if k!='update_steps'), '(reports exclude update_steps)', dict(tools))
