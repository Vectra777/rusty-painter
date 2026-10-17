#!/usr/bin/env python3
"""Build ibis_tips.json + tips/<md5>.png (in the current folder): the tip/texture MD5s stored in brush QR codes -> ibisPaint images.

ibisPaint hashes a pattern as MD5 over RGBA pixels with RGB = 255 and A = the pattern's coverage.
Coverage comes from the image's alpha channel (transparent tips) or 255 - luminance (opaque grayscale
images: paper textures, stamps), sometimes stored vertically flipped. Only hashes that occur as
constants in libibispaint.so are kept, i.e. real built-in patterns. Each one is also written as an
8-bit coverage mask (white = paint) to tips/<md5>.png, ready to use as a brush tip elsewhere.

usage: build_tip_table.py base.apk libibispaint.so
(both from the ibisPaint APK; then compare ibis_tips.json with assets/ibis/patterns.tsv)
"""
import hashlib, io, json, os, sys, warnings, zipfile
from PIL import Image, ImageOps

warnings.filterwarnings("ignore")
apk, lib = zipfile.ZipFile(sys.argv[1]), open(sys.argv[2], "rb").read()
here = os.getcwd()
os.makedirs(os.path.join(here, "tips"), exist_ok=True)


def md5_of(cov):
    white = Image.new("L", cov.size, 255)
    return hashlib.md5(Image.merge("RGBA", [white, white, white, cov]).tobytes()).digest()


table = {}
for name in sorted(apk.namelist()):
    if not name.endswith((".png", ".webp", ".jpg")):
        continue
    try:
        im = Image.open(io.BytesIO(apk.read(name)))
        rgba = im.convert("RGBA")
    except Exception:
        continue
    rules = {"alpha": rgba.getchannel("A"), "inverted-luminance": ImageOps.invert(im.convert("L"))}
    for rule, cov in rules.items():
        for flipped in (False, True):
            c = cov.transpose(Image.FLIP_TOP_BOTTOM) if flipped else cov
            md5 = md5_of(c)
            if md5 in lib and md5.hex() not in table:
                file = name.rsplit("/", 1)[-1]
                kind = ("blur" if file.startswith("blurring") else "paper" if file.startswith("paper")
                        else "tip")
                table[md5.hex()] = {"file": name, "kind": kind, "coverage": rule,
                                    "flipped": flipped, "size": list(im.size)}
                cov.save(os.path.join(here, "tips", md5.hex() + ".png"))  # unflipped, as the image looks
        if any(v["file"] == name for v in table.values()):
            break

json.dump(table, open(os.path.join(here, "ibis_tips.json"), "w"), indent=1)
print(len(table), "patterns:", {k: sum(v["kind"] == k for v in table.values()) for k in ("tip", "paper", "blur")})
