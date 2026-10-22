#!/usr/bin/env python3
"""SSIM on ink-bounded row crops, signed ink weight, and per-glyph advance-cell errors.

uv run --with numpy --with pillow --with scikit-image scripts/text-quality.py DIR
SSIM: Wang et al., IEEE TIP 2004. Gaussian sigma=1.5, population covariance, L=1.
No alignment search, rescaling, or full-page empty-background score inflation.
"""
import argparse
import json
from pathlib import Path
import numpy as np
from PIL import Image, ImageDraw
from skimage.metrics import structural_similarity

def decode(x):
    return np.where(x <= .04045, x/12.92, ((x+.055)/1.055)**2.4)

def encode(x):
    return np.where(x <= .0031308, x*12.92, 1.055*np.maximum(x,0)**(1/2.4)-.055)

def read(path):
    return np.asarray(Image.open(path).convert('RGB'),dtype=np.float64)[:,:,0]/255

def save(path, data):
    Image.fromarray(np.uint8(np.clip(data*255+.5,0,255))).save(path)

def analyze(root):
    results=[]
    for manifest in sorted(root.glob('[dl]*-?x.json')):
        m=json.loads(manifest.read_text()); name=manifest.stem
        a=read(root/f'rune-{name}.png'); b=read(root/f'coretext-{name}.png')
        fg=m['fg']/255; bg=m['bg']/255
        ink_a=(a-bg)/(fg-bg); ink_b=(b-bg)/(fg-bg)
        rows=[]; glyphs=[]
        for ri,row in enumerate(m['rows']):
            y0=int(row['top']); y1=min(a.shape[0],y0+int(42*m['scale']))
            mask=(np.abs(a[y0:y1]-bg)>.01)|(np.abs(b[y0:y1]-bg)>.01)
            yy,xx=np.where(mask)
            if not len(xx): continue
            x0=max(0,int(xx.min())-6); x1=min(a.shape[1],int(xx.max())+7)
            crop=(slice(y0,y1),slice(x0,x1))
            ssim=structural_similarity(a[crop],b[crop],data_range=1,gaussian_weights=True,
                sigma=1.5,use_sample_covariance=False)
            ia=ink_a[crop].sum(); ib=ink_b[crop].sum()
            rows.append({'size':row['size'],'text':row['text'],'ssim':ssim,
                'weight_error_pct':100*(ia/ib-1),'ink_mae':float(np.abs(ink_a[crop]-ink_b[crop]).sum()/ib)})
            for gi,g in enumerate(row['glyphs']):
                # Advance cells attribute overlapping side bearings consistently to each origin.
                l=int(g['x']); r=int(row['glyphs'][gi+1]['x']) if gi+1<len(row['glyphs']) else int(g['x']+g['advance']+3)
                ca=ink_a[y0:y1,l:r]; cb=ink_b[y0:y1,l:r]
                if cb.sum()>.1:
                    glyphs.append({'row':ri,'glyph':g['id'],'x':g['x'],'coverage_rune':float(ca.sum()),
                        'coverage_coretext':float(cb.sum()),'weight_error_pct':float(100*(ca.sum()/cb.sum()-1))})
        result={'name':name,'ssim':float(np.mean([r['ssim'] for r in rows])),
            'mean_abs_weight_error_pct':float(np.mean([abs(r['weight_error_pct']) for r in rows])),
            'signed_weight_error_pct':float(np.mean([r['weight_error_pct'] for r in rows])),
            'ink_mae':float(np.mean([r['ink_mae'] for r in rows])), 'rows':rows,'glyphs':glyphs}
        results.append(result)
        save(root/f'diff-{name}.png',np.abs(a-b)*4)
        sheet=Image.new('RGB',(a.shape[1]*2,a.shape[0]+24),'#777777')
        sheet.paste(Image.open(root/f'rune-{name}.png'),(0,24))
        sheet.paste(Image.open(root/f'coretext-{name}.png'),(a.shape[1],24))
        draw=ImageDraw.Draw(sheet); draw.text((8,5),'Rune',fill='white'); draw.text((a.shape[1]+8,5),'CoreText matched glyph positions',fill='white')
        sheet.save(root/f'compare-{name}.png')
        print(json.dumps({k:v for k,v in result.items() if k not in ('rows','glyphs')}))
    for scale in (1,2):
        path=root/f'phases-{scale}x.png'
        if not path.exists(): continue
        # White-on-black GPU sRGB output -> linear atlas coverage.
        p=decode(read(path)).reshape(128,40,-1)
        sums=p.sum(axis=(1,2)); mean=sums.mean()
        shimmer={'scale':scale,'coverage_cv_pct':float(100*sums.std()/mean),
            'coverage_peak_to_peak_pct':float(100*np.ptp(sums)/mean),
            'frame_delta_rms_pct':float(100*np.sqrt(np.mean(np.diff(sums)**2))/mean)}
        results.append({'motion':shimmer}); print(json.dumps(shimmer))
        for mode in ('light','dark'):
            m=json.loads((root/f'{mode}-{scale}x.json').read_text())
            fg=m['fg']/255; bg=m['bg']/255
            gamma=m.get('gamma',.78+.22*fg)
            pixels=encode(decode(bg)+(decode(fg)-decode(bg))*p**gamma)
            ink=(pixels-bg)/(fg-bg); mass=ink.sum(axis=(1,2))
            brightness={'scale':scale,'mode':mode,'gamma':gamma,
                'brightness_cv_pct':float(100*mass.std()/mass.mean()),
                'frame_delta_rms_pct':float(100*np.sqrt(np.mean(np.diff(mass)**2))/mass.mean())}
            results.append({'brightness':brightness}); print(json.dumps(brightness))
        strip=np.concatenate([p[i] for i in (0,8,16,24,32,40,48,56,64)],axis=0)
        save(root/f'phase-strip-{scale}x.png',encode(strip))
    (root/'metrics.json').write_text(json.dumps(results,indent=2)+'\n')

if __name__=='__main__':
    parser=argparse.ArgumentParser(); parser.add_argument('directory',type=Path)
    analyze(parser.parse_args().directory)
