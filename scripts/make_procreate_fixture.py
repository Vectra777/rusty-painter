# Writes a tiny synthetic .procreate (our own content) for tests.
import sys, io, zipfile, plistlib, struct
import numpy as np
from PIL import Image
from plistlib import UID
W, H, T = 300, 60, 256

def lzo_literals(data):
    """A valid LZO1X stream of literals only, then the end marker."""
    n = len(data); out = bytearray()
    if n <= 238:
        out.append(17 + n)
    else:
        out.append(0); r = n - 3 - 15
        while r > 255: out.append(0); r -= 255
        out.append(r)
    out += data; out += bytes([17, 0, 0])
    return bytes(out)

def shown_to_stored(img):          # orientation 1: rows kept bottom up
    return img[::-1].copy()

def chunks(uuid, shown):
    st = shown_to_stored(shown); out = {}
    for r in range(0, (H + T - 1) // T):
        for c in range(0, (W + T - 1) // T):
            tile = st[r*T:r*T+T, c*T:c*T+T]
            if tile[..., 3].any():
                out[f"{uuid}/{c}~{r}.chunk"] = lzo_literals(tile.tobytes())
    return out

def solid(x0, y0, x1, y1, rgba):
    a = np.zeros((H, W, 4), np.uint8); r, g, b, al = rgba
    a[y0:y1, x0:x1] = [r*al//255, g*al//255, b*al//255, al]   # premultiplied
    return a
top = solid(20, 10, 120, 30, (255, 0, 0, 255))          # red, top left
inside = solid(150, 35, 280, 55, (0, 0, 255, 128))    # half blue, bottom right
# The composite over white: what the thumbnail shows.
comp = np.full((H, W, 3), 255.0)
for layer in (inside, top):
    a = layer[..., 3:4] / 255.0
    comp = layer[..., :3] + comp * (1 - a)
comp_rgba = np.concatenate([comp.round().astype(np.uint8), np.full((H, W, 1), 255, np.uint8)], 2)

objs = ["$null"]
def add(o): objs.append(o); return UID(len(objs) - 1)
cls = lambda name: add({"$classname": name, "$classes": [name, "NSObject"]})
c_layer, c_group, c_arr, c_doc = cls("SilicaLayer"), cls("SilicaGroup"), cls("NSArray"), cls("SilicaDocument")
def layer(name, uuid, blend=0, opacity=1.0, hidden=False):
    return add({"$class": c_layer, "name": add(name), "UUID": add(uuid), "blend": blend,
                "opacity": opacity, "hidden": hidden, "clipped": False, "preserve": False,
                "locked": False, "mask": UID(0)})
l_inside = layer("Inside", "IN", blend=1)
group = add({"$class": c_group, "name": add("Folder"), "opacity": 1.0, "hidden": False,
             "blend": 0, "children": add({"$class": c_arr, "NS.objects": [l_inside]})})
l_top = layer("Top", "TOP")
l_comp = layer("Composite", "COMP")
root = add({"$class": c_doc, "size": add(f"{{{W}, {H}}}"), "tileSize": T, "orientation": 1,
            "flippedHorizontally": False, "flippedVertically": False, "backgroundHidden": False,
            "backgroundColor": struct.pack("<4f", 1, 1, 1, 1),
            "layers": add({"$class": c_arr, "NS.objects": [l_top, group]}),
            "composite": l_comp})
archive = {"$version": 100000, "$archiver": "NSKeyedArchiver", "$top": {"root": root}, "$objects": objs}
buf = io.BytesIO()
with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr("Document.archive", plistlib.dumps(archive, fmt=plistlib.FMT_BINARY))
    for uuid, img in (("TOP", top), ("IN", inside), ("COMP", comp_rgba)):
        for name, data in chunks(uuid, img).items(): z.writestr(name, data)
    png = io.BytesIO(); Image.fromarray(comp_rgba).resize((150, 30)).save(png, "PNG")
    z.writestr("QuickLook/Thumbnail.png", png.getvalue())
open(sys.argv[1], "wb").write(buf.getvalue())
print(len(buf.getvalue()), "bytes")
