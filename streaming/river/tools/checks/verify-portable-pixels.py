"""Run in a validation container with Pillow, against the portable host port."""
import io, json, math, os, urllib.parse, urllib.request
from PIL import Image
api = 'http://host.docker.internal:18880'
drivers = json.load(urllib.request.urlopen(api+'/api/drivers'))
assert all(d['available'] for d in drivers), drivers
asset = os.environ.get('RIVER_PORTABLE_ASSET', '94a94669d839b13292340cbbdec6b96c81475eb1e1f6158c55245a7355437290')
m = json.load(urllib.request.urlopen(api+'/api/assets/'+asset+'/manifest'))
samples = []
for reduction in [0,2,m['resolutionLevels']-1]:
    edge = 512*2**reduction
    cols, rows = math.ceil(m['width']/edge), math.ceil(m['height']/edge)
    for col,row in [(0,0),((cols-1)//2,(rows-1)//2),(cols-1,rows-1)]:
        baseline = {}
        expected = (math.ceil(min(edge,m['width']-col*edge)/2**reduction), math.ceil(min(edge,m['height']-row*edge)/2**reduction))
        for driver in ['openjpeg','jp2grok','nvjpeg2000']:
            for fmt in ['png','webp']:
                q = dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=asset,STYLE='default',FORMAT='image/'+fmt,DRIVER=driver,TILEMATRIXSET='RiverPixel-'+asset,TILEMATRIX=m['resolutionLevels']-1-reduction,TILEROW=row,TILECOL=col)
                with urllib.request.urlopen(api+'/wmts?'+urllib.parse.urlencode(q),timeout=120) as response:
                    pixels = Image.open(io.BytesIO(response.read())).convert('RGBA')
                    assert pixels.size == expected
                    if driver == 'openjpeg': baseline[fmt] = pixels.tobytes()
                    assert pixels.tobytes() == baseline[fmt], (driver,fmt,reduction,col,row)
                    samples.append(dict(driver=driver,format=fmt,reduction=reduction,col=col,row=row,traceId=response.headers.get('x-river-trace-id'),equalPixels=True))
print(json.dumps(dict(drivers=drivers,samples=samples,passed=len(samples)),indent=2))
