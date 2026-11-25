"""Run in the GPU sidecar, where Pillow/numpy are already installed."""
import io,json,math,urllib.request,urllib.parse
import numpy as np
from PIL import Image
api='http://river:8080'
assets=json.load(urllib.request.urlopen(api+'/api/assets'))
ids=['94a94669d839b13292340cbbdec6b96c81475eb1e1f6158c55245a7355437290',assets[0]['assetId']]
report=[]
for asset in ids:
 m=json.load(urllib.request.urlopen(api+'/api/assets/'+asset+'/manifest'))
 for reduction in [0,2,m['resolutionLevels']-1]:
  edge=512*2**reduction
  for position in ['origin','center','edge']:
   cols=math.ceil(m['width']/edge);rows=math.ceil(m['height']/edge)
   col=0 if position=='origin' else (cols-1)//2 if position=='center' else cols-1
   row=0 if position=='origin' else (rows-1)//2 if position=='center' else rows-1
   expected=(math.ceil(min(edge,m['width']-col*edge)/2**reduction),math.ceil(min(edge,m['height']-row*edge)/2**reduction))
   baseline=None
   format_baselines={}
   for driver in ['openjpeg','jp2grok','nvjpeg2000']:
    for fmt in ['png','webp']:
     s=dict(asset=asset,driver=driver,format=fmt,reduction=reduction,position=position,expected=expected)
     try:
      q=dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=asset,STYLE='default',FORMAT='image/'+fmt,DRIVER=driver,TILEMATRIXSET='RiverPixel-'+asset,TILEMATRIX=m['resolutionLevels']-1-reduction,TILEROW=row,TILECOL=col)
      with urllib.request.urlopen(api+'/wmts?'+urllib.parse.urlencode(q),timeout=120) as response:
       im=Image.open(io.BytesIO(response.read()));im.load();s['size']=im.size;s['ok']=im.size==expected;s['traceId']=response.headers.get('x-river-trace-id')
      a=np.asarray(im.convert('RGBA')).astype(np.int16)
      if driver=='openjpeg':format_baselines[fmt]=a
      if fmt in format_baselines:
       s['sameFormatEqualPixels']=bool(np.array_equal(a,format_baselines[fmt]))
      if driver=='openjpeg' and fmt=='png':baseline=a
      if baseline is not None and baseline.shape==a.shape:
       delta=np.abs(a-baseline);s['meanAbsoluteChannelError']=float(delta.mean());s['maxChannelError']=int(delta.max());s['equalPixels']=bool(np.array_equal(a,baseline))
     except Exception as e:s['ok']=False;s['error']=str(e)
     report.append(s)
print(json.dumps(report,indent=2))
