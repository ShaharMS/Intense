"""Screen WebP effort settings on unchanged corpus pixels before server tuning."""
import io
import json
import math
from pathlib import Path
import time
import numpy as np
from PIL import Image

root=Path(__file__).resolve().parents[1]
reference=Image.open(root/'corpus/cache/Bretagne2.j2k').convert('RGBA')
tiles=[reference.crop((x,y,min(x+512,reference.width),min(y+512,reference.height)))
    for y in range(0,reference.height,512) for x in range(0,reference.width,512)]
results=[]
for method in (4,0,1,2):
    elapsed=[]; size=0; squared=0.0; samples=0
    for tile in tiles:
        output=io.BytesIO()
        started=time.perf_counter()
        tile.save(output,format='WEBP',quality=82,method=method)
        elapsed.append((time.perf_counter()-started)*1000)
        size+=len(output.getvalue())
        decoded=Image.open(io.BytesIO(output.getvalue())).convert('RGBA')
        actual=np.asarray(decoded); expected=np.asarray(tile)
        assert np.array_equal(actual[:,:,3],expected[:,:,3]),'alpha changed'
        delta=actual[:,:,:3].astype(np.float64)-expected[:,:,:3]
        squared+=np.square(delta).sum(); samples+=delta.size
    result=dict(method=method,quality=82,tiles=len(tiles),encodeMs=sum(elapsed),
        p95Ms=sorted(elapsed)[-2],bytes=size,psnrDb=10*math.log10(255**2/(squared/samples)),alpha='exact')
    results.append(result); print(json.dumps(result),flush=True)
(root/'reports/webp-method-screen.json').write_text(json.dumps(results,indent=2),encoding='utf-8')
