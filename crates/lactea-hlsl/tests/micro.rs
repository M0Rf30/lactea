//! Micro shaders probing individual HLSL semantics (ours vs glslang, GPU-rendered).
//! `cargo test --test micro -- --nocapture` shows the summary; per-snippet max diffs go to
//! `target-*/tmp/diff-micro.csv`. Skipped silently when no GPU / glslang is available.
mod common;

use common::diffrun::run_diff;
use common::*;
use lactea_abi::PassKind;
use lactea_hlsl::{Options, translate};

fn mk(snippet: &str) -> Case {
    // "G:<globals>|<body>" declares globals before shader_body
    let (globals, body) = match snippet.strip_prefix("G:").and_then(|s| s.split_once('|')) {
        Some((g, b)) => (g, b),
        None => ("", snippet),
    };
    let text = format!("{globals}\nshader_body\n{{\n{body}\n}}\n");
    Case {
        path: std::path::PathBuf::from(format!(
            "{CORPUS_DIR}/micro/{}",
            body.replace('\n', " ").chars().take(70).collect::<String>()
        )),
        kind: PassKind::Warp,
        text,
    }
}

pub const SNIPPETS: &[&str] = &[
    "float2 a=(uv-0.5)*7; float2 b=float2(1.3,0.7); ret=float3(saturate(0.5+0.25*(a%b)),0.5);",
    "float a=(uv.x-0.5)*5; ret=saturate(0.5+0.2*(a%1.7)).xxx;",
    "float a=(uv.x-0.5); ret=saturate(0.5+pow(a,2)*3).xxx;",
    "float a=(uv.x-0.5); ret=saturate(0.5+pow(a,3)*3).xxx;",
    "float a=(uv.x-0.5)*2; ret=saturate(pow(abs(a),2.5)).xxx;",
    "int a=7; float x=a/2; ret=(x/4).xxx;",
    "int i=int(uv.x*7); ret=((i%3)/3.0).xxx;",
    "int i=int(uv.x*7+0.5); float f=i/2; ret=(f/4).xxx;",
    "int k=int(uv.x*8)%4; ret=(k/4.0).xxx;",
    "int2 k=(uv*8)%2; ret=float3(k.x,k.y,0.5);",
    "float3 v=float3(uv,0.3); ret=normalize(v);",
    "float x=(uv.x-0.5)*4; ret=saturate(0.5+0.2*float3(round(x),trunc(x),frac(x)));",
    "float x=(uv.x-0.5)*4; ret=saturate(0.5+0.2*float3(floor(x),ceil(x),sign(x)));",
    "float2 p=(uv-0.5)*3; ret=saturate(0.5+0.3*float3(atan2(p.y,p.x),length(p),dot(p,p)));",
    "ret=saturate(float3(smoothstep(0.2,0.8,uv.x),step(0.5,uv.y),lerp(0.1,0.9,uv.x)));",
    "float2x2 m=float2x2(0.5,0.3,-0.3,0.5); float2 v=mul((uv-0.5),m); ret=float3(v+0.5,0.5);",
    "float2x2 m=float2x2(0.5,0.3,-0.3,0.5); float2 v=mul(m,(uv-0.5)); ret=float3(v+0.5,0.5);",
    "float4 v=float4(uv,0.3,1); float3 w=mul(v,rot_f1); ret=saturate(0.5+0.3*w);",
    "float3 w=mul(rot_s2,float3(uv,0.4)); ret=saturate(0.5+0.3*w);",
    "float3 a=cross(float3(uv,0.3),float3(0.2,0.7,0.1)); ret=saturate(0.5+a);",
    "float s,c; sincos(uv.x*6.28,s,c); ret=saturate(0.5+0.4*float3(s,c,s*c));",
    "float ip; float fr=modf((uv.x-0.5)*5,ip); ret=saturate(0.5+0.1*float3(ip,fr,ip+fr));",
    "float x=(uv.x-0.5)*3; ret=float3(x>0,x<0.5,(x>-0.5)&&(x<1));",
    "float x=uv.x; ret=float3((x<0.3)?1:0, (x>0.6)?x:0.2, any(float2(x-0.5,0)));",
    "float3 a=uv.xyx; a.xy=a.yx*0.5; a.z+=0.1; ret=a;",
    "float3 a=float3(uv,0); a.zx=float2(0.7,0.2); ret=a;",
    "float x=uv.x*uv.y; x++; ret=(x/3).xxx;",
    "float s=0; for(int i=0;i<4;i++) s+=frac(uv.x*(i+1)); ret=(s/4).xxx;",
    "float s=0; int i=0; while(i<5){ s+=uv.y*i; i++; } ret=(s/10).xxx;",
    "float s=0; for(int i=0;i<8;i++){ if(i>5) break; if(i%2==0) continue; s+=uv.x; } ret=(s/3).xxx;",
    "float2 a=float2(1,2); float b=a; ret=b.xxx*0.3;",
    "float3 a=float3(0.1,0.2,0.3); float2 b=a; ret=float3(b,0.5);",
    "float4 t=tex2D(sampler_main,uv); ret=t.xyz*t.w;",
    "float4 t=tex2Dlod(sampler_main,float4(uv,0,1.5)); ret=t.xyz;",
    "float4 t=tex2Dbias(sampler_main,float4(uv,0,1)); ret=t.xyz;",
    "float4 t=tex2Dproj(sampler_main,float4(uv*2,0,2)); ret=t.xyz;",
    "ret=tex3D(sampler_noisevol_lq,float3(uv*0.7,uv.x)).xyz;",
    "ret=GetBlur2(uv)+GetBlur3(uv_orig)*0.3;",
    "ret=lum(tex2D(sampler_fc_main,uv*1.7)).xxx;",
    "G:sampler sampler_pw_noise_lq;|float3 a=float3(uv,0.5)*texsize_noise_lq.zwx; ret=tex2D(sampler_pw_noise_lq,uv*3).xyz+a;",
    "float x=(uv.x-0.5); ret=saturate(float3(rsqrt(abs(x)+0.1)*0.1,log(abs(x)+0.2)*0.2+0.5,exp2(x)*0.4));",
    "float x=(uv.x-0.5)*3; ret=saturate(0.5+0.1*float3(log10(abs(x)+1),fmod(x,0.7),abs(x)));",
    "float3 a=float3(uv,0.1); a*=a; a=-a; ret=saturate(a+1);",
    "float4x3 m=rot_f1; float4 v=float4(uv,0.5,1); ret=saturate(0.5+0.3*mul(v,m));",
    "ret=float3(rand_frame.x,rand_preset.y,q1*0.1+0.5)*uv.x;",
    "ret=float3(time*0.01,frac(time),bass+mid)*uv.xyx*0.3;",
    "float h=ddx(uv.x)*64; ret=h.xxx;",
    "float2 d=float2(ddx(uv.x*uv.y*5),ddy(uv.x*uv.y*5)); ret=float3(d*30+0.5,0.5);",
    "float a=-0.5; ret=(sqrt(a)>0).xxx + 0.2;",
    "float a=(uv.x-0.5)*2; ret=saturate(pow(a,0.5)+0.5).xxx;",
    "bool b=uv.x>0.5; ret=b?float3(1,0,0):float3(0,1,0);",
    "float2 a=uv; a=a>0.5; ret=float3(a,0.2);",
    "static float k=0.3; k+=uv.x; ret=k.xxx;",
    "float3 c=1-uv.x; ret=c*0.5;",
    "ret=uv.x>0.5 ? tex2D(sampler_main,uv).xyz : GetBlur1(uv);",
    "float f[3]={0.2,0.5,0.9}; int i=int(uv.x*2.99); ret=f[i].xxx;",
    "float2x2 m=float2x2(uv.x,0.3,-0.3,uv.y)+float2x2(0.2,uv.x,0.4,0.1); float2 v=mul(m,(uv-0.5)); ret=float3(v+0.5,0.5);",
    "float2x2 m=float2x2(uv.x,0.3,-0.3,uv.y)*2; float2 v=mul(m,(uv-0.5)); ret=float3(v+0.5,0.5);",
    "float2x2 m=-float2x2(uv.x,0.3,-0.3,uv.y); float2 v=mul(m,(uv-0.5)); ret=float3(v+0.5,0.5);",
    "float3x3 m=float3x3(uv.x,0.3,-0.3,uv.y,0.2,0.1,0.5,uv.x*uv.y,0.9); float3 v=mul(m,float3(uv,0.5)); ret=saturate(v);",
    "int a=int(uv.x*9); int b=a/2; int c=a%3; ret=float3(b/4.0,c/3.0,(a*b)/40.0);",
    "int a=int(uv.x*9)-4; int b=a/3; ret=float3(b/4.0+0.5,abs(a)/9.0,-a/9.0+0.5);",
    "int a=int(uv.y*9); float f=a*0.5/3; ret=float3(f,a/3,(a+1)/9.0);",
    "float x=uv.x*3.7; int i=(int)x; float j=(float)i; ret=float3(j/4,frac(x),x/4);",
    "float x=uv.x; ret=float3(x<0.5,x<=0.5,x!=0.5)*(x>0.2);",
    "float3 a=float3(0.2,0.4,0.8); float3 b=a.zyx*uv.x; b.xz=b.zx; ret=b;",
    "float2 a=uv; a.x=a.y=0.3*uv.x; ret=float3(a,0.2);",
    "float x=uv.x; float y=x++ + ++x; ret=(y/4).xxx;",
    "float4 c=float4(uv,uv.x*uv.y,1); ret=c.wzy*c.xxx;",
    "ret=((float3)uv.x)*0.5;",
    "float3 w=mul(float3(uv,0.4),rot_f1); ret=saturate(0.5+0.3*w);",
    "float3 w=mul(rot_vf2,float3(uv,0.4)); ret=saturate(0.5+0.3*w);",
    "float3 v=float3(0.1,0.2,0.3); float a,b; sincos(uv.x*6.28,v.x,v.y); ret=saturate(0.5+0.5*v);",
    "float2x2 m=float2x2(0.5,uv.x,-0.3,uv.y); float2 r0=m[0]; float e=m[1][0]; ret=float3(r0,e);",
    "float2x3 m=float2x3(uv.x,0.2,0.3,0.4,uv.y,0.6); float3 v=mul(float2(uv.x,0.5),m); ret=saturate(v);",
    "float3x2 m=float3x2(uv.x,0.2,0.3,0.4,uv.y,0.6); float2 v=mul(float3(uv,0.5),m); ret=float3(v,0.1);",
    "float2x2 m=transpose(float2x2(uv.x,0.2,0.3,uv.y)); ret=float3(mul(m,float2(1,0)),0.3);",
    "ret=float3(any(uv>0.5),all(uv>0.2),any(float3(0,0,uv.x>0.4)));",
    "float3 a=float3(uv,0.4); ret=lerp(a,a.zyx,uv.x);",
    "float3 a=float3(uv,0.4); ret=smoothstep(0.2,0.7,a);",
    "float3 a=float3(uv,0.4); ret=clamp(a,0.3,0.6)+min(a,0.5)+max(a,0.45)*0.1;",
    "float3 a=float3(uv,0.4); ret=step(0.5,a)+step(a,0.35)*0.5;",
    "float2 p=uv-0.5; ret=saturate(0.5+0.3*float3(atan2(p,p.yx),0));",
    "float3 a=float3(uv,0.4)+0.1; ret=pow(a,2.2)*0.5+pow(a,a.zyx)*0.2;",
    "float3 a=float3(uv-0.5,0.4)*3; ret=saturate(0.5+0.25*frac(a));",
    "int i=int(uv.x*4); ret=saturate(float3(abs(i-2)/2.0,sign(i-1)*0.5+0.5,saturate(i)*0.7));",
    "bool b=uv.x>0.4; ret=float3(-b,!b,b+b)*0.5;",
    "float4 a=float4(uv,uv.yx); float3 b=float3(a.xy,0.3); float2 c=(float2)b; ret=float3(c,a.w);",
    "float4 a=float4(uv,0.2,0.9); float3 t=float3(a.wzy); ret=t+float3(a.xx,0);",
    "float x=0; x=uv.x>0.5?(x=0.3):(x=0.7); ret=x.xxx;",
    "float a,b,c; a=b=c=uv.x*0.5; ret=float3(a,b,c);",
    "float x=uv.x; float y=(x+=0.1,x*2); ret=float3(x,y*0.5,0.2);",
    "float s=0; for(int i=0;i<3;i++){ for(int j=0;j<3;j++){ if(j==2) break; s+=uv.x*(i+1)+j*0.1; } } ret=(s/6).xxx;",
    "float s=0; int i=0; do{ s+=uv.y; i++; if(i>6) break; }while(i<4); ret=(s/4).xxx;",
    "float3 a[2]={float3(0.1,0.2,0.3),float3(uv,0.5)}; int i=int(uv.x*1.99); ret=a[i];",
    "float3 c=tex2D(sampler_main,uv)+tex2D(sampler_blur1,uv*0.5)+tex2D(sampler_blur3,uv.yx);ret=c*0.3;",
    "float4 t=tex2D(sampler_noise_hq,uv*texsize.xy*texsize_noise_hq.zw); ret=t.xyz*t.w;",
    "float4 t=tex2D(sampler_noise_lq_lite,uv*3); float4 u=tex2D(sampler_noise_mq,uv*2); ret=t.xyz*0.5+u.xyz*0.5;",
    "float3 v=tex3D(sampler_noisevol_hq,float3(uv,0.3)).xyz; ret=v;",
    "float2 d=uv-0.5; ret=saturate(float3(length(d),distance(uv,float2(0.3,0.3)),dot(normalize(d+0.01),float2(0.6,0.8))*0.5+0.5));",
    "float3 a=normalize(float3(uv-0.5,0.3)); float3 r=reflect(a,float3(0,0,1)); ret=saturate(0.5+0.5*r);",
    "float x=uv.x*8; ret=float3(sin(x),cos(x),tan(x*0.1))*0.3+0.5;",
    "float x=(uv.x-0.5)*3; ret=saturate(0.5+0.2*float3(asin(saturate(x)),acos(saturate(x)),atan(x)));",
    "float x=(uv.x-0.5)*3; ret=saturate(0.5+0.1*float3(sinh(x),cosh(x),tanh(x)));",
    "float x=uv.x*3; ret=saturate(float3(exp(-x),exp2(-x),log2(x+1)))*0.8;",
    "float x=uv.x*3; ret=saturate(float3(sqrt(x),rsqrt(x+0.5),1/(x+0.5)))*0.5;",
    "float3 a=float3(uv,0.3); a.xy*=2; a.yz-=0.1; a.xz/=1.5; ret=a;",
    "float2 a=uv; a.yx=a; ret=float3(a,0.3);",
    "float3 a=1; a.xy=uv; a.z=a.x*a.y; ret=a;",
    "static float3 g=float3(0.1,0.2,0.3); g.y+=uv.x; ret=g;",
    "float3 a=float3(uv,0.3); float3 b=a>0.3?a:0.2; ret=b;",
    "float2 a=uv; float3 b=float3(a>0.4,uv.x>0.2?1:0); ret=b;",
    "float3 a=float3(uv,0.5); float f=a; ret=f.xxx;",
    "float3 a=float3(uv,0.5); a.x=a; ret=a;",
    "float4 a=float4(uv,0.5,0.25); float2 b=a.zw*a.xy; float c=dot(a,a); ret=float3(b,c*0.3);",
    "float3 a=float3(uv,0.5); ret=a*float3(0.5,2,1)-a.yzx*0.3;",
    "float x=uv.x; ret=float3(x%0.3,(x*5)%1.5,fmod(x,0.4));",
    "float2 p=uv*5; ret=float3(frac(p),0.3)*(0.5+0.5*sin(p.x*p.y));",
    "int a=3; int b=a*2+1; int c=b-a; ret=float3(a,b,c)/8.0;",
    "int2 a=int2(uv*4); int b=a.x+a.y*4; ret=(b/16.0).xxx;",
    "float2 a=float2(uv.x,0.5); half3 h=half3(a,0.25); ret=h;",
    "float3 a=uv.xyx; float3 b=a.zyx.xzy; ret=b;",
    "float f=uv.x; f=f>0.5?f*2:f/2; f=f<0.2?0:f; ret=f.xxx;",
    "float3 c=GetMain(uv)+GetBlur1(uv)*0.5+GetPixel(uv*0.9); ret=c*0.5;",
    "float2 p=(uv-0.5)*2; ret=saturate(float3(p.x>0?p.x:-p.x, p.y>0.2&&p.x<0.3, (p.x>0)||(p.y>0)));",
];

/// Known glslang deviations from HLSL: `matrix * matrix` is component-wise in HLSL (D3D docs),
/// glslang's front end emits a matrix product. Our translation follows HLSL; these must differ.
pub const GLSLANG_DEVIATIONS: &[&str] = &[
    "float2x2 m=float2x2(0.5,0.3,-0.3,0.5)*float2x2(0.2,0.9,0.4,0.1); float2 v=mul((uv-0.5),m); ret=float3(v+0.5,0.5);",
    "float2x2 m=float2x2(uv.x,0.3,-0.3,uv.y)*float2x2(0.2,uv.x,0.4,0.1); float2 v=mul((uv-0.5),m); ret=float3(v+0.5,0.5);",
];

#[test]
fn micro_semantics_match_glslang() {
    let cases: Vec<Case> = SNIPPETS.iter().map(|s| mk(s)).collect();
    for c in &cases {
        let t = translate(c.kind, &c.text, &Options::default());
        assert!(t.is_ok(), "{}: {:?}", c.label(), t.err());
    }
    if let Some((_pass, fail, _total)) = run_diff(cases, "micro") {
        assert_eq!(
            fail, 0,
            "micro snippets differ from glslang (see target tmp diff-micro.csv)"
        );
    }
}

#[test]
fn glslang_matrix_times_matrix_deviation() {
    let cases: Vec<Case> = GLSLANG_DEVIATIONS.iter().map(|s| mk(s)).collect();
    if let Some((_pass, fail, total)) = run_diff(cases, "micro-deviations") {
        assert_eq!(
            fail, total,
            "glslang no longer deviates on matrix*matrix? revisit docs/hlsl.md"
        );
    }
}
