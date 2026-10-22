#!/usr/bin/env python3
"""Offline hypothesis search; production GPU renders must validate any selected result.

Fit on first two strings per size, report held-out last three. Never fit geometry.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import numpy as np
from scipy.ndimage import maximum_filter

spec=importlib.util.spec_from_file_location('quality',Path(__file__).with_name('text-quality.py'))
q=importlib.util.module_from_spec(spec); spec.loader.exec_module(q)

p=argparse.ArgumentParser(); p.add_argument('directory',type=Path); a=p.parse_args()
results=[]
for scale in (1,2):
    raw=q.read(a.directory/f'coverage-{scale}x.png')
    expanded=maximum_filter(raw,size=3)
    for mode in ('light','dark'):
        m=json.loads((a.directory/f'{mode}-{scale}x.json').read_text())
        ref=q.read(a.directory/f'coretext-{mode}-{scale}x.png')
        fg=m['fg']/255; bg=m['bg']/255
        train=np.zeros(raw.shape,dtype=bool); holdout=train.copy()
        for i,row in enumerate(m['rows']):
            y=int(row['top']); last=row['glyphs'][-1]
            right=int(last['x']+last['advance']+4)
            (train if i%5<2 else holdout)[y:y+42*scale,:right]=True
        mask=((np.abs(ref-bg)>.005)|(raw>0))
        train &= mask; holdout &= mask
        candidates=[]
        for dilation in (0,.05,.1,.15,.2,.3):
            cov=raw+dilation*(expanded-raw)
            for gamma in np.arange(.4,2.01,.05):
                rendered=q.encode(q.decode(bg)+(q.decode(fg)-q.decode(bg))*cov**gamma)
                err=(rendered-ref)**2
                candidates.append({'gamma':round(float(gamma),3),'dilation':dilation,
                    'train_rmse':float(np.sqrt(err[train].mean())),
                    'holdout_rmse':float(np.sqrt(err[holdout].mean()))})
        candidates.sort(key=lambda c:c['train_rmse'])
        result={'scale':scale,'mode':mode,'best':candidates[:5],
            'gamma_only':min((c for c in candidates if c['dilation']==0),key=lambda c:c['train_rmse'])}
        results.append(result); print(json.dumps(result))
(a.directory/'sweep.json').write_text(json.dumps(results,indent=2)+'\n')
