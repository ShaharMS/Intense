"""Compare first-full elapsed time and its additive stages on the original single-layer images."""
import argparse
import json
from pathlib import Path
from statistics import mean
import matplotlib.pyplot as plt
from matplotlib.patches import Patch

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('directory', type=Path)
args = parser.parse_args()
colors = ['#167D78', '#467CA3', '#AE5577', '#A9B4BA']
cases = [('Previous planner', 'wmts-planned-codewords', 'before'),
         ('Cache-first planner', 'wmts-planned-codewords', 'after'),
         ('Requested tile only', 'wmts-sibling-cost', 'after')]
fig, axes = plt.subplots(1, 2, figsize=(10, 3.5), sharex=True, sharey=True)
for ax, driver, title in zip(axes, ['openjpeg', 'jp2grok'], ['OpenJPEG', 'Grok']):
    for i, (label, filename, build) in enumerate(cases):
        rows = json.loads((args.directory / (filename + '.json')).read_text())
        stages = []
        for r in rows:
            if (r['driver'], r['phase'], r['build']) != (driver, 'firstFull', build) or '-single' not in r['image']:
                continue
            prep = float(r['stages']['prepare'])
            decode = r['grok_ms'].get('decompress', r['decode_ms'][0])
            convert = r['grok_ms'].get('conversion', 0)
            other = r['ms'] - prep - decode - convert
            assert other >= 0
            stages.append([prep, decode, convert, other])
        left = 0
        for color, values in zip(colors, zip(*stages)):
            value = mean(values)
            ax.barh(i, value, height=.52, left=left, color=color)
            left += value
        ax.text(left+1, i, f'{left:.1f}', va='center', fontsize=9)
    ax.set_title(title, loc='left', weight='bold')
    ax.set_yticks(range(len(cases)), [c[0] for c in cases])
    ax.spines[['top','right','left']].set_visible(False)
    ax.grid(axis='x', alpha=.2)
    ax.set_axisbelow(True)
    ax.set_xlabel('Mean elapsed time (ms)')
axes[0].invert_yaxis()
axes[0].set_xlim(0, 94)
fig.suptitle('First full tile after preview · original single-layer images', weight='bold')
fig.legend([Patch(color=c) for c in colors], ['Source preparation','Decode','Grok RGBA conversion','Other elapsed time'],
           loc='lower center', ncol=4, frameon=False)
fig.subplots_adjust(left=.18,right=.98,bottom=.26,top=.75,wspace=.13)
fig.savefig(args.directory / 'wmts-preview-headroom.png', dpi=150)
