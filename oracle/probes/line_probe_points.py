import numpy as np, subprocess, struct, sys
W,H=320,180
f32=np.float32
def probe(clips, ys):
    cases=[(float(x),float(y),0,0) for x,y in zip(clips,ys)]
    open('/tmp/mvw7_probe/pt_cases.txt','w').write('\n'.join(' '.join(repr(float(v)) for v in c) for c in cases)+'\n')
    subprocess.run(['/tmp/mvw7_probe/lines',str(W),str(H),'/tmp/mvw7_probe/pt_cases.txt','/tmp/mvw7_probe/pt_out.bin','3'],check=True)
    data=open('/tmp/mvw7_probe/pt_out.bin','rb').read(); off=0
    res=[]
    for _ in cases:
        n=struct.unpack_from('<I',data,off)[0]; off+=4
        pts=[struct.unpack_from('<HH',data,off+4*q) for q in range(n)]; off+=4*n
        res.append(pts)
    return res
def snap_even(v): return np.round(v*256)  # numpy round half even
def pixel_from_x(xw): return np.floor(snap_even(xw)/256)
def models_x(c):
    c=f32(c); w=f32(W)
    out={}
    out['exact']=(float(c)*0.5+0.5)*W
    out['v0']=float((c*f32(0.5)+f32(0.5))*w)
    out['v1 fma']=float(f32(np.float64(c)*float(w*f32(0.5))+float(w*f32(0.5))))  # single rounding of exact fma
    out['v2']=float(c*(w*f32(0.5))+w*f32(0.5))
    out['v3']=float((c+f32(1))*(w*f32(0.5)))
    return out
# sweep around several target ties
rng=np.random.default_rng(3)
targets=[100-1/512, 150-1/512, 250-1/512, 300-1/512, 20-1/512, 63-1/512]
tot={}
for T in targets:
    base=f32((T)/W*2-1)
    cur=base
    for _ in range(30): cur=np.nextafter(cur,f32(-2))
    vals=[]
    for _ in range(61):
        vals.append(cur); cur=np.nextafter(cur,f32(2))
    res=probe(vals,[0.1]*len(vals))
    ok={}
    for x,p in zip(vals,res):
        gpu=p[0][0] if p else None
        for k,xw in models_x(x).items():
            pred=int(pixel_from_x(np.float64(xw)))
            ok.setdefault(k,[0,0])
            ok[k][0]+= (pred==gpu); ok[k][1]+=1
    print('target',T,{k:f'{a}/{b}' for k,(a,b) in ok.items()})
