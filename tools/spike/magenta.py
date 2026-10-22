import sys
from PIL import Image
im = Image.open(sys.argv[1]).convert("RGB")
w, h = im.size
px = im.load()
xs, ys, n = [], [], 0
for y in range(0, h):
    for x in range(0, w):
        if px[x, y] == (255, 0, 255):
            n += 1; xs.append(x); ys.append(y)
if n:
    print(f"{sys.argv[1]}: {n} px magenta, caixa x={min(xs)}..{max(xs)} y={min(ys)}..{max(ys)} (imagem {w}x{h})")
else:
    print(f"{sys.argv[1]}: 0 px magenta (imagem {w}x{h})")
