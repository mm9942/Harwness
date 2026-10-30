#!/usr/bin/env python3
"""Export the reviewed LaTeX manuscript to a readable GitHub Markdown edition.

Requires Pandoc and PyYAML. PDF creation is independent of this optional exporter.
"""
from pathlib import Path
import argparse
import re
import subprocess
import tempfile
import yaml

source = Path(__file__).resolve().parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--output', type=Path, default=source.parent / 'harwness-paper.md')
parser.add_argument('--link-prefix', default='', help='Relative prefix for the PDF and source links')
args = parser.parse_args()
destination = args.output.resolve()
with tempfile.TemporaryDirectory() as temp:
    intermediate = Path(temp) / 'paper.md'
    subprocess.run([
        'pandoc', 'harwness-paper.tex', '--from=latex',
        '--to=gfm+tex_math_dollars', '--standalone', '--citeproc',
        '--bibliography=references.bib', '--csl=numeric.csl', '--wrap=none',
        '-o', str(intermediate)
    ], cwd=source, check=True)
    raw = intermediate.read_text()

match = re.match(r'^---\n(.*?)\n---\n', raw, re.S)
if not match:
    raise ValueError('Pandoc did not emit expected manuscript metadata')
metadata = yaml.safe_load(match.group(1))
body = raw[match.end():].lstrip()
title = 'Harwness: Policy-First Composition and Durable Execution for Model-Heterogeneous Agents'
abstract = metadata['abstract'].strip()
header = (
    f'# {title}\n\n**Mia**[^1]  \n'
    '**Architecture and implementation report — Preprint v1.0 — 30 October 2026**\n\n'
    f'[Read the typeset paper]({args.link_prefix}harwness-paper.pdf) · [Editable LaTeX source]({args.link_prefix}source/harwness-paper.tex)\n\n'
    f'## Abstract\n\n{abstract}\n\n'
)

diagram = '''```mermaid
flowchart TB
    client["Client plane: CLI, TUI and protocol surfaces"]
    control["Control plane: identity, policy and admission"]
    execution["Execution plane: sessions, tools and sandbox"]
    state["State plane: jobs, checkpoints and evidence"]
    security["Optional DoD: observation and gated Warden"]
    client --> control
    control --> execution
    control --> state
    execution <--> state
    execution -. "scoped observations" .-> security
```

**Figure 1.** Conceptual separation of planes. Solid arrows indicate ordinary coordination or state flow; the dashed arrow indicates scoped observation. The drawing does not assert equivalent integration across entry surfaces or active privileged enforcement.
'''
body = re.sub(r'<figure id="fig:architecture">.*?</figure>', lambda _:diagram, body, flags=re.S)

# Preserve numeric cross references without unsupported raw-HTML anchors.
equations = {'eq:rights':'1', 'eq:attenuation':'2', 'eq:separation':'3'}
def cross_reference(match):
    target, label = match.group(1), match.group(2)
    return equations.get(target, re.sub(r'<[^>]+>', '', label))
body = re.sub(r'<a href="#([^"]+)"[^>]*>(.*?)</a>', cross_reference, body)
for key, number in equations.items():
    body = body.replace('\\label{'+key+'}', '\\tag{'+number+'}')

body = body.replace('<div id="refs" class="references csl-bib-body" entry-spacing="1">', '# References')
body = re.sub(r'^</?div[^>]*>\s*$', '', body, flags=re.M)
body = re.sub(r'<span class="nocase">(.*?)</span>', r'\1', body)
body = re.sub(r'(<https?://[^>]+>)\s+\1', r'\1', body)
body = body.replace('```\n{\n  "schema"', '```json\n{\n  "schema"')

section = subsection = 0
rendered = []
for line in body.splitlines():
    if line.startswith('# '):
        heading = line[2:]
        if heading == 'Authorship, Assistance, and Availability':
            prefix = ''
        elif heading == 'Artifact Identity and Source Map':
            prefix = 'Appendix A. '
        elif heading == 'Falsifiers and Reproducibility Template':
            prefix = 'Appendix B. '
        elif heading == 'References':
            prefix = ''
        else:
            section += 1
            subsection = 0
            prefix = f'{section}. '
        line = '## '+prefix+heading
    elif line.startswith('## '):
        subsection += 1
        line = f'### {section}.{subsection} '+line[3:]
    elif line.startswith('### '):
        line = '#### '+line[4:]
    # Trim Pandoc's very wide table-padding whitespace without changing cells.
    if line.startswith('|'):
        line = '| ' + ' | '.join(cell.strip() for cell in line.strip('|').split('|')) + ' |'
    rendered.append(line)

result = header + '\n'.join(rendered).strip()+'\n'
result = re.sub(r'\n{4,}', '\n\n\n', result)
if '[eq:' in result or '<figure' in result or '\\label{' in result:
    raise ValueError('Unresolved source markup in Markdown export')
destination.write_text(result)
print(f'Wrote {destination.name}: {len(result.encode())} bytes')
