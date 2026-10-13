#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""make-icon.py <slug> <out.png> — 1024 px app icon: the CadCraft mark (assets/mark.png, from the site's brand folder) in white
on the app's layer colour (Catalog::LAYERS, paper values)."""
import os, sys
from PIL import Image, ImageDraw

LAYER = {'cadcraft': '#B3122E', 'bimcraft': '#1E48C8', 'modelcraft': '#A84A00', 'markupcraft': '#9A1F86',
         'takeoffcraft': '#8A6200', 'civilcraft': '#17703A', 'detailcraft': '#5B3CC4', 'partcraft': '#00707E', 'studio': '#1D1F24'}
slug, out = sys.argv[1], sys.argv[2]
S = 1024
img = Image.new('RGBA', (S, S), (0, 0, 0, 0))
ImageDraw.Draw(img).rounded_rectangle([40, 40, S - 40, S - 40], radius=180, fill=LAYER[slug])
mark = Image.open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'assets', 'mark.png')).convert('RGBA')
mark = mark.resize((620, 620), Image.LANCZOS)
white = Image.new('RGBA', mark.size, (255, 255, 255, 255))
white.putalpha(mark.getchannel('A'))
img.alpha_composite(white, ((S - 620) // 2, (S - 620) // 2))
img.save(out)
