#!/usr/bin/env python3
"""Decode ibisPaint brush QR codes into brush parameters (JSON).

usage: ibis_brush_qr.py image.png [...]      # decode every QR code in each image
       ibis_brush_qr.py --raw payload.bin    # one raw QR payload

Format (reversed from libibispaint.so 10.1.0, see BRUSH_QR_FORMAT.md):
  "IPBZ" | int32 BE min app version | deflate( chunk 0x01000202 = BrushParameterSubChunk )
"""
import json, struct, sys, zlib

BRUSH_CHUNK = 0x01000202
ALGORITHMS = {0: "Single", 1: "Water", 2: "Double", 3: "Color"}
JITTER_COLOR = {0: "PerPattern", 1: "PerEndPoint", 2: "PerStroke"}
FLAGS = {  # bit -> name, from the getters on the u32 at +0x58
    0: "antialias", 1: "rotation", 2: "useSaturationDirect", 3: "thinSpacing",
    4: "multiSamplingAntialias", 5: "endpointInvisible", 6: "centralBound", 7: "billboard",
    8: "billboardPerspective", 9: "spacingUpperLimit", 12: "settingBrushName",
    13: "defaultParameter", 14: "fbfBrushBlend", 15: "notComposeAtTouchUp",
    16: "scatter", 17: "angleBasedCenter", 18: "perParticle", 19: "absoluteParticleSize",
    20: "absoluteTextureSize", 21: "textureAntialiasing", 22: "textureInvert",
    23: "brushPatternColor", 24: "absoluteFrameShadowSize", 25: "selectFrameShadowColor",
    26: "billboardThickness", 27: "constantOpacity", 28: "newShapeFadeEnd",
    29: "applyTextureFrameShadow", 30: "strokeBlendAddOpacity", 31: "notDependsOnCanvasSize",
}
FLAGS2 = {0: "decreaseOpacityDirect", 1: "useEquationMinMax", 2: "thicknessPerParticle",
          3: "jitterPositionRestrictedVertically", 4: "watercolorCorrect", 5: "useCurrentColor"}


class Reader:
    def __init__(self, b, end=None):
        self.b, self.p, self.end = b, 0, len(b) if end is None else end

    def take(self, n):
        if self.p + n > self.end:
            raise EOFError(f"need {n} bytes at {self.p}, chunk ends at {self.end}")
        v = self.b[self.p:self.p + n]; self.p += n; return v

    def can(self, n): return self.end - self.p >= n
    def i16(self): return struct.unpack(">h", self.take(2))[0]
    def i32(self): return struct.unpack(">i", self.take(4))[0]
    def u32(self): return struct.unpack(">I", self.take(4))[0]
    def f32(self): return round(struct.unpack(">f", self.take(4))[0], 6)
    def opt_f32(self, default): return self.f32() if self.can(4) else default
    def opt_i32(self, default=0): return self.i32() if self.can(4) else default

    def binary(self):  # int32 length + bytes
        return self.take(self.i32()) if self.can(4) else None

    def string(self):  # u16 byte length + UTF-8 (Java writeUTF style)
        n = struct.unpack(">H", self.take(2))[0]
        return self.take(n).decode("utf-8", "replace")


def bits(v, names):
    return {n: bool(v >> b & 1) for b, n in names.items()}


def read_brush(r):
    """BrushParameterSubChunk::deserializeClassSpecifics; r is bounded to the chunk body."""
    b = {}
    b["version"] = r.i16()
    b["algorithm"] = ALGORITHMS.get(r.i16(), "Unknown")
    if b["version"] < 1:
        b["legacyPatternIds"] = [r.i32(), r.i32()]
    else:
        b["brushPatternId"], b["texturePatternId"] = r.i32(), r.i32()
    for k in ("thickness", "maxThickness", "thicknessOfStart", "thicknessOfEnd", "alphaOfStart",
              "alphaOfEnd", "initialAngle", "spacing", "patternAlpha"):
        b[k] = r.f32()
    flags = r.u32()
    b["flags"] = bits(flags, FLAGS)
    b["jitterColor"] = JITTER_COLOR.get(flags >> 10 & 3, "Unknown")
    # everything below is optional: older brushes end early and the app uses these defaults
    b["thicknessToPressure"] = r.opt_f32(-1.0)
    b["opacityToPressure"] = r.opt_f32(-1.0)
    for k in ("jitterRotation", "jitterOpacity", "jitterThickness"):
        b[k] = r.opt_f32(0.0)
    b["subBrushes"] = []
    for _ in range(r.opt_i32()):  # nested brushes (e.g. the second brush of a Double brush)
        b["subBrushes"].append(read_chunk(r))
    b["opacity"] = r.opt_f32(1.0)
    for k in ("jitterPosition", "jitterSpacing", "jitterHue", "jitterSaturation", "jitterBrightness",
              "colorMix", "wetness", "dragging", "thicknessToSpeed", "opacityToSpeed",
              "blurringToSpeed", "blurringToPressure"):
        b[k] = r.opt_f32(0.0)
    b["textureScale"] = r.opt_f32(1.0)
    for k in ("textureAngle", "textureOpacity", "textureMoving"):
        b[k] = r.opt_f32(0.0)
    b["minThickness"] = r.opt_f32(None)  # app default depends on the pattern type
    md5 = r.binary(); b["brushPatternMd5"] = md5.hex() if md5 else None
    md5 = r.binary(); b["texturePatternMd5"] = md5.hex() if md5 else None
    if b["flags"]["settingBrushName"] and r.can(2):
        b["brushName"] = r.string()
    b["field_0x110"] = r.opt_i32()  # unknown int; isNeedDrawingOrder() reads it (blend mode?)
    b["innerColor"] = r.take(4).hex() if r.can(4) else None
    b["frameShadowSize"] = r.opt_f32(0.2)
    b["particleSize"] = r.opt_f32(0.05)
    b["particleDensity"] = r.opt_f32(5.0)
    for k in ("particleDeviation", "blurringOfStart", "blurringOfMiddle", "blurringOfEnd"):
        b[k] = r.opt_f32(0.0)
    md5 = r.binary(); b["blurringTextureMd5"] = md5.hex() if md5 else None
    for k in ("blurringTextureDegree", "jitterBlurring", "frameShadowAngle", "frameShadowDistance",
              "aspect", "discreteRatio", "textureLowerLimit"):
        b[k] = r.opt_f32(0.0)
    b["flags2"] = bits(r.opt_i32(), FLAGS2)
    if r.p != r.end:
        b["_unparsedTail"] = r.b[r.p:r.end].hex()  # newer app version wrote more fields
    return b


def read_chunk(r):
    kind, length = r.u32(), r.i32()
    end = r.p + length
    if kind != BRUSH_CHUNK:
        raise ValueError(f"not a brush chunk: {kind:#x}")
    body = Reader(r.b, end); body.p = r.p
    r.p = end
    return read_brush(body)


TIPS_JSON = __import__("os").path.join(__import__("os").path.dirname(__file__), "ibis_tips.json")
try:  # built by build_tip_table.py; tips/<md5>.png holds each pattern as a coverage mask
    TIPS = json.load(open(TIPS_JSON))
except OSError:
    TIPS = {}


def resolve_patterns(b):
    for k in ("brushPatternMd5", "texturePatternMd5", "blurringTextureMd5"):
        if b.get(k):
            b[k.replace("Md5", "File")] = TIPS.get(b[k], {}).get("file")
    for sub in b["subBrushes"]:
        resolve_patterns(sub)


def decode_payload(data):
    if data[:4] != b"IPBZ":
        raise ValueError("not an ibisPaint brush QR (no IPBZ magic)")
    min_version = struct.unpack(">i", data[4:8])[0]
    body = data[8:]
    try:
        raw = zlib.decompress(body)
    except zlib.error:
        raw = zlib.decompress(body, -15)  # raw deflate, no zlib header
    brush = read_chunk(Reader(raw))
    resolve_patterns(brush)
    return {"minAppVersion": min_version, "brush": brush}


def qr_payloads(path):
    import zxingcpp  # OpenCV's reader cuts binary payloads at the first NUL byte
    from PIL import Image
    return [r.bytes for r in zxingcpp.read_barcodes(Image.open(path), formats=zxingcpp.BarcodeFormat.QRCode)]


if __name__ == "__main__":
    args = sys.argv[1:]
    if args[:1] == ["--raw"]:
        out = [decode_payload(open(p, "rb").read()) for p in args[1:]]
    else:
        out = [decode_payload(p) for path in args for p in qr_payloads(path)]
    print(json.dumps(out, indent=2, ensure_ascii=False))
