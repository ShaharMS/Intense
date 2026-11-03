"""Build tiny decoder probes from generated pixels; never read served images."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile

root = Path(sys.argv[1])
root.mkdir(parents=True, exist_ok=True)
sources = []
with tempfile.TemporaryDirectory(prefix='river-warmup-build-') as work:
    work = Path(work)
    rgb = bytes((x*17+y*31+c*83) & 255 for y in range(64) for x in range(64) for c in range(3))
    gray = b''.join(((x*911+y*353) & 65535).to_bytes(2,'big') for y in range(64) for x in range(64))
    for name, data, header, style in [('rgb8',rgb,b'P6\n64 64\n255\n',0),
            ('gray16',gray,b'P5\n64 64\n65535\n',0),('ht-rgb8',rgb,b'P6\n64 64\n255\n',64)]:
        input_path = work/(name+('.pgm' if name=='gray16' else '.ppm'))
        input_path.write_bytes(header+data)
        output = root/(name+('.jhc' if style else '.j2k'))
        command = ['grk_compress','-i',str(input_path),'-o',str(output),'-n','6','-t','64,64',
            '-H','1','-Y','0','-M',str(style),'-p','RPCL']
        if style: command += ['-Z','16384']
        subprocess.run(command,check=True)
        assert output.stat().st_size <= 64*1024
        sources.append(dict(source=output.name,width=64,height=64,reduction=2))
(root/'sources.json').write_text(json.dumps(sources),encoding='utf-8')
