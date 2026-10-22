#!/usr/bin/env python3
"""Publish compact comparison/phase strips and measurements; keep raw frames out of git."""
import argparse
import json
from pathlib import Path
import numpy as np
from PIL import Image, ImageDraw

p=argparse.ArgumentParser(); p.add_argument('runs',type=Path);p.add_argument('out',type=Path);a=p.parse_args()
a.out.mkdir(parents=True,exist_ok=True)
metrics={}
for font in ('inter','sf'):
    before=a.runs/'before'/font;after=a.runs/'after'/font
    # Keep only aggregates in the public report. text-quality.py regenerates the
    # per-row and per-glyph data under the ignored run directory when needed.
    metrics[font]={stage:[{k:v for k,v in result.items() if k not in ('rows','glyphs')}
        for result in json.loads((a.runs/stage/font/'metrics.json').read_text())]
        for stage in ('before','after')}
    for scale in (1,2):
        for mode in ('light','dark'):
            name=f'{mode}-{scale}x';m=json.loads((after/f'{name}.json').read_text())
            width=400*scale;row_h=42*scale;gap=8
            sheet=Image.new('RGB',(width*3+gap*2,28+5*row_h),'#808080')
            draw=ImageDraw.Draw(sheet)
            for col,(directory,stem,title) in enumerate([(before,'rune','Before'),(after,'rune','After'),(after,'coretext','CoreText')]):
                draw.text((col*(width+gap)+8,8),title,fill='white')
                image=Image.open(directory/f'{stem}-{name}.png').convert('RGB')
                for ri,row in enumerate(m['rows'][::5]):
                    y=int(row['top']);sheet.paste(image.crop((0,y,width,y+row_h)),(col*(width+gap),28+ri*row_h))
            sheet.save(a.out/f'{font}-{name}.png')
            ref=np.array(Image.open(after/f'coretext-{name}.png').convert('RGB'),dtype=np.int16)
            diffs=Image.new('RGB',(width*2+gap,28+5*row_h),'#808080');d=ImageDraw.Draw(diffs)
            for col,directory in enumerate((before,after)):
                d.text((col*(width+gap)+8,8),('Before' if col==0 else 'After')+' absolute error x4',fill='white')
                image=np.array(Image.open(directory/f'rune-{name}.png').convert('RGB'),dtype=np.int16)
                diff=Image.fromarray(np.uint8(np.clip(abs(image-ref)*4,0,255)))
                for ri,row in enumerate(m['rows'][::5]):
                    y=int(row['top']);diffs.paste(diff.crop((0,y,width,y+row_h)),(col*(width+gap),28+ri*row_h))
            diffs.save(a.out/f'{font}-diff-{name}.png')
        strip=Image.new('RGB',(648,28+40*9),'#808080');draw=ImageDraw.Draw(strip)
        for col,directory in enumerate((before,after)):
            draw.text((col*328+8,8),'Before' if col==0 else 'After',fill='white')
            image=Image.open(directory/f'phases-{scale}x.png').convert('RGB')
            for row,frame in enumerate((0,8,16,24,32,40,48,56,64)):
                strip.paste(image.crop((0,frame*40,320,(frame+1)*40)),(col*328,28+row*40))
        strip.save(a.out/f'{font}-phases-{scale}x.png')
(a.out/'metrics.json').write_text(json.dumps(metrics,indent=2)+'\n')
