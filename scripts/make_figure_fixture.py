#!/usr/bin/env python3
"""Write testdata/figure-tiny.gltf: the skinned glTF the 3D figure loader
tests read (src/canvas/figure/model.rs).

Usage: python3 scripts/make_figure_fixture.py testdata/figure-tiny.gltf"""
import json, struct, base64, sys
pos = [(-0.5,1,0),(0.5,1,0),(0,2,0),(0.5,1.4,0),(1.0,1.5,0),(0.5,1.6,0)]
nrm = [(0,0,1)]*6
# skin joint order in the file: 0 = arm, 1 = hips, 2 = spine
jnt = [(1,0,0,0),(1,0,0,0),(2,0,0,0),(2,0,0,0),(0,0,0,0),(2,0,0,0)]
wts = [(1,0,0,0),(1,0,0,0),(1,0,0,0),(1,0,0,0),(1,0,0,0),(0.5,0.5,0,0)]
jnt[5] = (2,0,0,0)
idx = [0,1,2,3,4,5]
def tr(x,y,z):
    return [1,0,0,0, 0,1,0,0, 0,0,1,0, x,y,z,1]
ibm = tr(-0.5,-1.5,0) + tr(0,-1,0) + tr(0,-1.5,0)
morph = [(-0.25,0,0)] + [(0,0,0)]*5
buf = b''
views = []
def add(data):
    global buf
    off = len(buf)
    buf += data
    while len(buf) % 4: buf += b'\0'
    views.append({"buffer":0,"byteOffset":off,"byteLength":len(data)})
    return len(views)-1
v_pos = add(b''.join(struct.pack('<3f',*p) for p in pos))
v_nrm = add(b''.join(struct.pack('<3f',*p) for p in nrm))
v_jnt = add(b''.join(struct.pack('<4B',*p) for p in jnt))
v_wts = add(b''.join(struct.pack('<4f',*p) for p in wts))
v_idx = add(b''.join(struct.pack('<H',i) for i in idx))
v_ibm = add(struct.pack('<48f',*ibm))
v_mor = add(b''.join(struct.pack('<3f',*p) for p in morph))
def mm(ps):
    return [min(p[k] for p in ps) for k in range(3)], [max(p[k] for p in ps) for k in range(3)]
pmin,pmax = mm(pos); mmin,mmax = mm(morph)
doc = {
 "asset":{"version":"2.0","generator":"rusty-painter test fixture (hand-made, public domain)"},
 "scene":0,
 "scenes":[{"nodes":[1,4]}],
 "nodes":[
  {"name":"mixamorig:LeftArm","translation":[0.5,0,0]},
  {"name":"Armature","translation":[0,1,0],"children":[2]},
  {"name":"mixamorig:Hips","children":[3]},
  {"name":"mixamorig:Spine","translation":[0,0.5,0],"children":[0]},
  {"name":"Body","mesh":0,"skin":0}
 ],
 "skins":[{"joints":[0,2,3],"inverseBindMatrices":5}],
 "materials":[{"pbrMetallicRoughness":{"baseColorFactor":[0.5,0.5,0.5,1]}}],
 "meshes":[{"name":"Body","extras":{"targetNames":["Wide"]},"primitives":[{
   "attributes":{"POSITION":0,"NORMAL":1,"JOINTS_0":2,"WEIGHTS_0":3},
   "indices":4,"material":0,
   "targets":[{"POSITION":6}]}]}],
 "accessors":[
  {"bufferView":v_pos,"componentType":5126,"count":6,"type":"VEC3","min":pmin,"max":pmax},
  {"bufferView":v_nrm,"componentType":5126,"count":6,"type":"VEC3"},
  {"bufferView":v_jnt,"componentType":5121,"count":6,"type":"VEC4"},
  {"bufferView":v_wts,"componentType":5126,"count":6,"type":"VEC4"},
  {"bufferView":v_idx,"componentType":5123,"count":6,"type":"SCALAR"},
  {"bufferView":v_ibm,"componentType":5126,"count":3,"type":"MAT4"},
  {"bufferView":v_mor,"componentType":5126,"count":6,"type":"VEC3","min":mmin,"max":mmax}
 ],
 "bufferViews":views,
 "buffers":[{"byteLength":len(buf),"uri":"data:application/octet-stream;base64,"+base64.b64encode(buf).decode()}]
}
open(sys.argv[1],'w').write(json.dumps(doc, indent=1)+"\n")
